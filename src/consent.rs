//! Consent for project-scoped servers (`<cwd>/.mcp.json`): a server runs
//! only after the user allowed it once, keyed on the project directory, the
//! server name and the canonical *unexpanded* entry, so an edited entry
//! asks again and secrets never reach `consent.json`.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::config::ServerEntry;

/// `host/ask` allows at most this many questions per request.
pub const MAX_QUESTIONS: usize = 3;
pub const ALLOW: &str = "Allow";
pub const DENY: &str = "Deny";

/// Persisted set of allowed keys: `{"allowed": ["<hex>", …]}`.
#[derive(Debug, Clone)]
pub struct ConsentStore {
    path: PathBuf,
    allowed: BTreeSet<String>,
}

impl ConsentStore {
    /// Read `path`; a missing or corrupt file yields an empty store (the
    /// corrupt case is logged, never fatal).
    pub fn load(path: PathBuf) -> Self {
        let allowed = match std::fs::read_to_string(&path) {
            Ok(text) => match serde_json::from_str::<Value>(&text) {
                Ok(v) => v
                    .get("allowed")
                    .and_then(Value::as_array)
                    .map(|a| a.iter().filter_map(|k| k.as_str().map(str::to_string)).collect())
                    .unwrap_or_default(),
                Err(e) => {
                    log::warn!("{}: ignoring corrupt consent file: {e}", path.display());
                    BTreeSet::new()
                }
            },
            Err(_) => BTreeSet::new(),
        };
        Self { path, allowed }
    }

    /// `~/.gray/mcp/consent.json`.
    pub fn default_path() -> anyhow::Result<PathBuf> {
        Ok(crate::gray_home()?.join("mcp").join("consent.json"))
    }

    /// Hex SHA-256 of `dir \0 name \0 raw`.
    pub fn key(dir: &Path, name: &str, raw: &str) -> String {
        let mut h = Sha256::new();
        h.update(dir.as_os_str().as_encoded_bytes());
        h.update(b"\0");
        h.update(name.as_bytes());
        h.update(b"\0");
        h.update(raw.as_bytes());
        format!("{:x}", h.finalize())
    }

    pub fn is_allowed(&self, key: &str) -> bool {
        self.allowed.contains(key)
    }

    /// Remember `key` and write the file (creating parent directories).
    pub fn allow(&mut self, key: String) -> anyhow::Result<()> {
        self.allowed.insert(key);
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let body = serde_json::to_string_pretty(&json!({ "allowed": self.allowed }))?;
        std::fs::write(&self.path, body + "\n")?;
        Ok(())
    }
}

/// `host/ask` params for up to `MAX_QUESTIONS` project servers; extra
/// entries are dropped (the caller asks again on the next batch).
pub fn ask_params(entries: &[&ServerEntry]) -> Value {
    let questions: Vec<Value> = entries
        .iter()
        .take(MAX_QUESTIONS)
        .map(|e| {
            let where_ = match &e.source {
                crate::config::Source::Project(d) => d.join(".mcp.json").display().to_string(),
                crate::config::Source::User => "mcp.json".to_string(),
            };
            json!({
                "id": e.name,
                "header": "MCP server",
                "question": format!(
                    "{where_} wants to start MCP server '{}': {}. Allow it to run?",
                    e.name,
                    e.transport.describe()
                ),
                "options": [
                    {"label": ALLOW, "description": "start it now and remember for this project"},
                    {"label": DENY, "description": "skip it this session"}
                ]
            })
        })
        .collect();
    json!({ "questions": questions, "blocking": true })
}

/// Map a `host/ask` result (`{"answers": {"<qid>": {"answers": [label, …]}}}`)
/// to `(name, allowed)` for every name asked. Only a literal `Allow` counts.
pub fn parse_ask_result(result: &Value, names: &[&str]) -> Vec<(String, bool)> {
    let map = result.get("answers").and_then(Value::as_object);
    names
        .iter()
        .map(|n| {
            let allowed = map
                .and_then(|m| m.get(*n))
                .and_then(|e| e.get("answers"))
                .and_then(Value::as_array)
                .is_some_and(|a| a.iter().any(|v| v.as_str() == Some(ALLOW)));
            (n.to_string(), allowed)
        })
        .collect()
}

#[cfg(test)]
#[path = "consent_tests.rs"]
mod consent_tests;
