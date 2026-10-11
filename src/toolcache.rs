//! `~/.gray/mcp-tools-cache.json`: per-server advertised tool defs from
//! the last session that actually connected that upstream. A fresh
//! sidecar answers `plugin/tools` from this file and leaves the server
//! [`State::Dormant`] — the upstream child spawns on the first `tool/call`
//! routed to it, never at boot. Identity is `sha256(name \0 raw)` (the
//! same `ServerEntry::raw` consent hashes): any config change misses and
//! the server goes back to the eager `Connecting` path, so a swapped or
//! upgraded server can never serve stale defs. Entries a live `rebuild`
//! produced are rewritten per server, so the cache self-heals drift at
//! first real use. `GRAY_MCP_LAZY=0` disables seeding entirely.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::config::ServerEntry;

/// One advertised tool: the flattened `mcp__server__tool` name, the raw
/// upstream name calls route to, and the `plugin/tools` def value.
#[derive(Debug, Clone)]
pub struct CachedTool {
    pub mcp: String,
    pub raw: String,
    pub def: Value,
}

#[derive(Debug, Default)]
pub struct ToolCache {
    /// server name → (identity, tools)
    servers: HashMap<String, (String, Vec<CachedTool>)>,
}

/// Identity input for a cache entry: `sha256(name \0 raw)`. `raw` is the
/// unexpanded config JSON — the consent hash input — so secrets resolved
/// from the environment never reach this file either.
pub fn identity(entry: &ServerEntry) -> String {
    let mut h = Sha256::new();
    h.update(entry.name.as_bytes());
    h.update([0]);
    h.update(entry.raw.as_bytes());
    format!("{:x}", h.finalize())
}

pub fn path() -> Option<PathBuf> {
    crate::gray_home().ok().map(|h| h.join("mcp-tools-cache.json"))
}

/// `GRAY_MCP_LAZY=0` (or false/off/no) keeps the old eager boot.
pub fn lazy_enabled() -> bool {
    match std::env::var("GRAY_MCP_LAZY") {
        Ok(v) => !matches!(v.as_str(), "0" | "false" | "off" | "no"),
        Err(_) => true,
    }
}

impl ToolCache {
    pub fn load(path: &Path) -> Self {
        let Ok(text) = std::fs::read_to_string(path) else {
            return Self::default();
        };
        let Ok(v) = serde_json::from_str::<Value>(&text) else {
            return Self::default();
        };
        let mut servers = HashMap::new();
        if let Some(map) = v.get("servers").and_then(Value::as_object) {
            for (name, entry) in map {
                let Some(identity) = entry.get("identity").and_then(Value::as_str) else {
                    continue;
                };
                let tools = entry
                    .get("tools")
                    .and_then(Value::as_array)
                    .map(|a| {
                        a.iter()
                            .filter_map(|t| {
                                Some(CachedTool {
                                    mcp: t.get("mcp")?.as_str()?.to_string(),
                                    raw: t.get("raw")?.as_str()?.to_string(),
                                    def: t.get("def")?.clone(),
                                })
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                servers.insert(name.clone(), (identity.to_string(), tools));
            }
        }
        Self { servers }
    }

    pub fn get(&self, server: &str, identity: &str) -> Option<&Vec<CachedTool>> {
        let (id, tools) = self.servers.get(server)?;
        (id == identity && !tools.is_empty()).then_some(tools)
    }

    /// Replace one server's entry; `None` tools removes it (a connected
    /// server exposing zero tools caches an explicit empty set so the
    /// next boot doesn't spawn it just to re-learn that).
    pub fn set(&mut self, server: &str, identity: String, tools: Vec<CachedTool>) {
        self.servers.insert(server.to_string(), (identity, tools));
    }

    pub fn save(&self, path: &Path) {
        let mut servers = serde_json::Map::new();
        for (name, (identity, tools)) in &self.servers {
            servers.insert(
                name.clone(),
                json!({
                    "identity": identity,
                    "tools": tools.iter().map(|t| json!({
                        "mcp": t.mcp, "raw": t.raw, "def": t.def,
                    })).collect::<Vec<_>>(),
                }),
            );
        }
        let out = json!({ "servers": servers });
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let _ = std::fs::write(path, out.to_string());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_and_identity_miss() {
        let dir = std::env::temp_dir().join(format!("tc-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("c.json");
        let mut c = ToolCache::default();
        c.set(
            "rea",
            "id1".into(),
            vec![CachedTool {
                mcp: "mcp__rea__x".into(),
                raw: "x".into(),
                def: json!({"name": "mcp__rea__x", "description": "d", "parameters": {}}),
            }],
        );
        c.save(&p);
        let c2 = ToolCache::load(&p);
        assert_eq!(c2.get("rea", "id1").unwrap().len(), 1);
        assert!(c2.get("rea", "other").is_none(), "identity change misses");
        assert!(c2.get("nope", "id1").is_none());
        std::fs::remove_dir_all(&dir).ok();
    }
}
