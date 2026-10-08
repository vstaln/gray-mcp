//! `gray mcp doctor` / the `mcp_doctor` tool: pre-flight checks on the
//! configured servers — report only, never edits the config.
//!
//! Static pass: stdio command on PATH, URL parses, `~`/absolute arg paths
//! exist, no hardcoded secrets in env/headers (`${VAR}` is the right shape).
//! `deep` additionally launches each stdio server and lists its tools
//! (DEEP_TIMEOUT each, sequential), flagging dangerously-named tools.

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::{Value, json};

use crate::config::{self, ServerEntry, Transport};

/// Deep-probe budget per server (connect, then tools/list).
pub const DEEP_TIMEOUT: Duration = Duration::from_secs(8);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    Ok,
    Warn,
    Fail,
}

impl Level {
    fn label(self) -> &'static str {
        match self {
            Level::Ok => "ok",
            Level::Warn => "warn",
            Level::Fail => "fail",
        }
    }
}

/// One server's verdict.
#[derive(Debug)]
pub struct Row {
    pub name: String,
    pub level: Level,
    pub notes: Vec<String>,
}

fn on_path(cmd: &str) -> bool {
    if cmd.contains('/') || cmd.starts_with('~') {
        return expand_home(cmd).is_file();
    }
    std::env::var_os("PATH").map(|p| std::env::split_paths(&p).any(|d| d.join(cmd).is_file())).unwrap_or(false)
}

fn expand_home(p: &str) -> PathBuf {
    if let Some(home) = std::env::var_os("HOME").map(PathBuf::from) {
        if p == "~" {
            return home;
        }
        if let Some(rest) = p.strip_prefix("~/") {
            return home.join(rest);
        }
    }
    PathBuf::from(p)
}

/// Does this config key name look like it carries a secret?
fn secret_key(k: &str) -> bool {
    let k = k.to_lowercase();
    ["key", "token", "secret", "password", "authorization", "auth"].iter().any(|w| k.contains(w))
}

/// The entry's raw (pre-`${VAR}`-expansion) map field, for hardcoded-secret
/// checks — `entry.raw` is canonical JSON of the on-disk object.
fn raw_map(e: &ServerEntry, field: &str) -> Vec<(String, String)> {
    serde_json::from_str::<Value>(&e.raw)
        .ok()
        .and_then(|v| v.get(field).and_then(Value::as_object).cloned())
        .map(|m| m.into_iter().filter_map(|(k, v)| v.as_str().map(|s| (k, s.to_string()))).collect())
        .unwrap_or_default()
}

/// Flag secret-looking keys whose value is a literal (no `${VAR}`).
fn check_secrets(e: &ServerEntry, field: &str, notes: &mut Vec<String>, warn: &mut bool) {
    for (k, v) in raw_map(e, field) {
        if secret_key(&k) && !v.contains("${") {
            *warn = true;
            notes.push(format!("hardcoded secret in {field}.{k} — prefer ${{VAR}} indirection"));
        }
    }
}

/// Arg looks like a filesystem path worth existence-checking.
fn pathish(arg: &str) -> bool {
    arg == "~" || arg.starts_with("~/") || (arg.starts_with('/') && !arg.contains('='))
}

/// Static checks. `Fail` when the server can't launch, `Warn` for smells.
pub fn static_check(e: &ServerEntry) -> Row {
    let mut notes = Vec::new();
    let mut fail = false;
    let mut warn = false;
    match &e.transport {
        Transport::Stdio { command, args, .. } => {
            if !on_path(command) {
                fail = true;
                notes.push(format!("command not found: {command}"));
            }
            for a in args {
                if pathish(a) && !expand_home(a).exists() {
                    warn = true;
                    notes.push(format!("arg path does not exist: {a}"));
                }
            }
            check_secrets(e, "env", &mut notes, &mut warn);
        }
        Transport::Http { url, .. } => {
            match reqwest::Url::parse(url) {
                Ok(u) if matches!(u.scheme(), "http" | "https") => {}
                _ => {
                    fail = true;
                    notes.push(format!("url does not parse as http(s): {url}"));
                }
            }
            check_secrets(e, "headers", &mut notes, &mut warn);
        }
    }
    if e.disabled {
        notes.push("disabled".into());
    }
    let level = if fail {
        Level::Fail
    } else if warn {
        Level::Warn
    } else {
        Level::Ok
    };
    Row { name: e.name.clone(), level, notes }
}

/// Tool names that hand the model shell-shaped or destructive powers.
fn suspicious_tool(name: &str) -> bool {
    let l = name.to_lowercase();
    if l.contains("write_remote") {
        return true;
    }
    l.split(|c: char| !c.is_ascii_alphanumeric()).any(|t| matches!(t, "exec" | "shell" | "eval" | "rm" | "delete"))
}

/// Launch one stdio server and list its tools, DEEP_TIMEOUT per step.
pub async fn deep_probe(e: &ServerEntry) -> (Level, Vec<String>) {
    if !matches!(e.transport, Transport::Stdio { .. }) {
        return (Level::Ok, vec!["http transport — nothing to launch".into()]);
    }
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
    let connect = crate::hub::real_connect();
    let conn = match tokio::time::timeout(DEEP_TIMEOUT, connect(e, tx)).await {
        Ok(Ok(c)) => c,
        Ok(Err(err)) => return (Level::Fail, vec![format!("connect: {err:#}")]),
        Err(_) => return (Level::Fail, vec![format!("connect timed out ({DEEP_TIMEOUT:?})")]),
    };
    let tools = tokio::time::timeout(DEEP_TIMEOUT, conn.list_tools()).await;
    conn.close().await;
    match tools {
        Ok(Ok(tools)) => {
            let flagged: Vec<String> =
                tools.iter().map(|t| t.name.to_string()).filter(|n| suspicious_tool(n)).collect();
            let mut notes = vec![format!("{} tools", tools.len())];
            let level = if flagged.is_empty() {
                Level::Ok
            } else {
                notes.push(format!("suspicious tool names: {}", flagged.join(", ")));
                Level::Warn
            };
            (level, notes)
        }
        Ok(Err(err)) => (Level::Fail, vec![format!("tools/list: {err:#}")]),
        Err(_) => (Level::Fail, vec![format!("tools/list timed out ({DEEP_TIMEOUT:?})")]),
    }
}

/// The full report: one row per server (or the one named by `only`),
/// `deep` launching each stdio server in turn.
pub async fn report(cwd: &Path, deep: bool, only: Option<&str>) -> String {
    let cfg = config::load(cwd);
    let servers: Vec<&ServerEntry> = cfg.servers.iter().filter(|s| only.is_none_or(|n| s.name == n)).collect();
    if servers.is_empty() {
        return match only {
            Some(n) => format!("no MCP server named '{n}'"),
            None => "no MCP servers configured".into(),
        };
    }
    let mut rows = Vec::new();
    for e in servers {
        let mut row = static_check(e);
        // No point launching when the command is already missing.
        if deep && row.level != Level::Fail {
            let (level, notes) = deep_probe(e).await;
            match level {
                Level::Fail => row.level = Level::Fail,
                Level::Warn if row.level == Level::Ok => row.level = Level::Warn,
                _ => {}
            }
            row.notes.extend(notes);
        }
        rows.push(row);
    }
    render(&rows, &cfg.warnings)
}

/// `name  status  notes` table, then config warnings.
pub fn render(rows: &[Row], warnings: &[String]) -> String {
    let w = rows.iter().map(|r| r.name.len()).max().unwrap_or(0);
    let mut lines = Vec::new();
    for r in rows {
        let notes = if r.notes.is_empty() { "-".into() } else { r.notes.join("; ") };
        lines.push(format!("{:<w$}  {:<4}  {}", r.name, r.level.label(), notes));
    }
    for warn in warnings {
        lines.push(format!("warning: {warn}"));
    }
    lines.join("\n")
}

/// The `mcp_doctor` definition appended to the `plugin/tools` reply.
pub fn tool_def() -> Value {
    json!({
        "name": "mcp_doctor",
        "description": "Pre-flight check of the configured MCP servers: command on PATH, URL parses, suspicious config (hardcoded secrets, missing arg paths). 'name' limits to one server; 'deep' also launches each stdio server and lists its tools (8s each). Report only — never modifies anything.",
        "parameters": {
            "type": "object",
            "properties": {
                "name": { "type": "string", "description": "Check only this server." },
                "deep": { "type": "boolean", "description": "Also launch each stdio server and list its tools." }
            }
        }
    })
}

/// `tool/call` entry point for `mcp_doctor`.
pub async fn call(args: &Value, cwd: &Path) -> Value {
    let name = args.get("name").and_then(Value::as_str);
    let deep = args.get("deep").and_then(Value::as_bool).unwrap_or(false);
    json!({ "content": report(cwd, deep, name).await })
}

#[cfg(test)]
#[path = "doctor_tests.rs"]
mod doctor_tests;
