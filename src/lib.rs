//! gray-mcp: MCP client and server for the gray agent harness.
//!
//! Client side: servers from `~/.gray/mcp.json` and `<cwd>/.mcp.json`
//! become gray tools named `mcp__<server>__<tool>`, published live over
//! plugin protocol 1.3. Server side: `gray mcp serve` is a stdio MCP server
//! exposing gray itself (`gray_prompt`, `gray_sessions`, `gray_session_read`).

use std::path::PathBuf;

pub mod cli;
pub mod config;
pub mod consent;
pub mod sidecar;

/// Manifest name: the host forwards `gray mcp ...` to this binary.
pub const PLUGIN_NAME: &str = "mcp";
/// Wire protocol version (1.3: live tools + media replies; implies 1.1).
pub const PROTOCOL: &str = "1.3";
/// Slash commands claimed in the TUI.
pub const COMMANDS: &[&str] = &["/mcp"];
/// Subcommands offered for shell/TUI completion.
pub const COMPLETION: &[&str] = &["list", "add", "remove", "allow", "tools", "serve"];

/// The `plugin/manifest` reply. Tools are deliberately empty: the live set
/// arrives through `plugin/tools`.
pub fn manifest() -> serde_json::Value {
    serde_json::json!({
        "name": PLUGIN_NAME,
        "version": env!("CARGO_PKG_VERSION"),
        "protocol": PROTOCOL,
        "tools": [],
        "commands": COMMANDS,
        "completion": COMPLETION,
        "capabilities": ["host.ask"],
        "hooks": [],
    })
}

/// No arguments and a piped stdin means the host spawned us as a sidecar;
/// anything else is the `gray mcp …` CLI.
pub fn is_sidecar_invocation(args: &[String], stdin_is_tty: bool) -> bool {
    args.is_empty() && !stdin_is_tty
}

/// `~/.gray`, overridable with `GRAY_HOME` (same resolution as gray-recall).
pub fn gray_home() -> anyhow::Result<PathBuf> {
    gray_home_from(
        std::env::var_os("GRAY_HOME").map(|v| v.to_string_lossy().into_owned()),
        std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" })
            .map(|v| v.to_string_lossy().into_owned()),
    )
}

fn gray_home_from(gray_home: Option<String>, home: Option<String>) -> anyhow::Result<PathBuf> {
    gray_home
        .filter(|v| !v.trim().is_empty())
        .map(PathBuf::from)
        .or_else(|| {
            home.filter(|h| !h.is_empty())
                .map(|h| PathBuf::from(h).join(".gray"))
        })
        .ok_or_else(|| anyhow::anyhow!("cannot resolve home: set GRAY_HOME or HOME"))
}

#[cfg(test)]
mod lib_tests;
