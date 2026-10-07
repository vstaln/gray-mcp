//! `gray mcp serve`: a stdio MCP server that exposes gray itself, so other
//! MCP clients (Claude Desktop, Cursor, another gray) can drive the agent.
//!
//! Three tools: `gray_prompt` runs `gray -p <prompt> --json` and returns the
//! final answer plus the session id; `gray_sessions` lists recent sessions;
//! `gray_session_read` renders a transcript. Prompts that continue a session
//! are serialised per session id (gray locks the session file anyway; the
//! lock here gives a queue instead of a `locked` error).

use std::collections::HashMap;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, UNIX_EPOCH};

use rmcp::handler::server::tool::ToolRouter;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{
    CallToolResult, ContentBlock, Implementation, ProgressNotificationParam, ServerCapabilities, ServerConfig,
};
use rmcp::service::{RequestContext, RoleServer};
use rmcp::{ErrorData as McpError, ServerHandler, ServiceExt, tool, tool_handler, tool_router};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{Value, json};

/// A prompt that runs longer than this is killed.
const PROMPT_TIMEOUT: Duration = Duration::from_secs(600);
/// Progress notification cadence while the child runs (only when the
/// client sent a progress token).
const PROGRESS_EVERY: Duration = Duration::from_secs(10);
const DEFAULT_SESSIONS: usize = 20;
const DEFAULT_LAST: usize = 20;

#[derive(Debug, Deserialize, JsonSchema)]
pub struct PromptArgs {
    /// The task for gray, in natural language.
    pub prompt: String,
    /// Continue an existing session (from an earlier gray_prompt result or
    /// gray_sessions). Omit to start a fresh one.
    pub session_id: Option<String>,
    /// Working directory for the run; default: the server's own.
    pub cwd: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct SessionsArgs {
    /// How many sessions to list (newest first; default 20).
    pub limit: Option<usize>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ReadArgs {
    /// The session id (as printed by gray_prompt / gray_sessions).
    pub session_id: String,
    /// Only the last N messages (default 20).
    pub last: Option<usize>,
}

/// What one `gray -p --json` run produced.
#[derive(Debug, Clone, PartialEq)]
pub struct PromptOutcome {
    pub session_id: Option<String>,
    pub text: String,
    pub usage: Value,
}

/// Parse the NDJSON `gray -p --json` writes: the last `{"type":"result"}`
/// row wins, a `{"type":"error"}` row is an error (code + message, plus
/// the holder pid when a session is locked), the session id comes from any
/// row that carries one. Lines that are not JSON are ignored.
pub fn parse_ndjson(rows: &str) -> anyhow::Result<PromptOutcome> {
    let mut session_id = None;
    let mut result: Option<Value> = None;
    for line in rows.lines() {
        let Ok(row) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        if let Some(sid) = row.get("session_id").and_then(Value::as_str) {
            session_id = Some(sid.to_string());
        }
        match row.get("type").and_then(Value::as_str) {
            Some("result") => result = Some(row),
            Some("error") => {
                let code = match &row["code"] {
                    Value::String(s) => s.clone(),
                    Value::Null => "error".into(),
                    v => v.to_string(),
                };
                let message = row["message"].as_str().unwrap_or("gray failed");
                let pid = row.get("pid").and_then(Value::as_u64).map(|p| format!(" (pid {p})")).unwrap_or_default();
                anyhow::bail!("gray {code}: {message}{pid}");
            }
            _ => {}
        }
    }
    let row = result.ok_or_else(|| anyhow::anyhow!("gray produced no result row"))?;
    Ok(PromptOutcome {
        session_id,
        text: row["text"].as_str().unwrap_or("").to_string(),
        usage: row.get("usage").cloned().unwrap_or(Value::Null),
    })
}

fn first_line(path: &Path) -> Option<String> {
    let mut line = String::new();
    BufReader::new(std::fs::File::open(path).ok()?).read_line(&mut line).ok()?;
    Some(line)
}

/// `[{id, cwd, model, started_at, path}]` for the `.jsonl` files in `dir`,
/// newest mtime first, at most `limit`. Only the header row is read. A
/// missing directory is an empty list; unreadable files are skipped.
pub fn list_sessions(dir: &Path, limit: usize) -> anyhow::Result<Vec<Value>> {
    let rd = match std::fs::read_dir(dir) {
        Ok(rd) => rd,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(vec![]),
        Err(e) => return Err(anyhow::anyhow!("cannot read {}: {e}", dir.display())),
    };
    let mut files = Vec::new();
    for entry in rd.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("jsonl") {
            continue;
        }
        let Ok(meta) = entry.metadata() else { continue };
        if meta.is_file() {
            files.push((meta.modified().unwrap_or(UNIX_EPOCH), path));
        }
    }
    files.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
    let mut out = Vec::new();
    for (_, path) in files {
        if out.len() >= limit {
            break;
        }
        let Some(header) = first_line(&path) else {
            continue;
        };
        let Ok(h) = serde_json::from_str::<Value>(&header) else {
            continue;
        };
        let id =
            h["id"].as_str().map(str::to_string).or_else(|| path.file_stem().map(|s| s.to_string_lossy().into_owned()));
        out.push(json!({"id": id, "cwd": h["cwd"], "model": h["model"], "started_at": h["timestamp"], "path": path}));
    }
    Ok(out)
}

fn valid_id(id: &str) -> bool {
    !id.is_empty() && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

/// The last `last` messages of `<dir>/<id>.jsonl` as `role: text` lines
/// (text blocks joined; entries without text are skipped). The id must be
/// `[A-Za-z0-9_-]+`, so it cannot escape `dir`.
pub fn read_session(dir: &Path, id: &str, last: usize) -> anyhow::Result<String> {
    anyhow::ensure!(valid_id(id), "invalid session id {id:?}");
    let path = dir.join(format!("{id}.jsonl"));
    let text = std::fs::read_to_string(&path).map_err(|e| anyhow::anyhow!("cannot read {}: {e}", path.display()))?;
    let entries: Vec<(String, String)> = text
        .lines()
        .skip(1)
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .filter_map(|row| {
            let msg = row.get("message")?;
            let role = msg["role"].as_str()?.to_string();
            let text = msg["content"]
                .as_array()?
                .iter()
                .filter(|b| b["type"] == "text")
                .filter_map(|b| b["text"].as_str())
                .collect::<Vec<_>>()
                .join("\n");
            (!text.is_empty()).then_some((role, text))
        })
        .collect();
    let skip = entries.len().saturating_sub(last);
    Ok(entries[skip..].iter().map(|(role, text)| format!("{role}: {text}\n")).collect())
}

fn internal(e: impl std::fmt::Display) -> McpError {
    McpError::internal_error(e.to_string(), None)
}

#[derive(Clone)]
pub struct GrayServer {
    tool_router: ToolRouter<Self>,
    locks: Arc<Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>>,
    /// `$GRAY_BIN`, else `gray` on `$PATH`.
    gray_bin: String,
    sessions_dir: PathBuf,
}

impl Default for GrayServer {
    fn default() -> Self {
        Self::new()
    }
}

impl GrayServer {
    pub fn new() -> Self {
        let gray_bin = std::env::var("GRAY_BIN").ok().filter(|s| !s.is_empty()).unwrap_or_else(|| "gray".into());
        let home = crate::gray_home().unwrap_or_else(|_| PathBuf::from(".gray"));
        Self::with(gray_bin, home.join("sessions"))
    }

    pub fn with(gray_bin: String, sessions_dir: PathBuf) -> Self {
        Self { tool_router: Self::tool_router(), locks: Default::default(), gray_bin, sessions_dir }
    }

    fn lock_for(&self, key: &str) -> Arc<tokio::sync::Mutex<()>> {
        self.locks.lock().unwrap_or_else(|p| p.into_inner()).entry(key.to_string()).or_default().clone()
    }
}

#[tool_router]
impl GrayServer {
    #[tool(
        description = "Run one task through the gray coding agent (`gray -p`) and return its final answer. Pass session_id to continue an earlier conversation; the reply ends with `[session_id: …]` for that."
    )]
    async fn gray_prompt(
        &self,
        Parameters(a): Parameters<PromptArgs>,
        ctx: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        let _guard = match &a.session_id {
            Some(sid) => Some(self.lock_for(sid).lock_owned().await),
            None => None,
        };
        let mut cmd = tokio::process::Command::new(&self.gray_bin);
        cmd.args(["-p", &a.prompt, "--json"]);
        if let Some(sid) = &a.session_id {
            cmd.args(["--session", sid]);
        }
        if let Some(cwd) = &a.cwd {
            cmd.current_dir(cwd);
        }
        cmd.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped()).kill_on_drop(true);
        let child = cmd.spawn().map_err(|e| internal(format!("cannot start {}: {e}", self.gray_bin)))?;
        let token = ctx.meta.get_progress_token();
        let started = Instant::now();
        // Dropping `wait` kills the child (`kill_on_drop`), so every early
        // return below tears gray down.
        let wait = child.wait_with_output();
        tokio::pin!(wait);
        let output = loop {
            tokio::select! {
                res = &mut wait => break res.map_err(|e| internal(format!("gray failed: {e}")))?,
                _ = ctx.ct.cancelled() => return Err(internal("cancelled by client")),
                _ = tokio::time::sleep(PROGRESS_EVERY) => {
                    let secs = started.elapsed().as_secs();
                    if started.elapsed() >= PROMPT_TIMEOUT {
                        return Err(internal(format!("gray timed out after {secs}s")));
                    }
                    if let Some(tok) = token.clone() {
                        let p = ProgressNotificationParam::new(tok, secs as f64).with_message(format!("gray running for {secs}s"));
                        let _ = ctx.peer.notify_progress(p).await;
                    }
                }
            }
        };
        let stdout = String::from_utf8_lossy(&output.stdout);
        let outcome = match parse_ndjson(&stdout) {
            Ok(o) => o,
            Err(e) => {
                let stderr = String::from_utf8_lossy(&output.stderr);
                let detail = stderr.trim();
                let msg = if detail.is_empty() { e.to_string() } else { format!("{e}\n{detail}") };
                return Ok(CallToolResult::error(vec![ContentBlock::text(msg)]));
            }
        };
        let sid = outcome.session_id.clone().unwrap_or_default();
        let mut result =
            CallToolResult::success(vec![ContentBlock::text(format!("{}\n\n[session_id: {sid}]", outcome.text))]);
        result.structured_content =
            Some(json!({"session_id": outcome.session_id, "text": outcome.text, "usage": outcome.usage}));
        Ok(result)
    }

    #[tool(description = "List recent gray sessions, newest first: id, model, working directory, start time.")]
    fn gray_sessions(&self, Parameters(a): Parameters<SessionsArgs>) -> Result<CallToolResult, McpError> {
        let rows = list_sessions(&self.sessions_dir, a.limit.unwrap_or(DEFAULT_SESSIONS)).map_err(internal)?;
        let text: Vec<String> = rows
            .iter()
            .map(|r| {
                format!(
                    "{}  {}  {}",
                    r["id"].as_str().unwrap_or(""),
                    r["model"].as_str().unwrap_or(""),
                    r["cwd"].as_str().unwrap_or("")
                )
            })
            .collect();
        let text = if text.is_empty() { "no sessions".to_string() } else { text.join("\n") };
        let mut result = CallToolResult::success(vec![ContentBlock::text(text)]);
        result.structured_content = Some(json!({"sessions": rows}));
        Ok(result)
    }

    #[tool(description = "Read a gray session transcript (the last N messages, default 20) as `role: text` lines.")]
    fn gray_session_read(&self, Parameters(a): Parameters<ReadArgs>) -> Result<CallToolResult, McpError> {
        let text = read_session(&self.sessions_dir, &a.session_id, a.last.unwrap_or(DEFAULT_LAST)).map_err(internal)?;
        let text = if text.is_empty() { "(no messages)".to_string() } else { text };
        Ok(CallToolResult::success(vec![ContentBlock::text(text)]))
    }
}

#[tool_handler(router = self.tool_router)]
impl ServerHandler for GrayServer {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new(env!("CARGO_PKG_NAME"), env!("CARGO_PKG_VERSION")))
            .with_instructions(
                "Drive the gray coding agent. gray_prompt runs a task (pass session_id to continue a conversation), \
             gray_sessions lists earlier sessions, gray_session_read shows a transcript.",
            )
    }
}

/// Serve MCP over this process's stdin/stdout until the client disconnects.
pub async fn serve_stdio() -> anyhow::Result<()> {
    let service = GrayServer::new().serve(rmcp::transport::io::stdio()).await?;
    service.waiting().await?;
    Ok(())
}

#[cfg(test)]
#[path = "server_tests.rs"]
mod server_tests;
