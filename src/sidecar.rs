//! The plugin sidecar: newline-delimited JSON frames on stdin/stdout,
//! speaking gray plugin protocol 1.3 (see `~/gray/crates/gray-plugin`).
//!
//! Host → plugin: `plugin/manifest`, `plugin/tools`, `tool/call`,
//! `command/run` (requests, numeric id) and `plugin/shutdown` (notification).
//! Plugin → host: `host/ask` (request, string id; the host answers with
//! `{"id","result"}` where a failed ask is `result.error`) and the
//! `host/tools_changed` notification.

use std::collections::HashMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::anyhow;
use serde_json::{Value, json};
use tokio::sync::{mpsc, oneshot};

use crate::config::{self, Source};
use crate::consent::{self, ConsentStore};
use crate::hub::{Hub, State};

/// How long the first `plugin/tools` waits for servers to come up.
pub const FIRST_TOOLS_WAIT: Duration = Duration::from_secs(3);

/// Frame writer plus the table of our own outstanding `host/*` requests.
pub struct Io {
    out: Mutex<Box<dyn Write + Send>>,
    pending: Mutex<HashMap<String, oneshot::Sender<Value>>>,
    next_id: AtomicU64,
}

impl Io {
    pub fn new(out: Box<dyn Write + Send>) -> Self {
        Self { out: Mutex::new(out), pending: Mutex::new(HashMap::new()), next_id: AtomicU64::new(1) }
    }

    pub fn stdout() -> Self {
        Self::new(Box::new(std::io::stdout()))
    }

    fn write(&self, frame: &Value) {
        let mut out = self.out.lock().unwrap_or_else(|e| e.into_inner());
        if let Err(e) = writeln!(out, "{frame}").and_then(|_| out.flush()) {
            log::warn!("stdout write failed: {e}");
        }
    }

    pub fn reply(&self, id: &Value, result: Value) {
        self.write(&json!({"id": id, "result": result}));
    }

    pub fn error(&self, id: &Value, code: i64, msg: &str) {
        self.write(&json!({"id": id, "error": {"code": code, "message": msg}}));
    }

    pub fn notify(&self, method: &str, params: Value) {
        self.write(&json!({"method": method, "params": params}));
    }

    /// Send a `host/*` request and wait for its answer. `Err` when the host
    /// reports `result.error`/`error` or the channel closes.
    pub async fn request(&self, method: &str, params: Value) -> anyhow::Result<Value> {
        let id = format!("mcp-ask-{}", self.next_id.fetch_add(1, Ordering::Relaxed));
        let (tx, rx) = oneshot::channel();
        self.pending.lock().unwrap_or_else(|e| e.into_inner()).insert(id.clone(), tx);
        self.write(&json!({"id": id, "method": method, "params": params}));
        let frame = rx.await.map_err(|_| anyhow!("{method}: no answer (closed)"))?;
        if let Some(e) = frame.get("error") {
            return Err(anyhow!("{method}: {e}"));
        }
        let result = frame.get("result").cloned().unwrap_or(Value::Null);
        if let Some(e) = result.get("error") {
            return Err(anyhow!("{method}: {}", e.as_str().unwrap_or(&e.to_string())));
        }
        Ok(result)
    }

    /// Route a frame to a waiting `request`; `true` if it was one of ours.
    pub fn on_frame(&self, v: &Value) -> bool {
        let Some(id) = v.get("id").and_then(Value::as_str) else { return false };
        if v.get("method").is_some() {
            return false;
        }
        let tx = self.pending.lock().unwrap_or_else(|e| e.into_inner()).remove(id);
        match tx {
            Some(tx) => {
                let _ = tx.send(v.clone());
                true
            }
            None => false,
        }
    }
}

/// Answer one host request. `Some(result)` to reply, `None` when the frame
/// was a notification or the error reply was already written.
pub async fn handle(req: &Value, hub: &Arc<Hub>, io: &Io, cwd: &Path) -> Option<Value> {
    let method = req.get("method").and_then(Value::as_str)?;
    let id = req.get("id");
    let params = req.get("params").cloned().unwrap_or(Value::Null);
    match method {
        "plugin/manifest" => Some(crate::manifest()),
        "plugin/tools" => {
            let mut tools = hub.tools();
            tools.push(crate::doctor::tool_def());
            Some(json!({"tools": tools}))
        }
        "tool/call" => {
            let name = params.get("name").and_then(Value::as_str).unwrap_or("");
            let args = params.get("args").cloned().unwrap_or_else(|| json!({}));
            if name == "mcp_doctor" {
                let cwd = params
                    .get("session")
                    .and_then(|s| s.get("cwd"))
                    .and_then(Value::as_str)
                    .filter(|c| !c.is_empty())
                    .map(PathBuf::from)
                    .unwrap_or_else(|| cwd.to_path_buf());
                Some(crate::doctor::call(&args, &cwd).await)
            } else {
                Some(hub.call(name, args).await)
            }
        }
        "command/run" => {
            let argv: Vec<String> = params
                .get("argv")
                .and_then(Value::as_array)
                .map(|a| a.iter().filter_map(Value::as_str).map(str::to_string).collect())
                .unwrap_or_default();
            let cwd = params
                .get("session")
                .and_then(|s| s.get("cwd"))
                .and_then(Value::as_str)
                .filter(|c| !c.is_empty())
                .map(PathBuf::from)
                .unwrap_or_else(|| cwd.to_path_buf());
            Some(json!({"text": command(&argv, hub, &cwd).await}))
        }
        "plugin/shutdown" => None,
        _ => {
            if let Some(id) = id {
                io.error(id, -32601, &format!("unknown method {method}"));
            }
            None
        }
    }
}

fn state_line(state: &State) -> String {
    match state {
        State::Connecting => "connecting".into(),
        State::Pending => "awaiting consent (/mcp allow <name>)".into(),
        State::Ready { tools } => format!("ready ({tools} tools)"),
        State::Failed { error } => format!("failed: {error}"),
        State::Denied => "denied".into(),
        State::Disabled => "disabled".into(),
    }
}

pub fn listing(hub: &Hub) -> String {
    let rows = hub.status();
    if rows.is_empty() {
        return "no MCP servers configured (gray mcp add <name> -- <command>)".into();
    }
    let width = rows.iter().map(|(n, _)| n.len()).max().unwrap_or(0);
    let mut s: Vec<String> = rows.iter().map(|(n, st)| format!("{n:<width$}  {}", state_line(st))).collect();
    s.push("use /mcp reload | /mcp allow <name> | /mcp tools".into());
    s.join("\n")
}

/// `/mcp` in the TUI.
pub async fn command(argv: &[String], hub: &Arc<Hub>, cwd: &Path) -> String {
    match argv.first().map(String::as_str) {
        None | Some("") | Some("list") => listing(hub),
        Some("reload") => {
            let cfg = config::load(cwd);
            let mut hub_servers = cfg.servers;
            let consent = ConsentStore::default_path().map(ConsentStore::load);
            let pending = split_pending(&mut hub_servers, consent.as_ref().ok());
            hub.reload(hub_servers).await;
            for p in &pending {
                hub.mark_pending(p);
            }
            let mut out = cfg.warnings;
            out.push(listing(hub));
            out.join("\n")
        }
        Some("allow") => {
            let Some(name) = argv.get(1) else { return "usage: /mcp allow <name>".into() };
            match allow(name, hub, cwd).await {
                Ok(()) => listing(hub),
                Err(e) => format!("allow {name}: {e:#}"),
            }
        }
        Some("tools") => {
            let tools = hub.tools();
            if tools.is_empty() {
                return "no MCP tools available".into();
            }
            tools
                .iter()
                .map(|t| {
                    let d: String = t["description"].as_str().unwrap_or("").chars().take(80).collect();
                    format!("{}  {d}", t["name"].as_str().unwrap_or(""))
                })
                .collect::<Vec<_>>()
                .join("\n")
        }
        Some("doctor") => {
            let deep = argv.iter().any(|a| a == "--deep");
            let name = argv.iter().skip(1).find(|a| !a.starts_with('-'));
            crate::doctor::report(cwd, deep, name.map(String::as_str)).await
        }
        Some(other) => {
            format!("unknown subcommand {other}; use /mcp [list|reload|allow <name>|tools|doctor [--deep] [name]]")
        }
    }
}

/// Names of project servers in `servers` that still need consent.
fn split_pending(servers: &mut [config::ServerEntry], consent: Option<&ConsentStore>) -> Vec<String> {
    servers
        .iter()
        .filter(|s| !s.disabled)
        .filter_map(|s| match &s.source {
            Source::Project(dir) => {
                let key = ConsentStore::key(dir, &s.name, &s.raw);
                (!consent.is_some_and(|c| c.is_allowed(&key))).then(|| s.name.clone())
            }
            Source::User => None,
        })
        .collect()
}

async fn allow(name: &str, hub: &Arc<Hub>, cwd: &Path) -> anyhow::Result<()> {
    let cfg = config::load(cwd);
    let entry = cfg.servers.iter().find(|s| s.name == name).ok_or_else(|| anyhow!("no such server"))?;
    if let Source::Project(dir) = &entry.source {
        let mut store = ConsentStore::load(ConsentStore::default_path()?);
        store.allow(ConsentStore::key(dir, &entry.name, &entry.raw))?;
    }
    hub.start_server(name).await;
    Ok(())
}

/// Spawn the consent flow for `pending` project servers: ask the host in
/// batches, then start or deny each one.
fn spawn_consent(pending: Vec<String>, hub: Arc<Hub>, io: Arc<Io>, cwd: PathBuf) {
    tokio::spawn(async move {
        let cfg = config::load(&cwd);
        let entries: Vec<config::ServerEntry> = cfg.servers.into_iter().filter(|s| pending.contains(&s.name)).collect();
        let mut store = match ConsentStore::default_path() {
            Ok(p) => Some(ConsentStore::load(p)),
            Err(e) => {
                log::warn!("consent store unavailable: {e:#}");
                None
            }
        };
        let mut warned = false;
        for batch in entries.chunks(consent::MAX_QUESTIONS) {
            let refs: Vec<&config::ServerEntry> = batch.iter().collect();
            let names: Vec<&str> = batch.iter().map(|e| e.name.as_str()).collect();
            let decisions = match io.request("host/ask", consent::ask_params(&refs)).await {
                Ok(res) => consent::parse_ask_result(&res, &names),
                Err(e) => {
                    if !warned {
                        log::warn!("host/ask failed; denying project servers: {e:#}");
                        warned = true;
                    }
                    names.iter().map(|n| (n.to_string(), false)).collect()
                }
            };
            for (name, allowed) in decisions {
                if !allowed {
                    hub.deny(&name);
                    continue;
                }
                if let Some(entry) = batch.iter().find(|e| e.name == name)
                    && let Source::Project(dir) = &entry.source
                    && let Some(store) = store.as_mut()
                    && let Err(e) = store.allow(ConsentStore::key(dir, &entry.name, &entry.raw))
                {
                    log::warn!("could not persist consent for {name}: {e:#}");
                }
                hub.start_server(&name).await;
            }
        }
    });
}

/// Wait up to `FIRST_TOOLS_WAIT` for the first connections to settle.
async fn settle(hub: &Hub, rx: &mut tokio::sync::watch::Receiver<u64>) {
    let deadline = tokio::time::Instant::now() + FIRST_TOOLS_WAIT;
    loop {
        if !hub.status().iter().any(|(_, s)| matches!(s, State::Connecting)) {
            return;
        }
        match tokio::time::timeout_at(deadline, rx.changed()).await {
            Ok(Ok(())) => continue,
            _ => return,
        }
    }
}

/// Sidecar entry point: serve gray on stdin/stdout until shutdown or EOF.
pub async fn run() -> anyhow::Result<()> {
    let cwd = std::env::current_dir()?;
    let cfg = config::load(&cwd);
    for w in &cfg.warnings {
        log::warn!("{w}");
    }
    let consent_store = ConsentStore::default_path().map(ConsentStore::load).ok();
    let mut servers = cfg.servers.clone();
    let pending = split_pending(&mut servers, consent_store.as_ref());
    let hub = Hub::new(servers, crate::hub::real_connect());
    for p in &pending {
        hub.mark_pending(p);
    }
    hub.start().await;

    let (tx, mut frames) = mpsc::unbounded_channel::<Value>();
    std::thread::spawn(move || {
        use std::io::BufRead;
        for line in std::io::stdin().lock().lines() {
            let Ok(line) = line else { break };
            if line.trim().is_empty() {
                continue;
            }
            match serde_json::from_str::<Value>(&line) {
                Ok(v) => {
                    if tx.send(v).is_err() {
                        break;
                    }
                }
                Err(e) => log::warn!("skipping non-JSON frame: {e}"),
            }
        }
    });

    let io = Arc::new(Io::stdout());
    let mut rx_changed = hub.subscribe();
    let mut first_tools = true;
    let mut pending = Some(pending);
    loop {
        tokio::select! {
            frame = frames.recv() => {
                let Some(frame) = frame else { break };
                if io.on_frame(&frame) {
                    continue;
                }
                let method = frame.get("method").and_then(Value::as_str).unwrap_or("");
                if method == "plugin/shutdown" {
                    break;
                }
                if method == "plugin/tools" && std::mem::take(&mut first_tools) {
                    settle(&hub, &mut rx_changed).await;
                }
                let result = handle(&frame, &hub, &io, &cwd).await;
                if let (Some(result), Some(id)) = (result, frame.get("id")) {
                    io.reply(id, result);
                }
                if method == "plugin/manifest"
                    && let Some(p) = pending.take()
                    && !p.is_empty()
                {
                    spawn_consent(p, Arc::clone(&hub), Arc::clone(&io), cwd.clone());
                }
            }
            changed = rx_changed.changed() => {
                if changed.is_err() {
                    break;
                }
                io.notify("host/tools_changed", json!({}));
            }
        }
    }
    Ok(())
}

#[cfg(test)]
#[path = "sidecar_tests.rs"]
mod sidecar_tests;
