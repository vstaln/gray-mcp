//! One MCP connection (stdio child process or streamable HTTP) built on rmcp.

use std::collections::BTreeMap;
use std::process::Stdio;
use std::time::Duration;

use anyhow::{Context, anyhow};
use rmcp::model::{CallToolRequestParams, CallToolResult, ClientCapabilities, ClientConfig, Implementation, Tool};
use rmcp::service::{NotificationContext, RoleClient, RunningService};
use rmcp::transport::{
    StreamableHttpClientTransport, TokioChildProcess, streamable_http_client::StreamableHttpClientTransportConfig,
};
use rmcp::{ClientHandler, ServiceExt};
use serde_json::Value;
use tokio::sync::mpsc::UnboundedSender;

use crate::config::{ServerEntry, Transport};

/// Receives `notifications/tools/list_changed` and reports the server name.
struct Handler {
    server: String,
    changed: UnboundedSender<String>,
}

impl ClientHandler for Handler {
    async fn on_tool_list_changed(&self, _ctx: NotificationContext<RoleClient>) {
        let _ = self.changed.send(self.server.clone());
    }

    fn get_info(&self) -> ClientConfig {
        ClientConfig::new(ClientCapabilities::default(), Implementation::new("gray-mcp", env!("CARGO_PKG_VERSION")))
    }
}

pub struct McpClient {
    service: RunningService<RoleClient, Handler>,
    pub server: String,
}

impl McpClient {
    /// Spawn/connect and finish the MCP handshake, all within `entry.timeout`.
    pub async fn connect(entry: &ServerEntry, changed: UnboundedSender<String>) -> anyhow::Result<Self> {
        let handler = Handler { server: entry.name.clone(), changed };
        let fut = async {
            match &entry.transport {
                Transport::Stdio { command, args, env, env_file } => {
                    let mut cmd = tokio::process::Command::new(command);
                    cmd.args(args);
                    if let Some(f) = env_file {
                        cmd.envs(load_env_file(f)?);
                    }
                    cmd.envs(env);
                    let (proc, _stderr) = TokioChildProcess::builder(cmd)
                        .stderr(Stdio::null())
                        .spawn()
                        .with_context(|| format!("spawning {command}"))?;
                    handler.serve(proc).await.map_err(|e| anyhow!("{e}"))
                }
                Transport::Http { url, headers } => {
                    let mut map = reqwest::header::HeaderMap::new();
                    for (k, v) in headers {
                        let name = reqwest::header::HeaderName::from_bytes(k.as_bytes())
                            .with_context(|| format!("invalid header name {k:?}"))?;
                        let value = reqwest::header::HeaderValue::from_str(v)
                            .with_context(|| format!("invalid value for header {k}"))?;
                        map.insert(name, value);
                    }
                    let client = reqwest::Client::builder().default_headers(map).build()?;
                    let transport = StreamableHttpClientTransport::with_client(
                        client,
                        StreamableHttpClientTransportConfig::with_uri(url.as_str()),
                    );
                    handler.serve(transport).await.map_err(|e| anyhow!("{e}"))
                }
            }
        };
        let service = tokio::time::timeout(entry.timeout, fut)
            .await
            .map_err(|_| anyhow!("handshake timed out after {}s", entry.timeout.as_secs()))??;
        Ok(Self { service, server: entry.name.clone() })
    }

    pub async fn list_tools(&self) -> anyhow::Result<Vec<Tool>> {
        self.service.list_all_tools().await.map_err(|e| anyhow!("{e}"))
    }

    pub async fn call(&self, tool: &str, args: Value, timeout: Duration) -> anyhow::Result<CallToolResult> {
        let mut params = CallToolRequestParams::new(tool.to_string());
        if let Some(obj) = args.as_object() {
            params = params.with_arguments(obj.clone());
        }
        match tokio::time::timeout(timeout, self.service.call_tool(params)).await {
            Ok(r) => r.map_err(|e| anyhow!("{e}")),
            Err(_) => Err(anyhow!("timed out after {}s", timeout.as_secs())),
        }
    }

    /// Ask the service task to stop; the child process (if any) exits with it.
    pub fn close(&self) {
        self.service.cancellation_token().cancel();
    }
}

/// `KEY=VAL` file -> env map for a spawned stdio server. Blank lines and
/// `#` comments are skipped, an optional `export ` prefix is allowed, and
/// the value keeps everything after the first `=`. `~` expands to `$HOME`.
fn load_env_file(path: &str) -> anyhow::Result<BTreeMap<String, String>> {
    let path = crate::config::expand_home(path);
    let text = std::fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
    let mut out = BTreeMap::new();
    for (i, raw) in text.lines().enumerate() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let line = line.strip_prefix("export ").unwrap_or(line);
        let Some((k, v)) = line.split_once('=') else {
            return Err(anyhow!("{}:{}: expected KEY=VAL", path.display(), i + 1));
        };
        out.insert(k.trim().to_string(), v.trim().to_string());
    }
    Ok(out)
}
