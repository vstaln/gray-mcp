//! `gray mcp …` CLI: edit the config files, grant consent, probe servers.

use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, anyhow};
use clap::{Parser, Subcommand};
use serde_json::{Map, Value, json};

use crate::config::{self, Config, Source};
use crate::consent::ConsentStore;
use crate::hub::{Hub, State};

/// How long `gray mcp tools` waits for servers to settle.
const TOOLS_WAIT: Duration = Duration::from_secs(12);

#[derive(Parser, Debug)]
#[command(name = "gray-mcp", bin_name = "gray mcp", about = "MCP servers for gray", disable_help_subcommand = true)]
pub struct Cli {
    #[command(subcommand)]
    pub cmd: Cmd,
}

#[derive(Subcommand, Debug)]
pub enum Cmd {
    /// Show configured servers with their source and consent state.
    List {
        #[arg(long)]
        json: bool,
    },
    /// Add a server to ~/.gray/mcp.json (or ./.mcp.json with --project).
    Add {
        name: String,
        #[arg(long)]
        project: bool,
        /// Streamable HTTP endpoint (instead of a command).
        #[arg(long, conflicts_with = "command")]
        url: Option<String>,
        /// `K=V` environment variable for a stdio server.
        #[arg(long = "env", value_name = "K=V")]
        env: Vec<String>,
        /// `K=V` HTTP header for a URL server.
        #[arg(long = "header", value_name = "K=V")]
        header: Vec<String>,
        /// Per-call timeout in seconds.
        #[arg(long)]
        timeout: Option<u64>,
        /// Command and arguments (after `--`).
        #[arg(last = true)]
        command: Vec<String>,
    },
    /// Remove a server from the user (or --project) config.
    Remove {
        name: String,
        #[arg(long)]
        project: bool,
    },
    /// Grant consent for a project server in ./.mcp.json.
    Allow { name: String },
    /// Connect to every allowed server and list the tools it offers.
    Tools,
    /// Static-check configured servers before connecting (report only).
    Doctor {
        /// Only check this server.
        name: Option<String>,
        /// Also launch each stdio server and list its tools (30s each).
        #[arg(long)]
        deep: bool,
    },
    /// Serve gray itself as an MCP server on stdio.
    Serve,
}

pub async fn run(args: Vec<String>) -> anyhow::Result<()> {
    let cli = Cli::try_parse_from(std::iter::once("gray-mcp".to_string()).chain(args))?;
    let cwd = std::env::current_dir()?;
    match cli.cmd {
        Cmd::List { json } => {
            let cfg = config::load(&cwd);
            let consent = ConsentStore::load(ConsentStore::default_path()?);
            if json {
                println!("{}", serde_json::to_string_pretty(&list_json(&cfg, &consent))?);
            } else {
                println!("{}", render_list(&cfg, &consent, &cwd));
            }
        }
        Cmd::Add { name, project, url, env, header, timeout, command } => {
            let entry = build_entry(url, command, env, header, timeout)?;
            let path = target_path(project, &cwd)?;
            let doc = read_doc(&path)?;
            write_doc(&path, &edit_add(doc, &name, entry))?;
            println!("added '{name}' to {}", path.display());
        }
        Cmd::Remove { name, project } => {
            let path = target_path(project, &cwd)?;
            let doc = read_doc(&path)?;
            write_doc(&path, &edit_remove(doc, &name)?)?;
            println!("removed '{name}' from {}", path.display());
        }
        Cmd::Allow { name } => {
            let cfg = config::load(&cwd);
            let entry = cfg
                .servers
                .iter()
                .find(|s| s.name == name && matches!(s.source, Source::Project(_)))
                .ok_or_else(|| anyhow!("no project server '{name}' in {}", config::project_path(&cwd).display()))?;
            let Source::Project(dir) = &entry.source else { unreachable!() };
            let mut store = ConsentStore::load(ConsentStore::default_path()?);
            store.allow(ConsentStore::key(dir, &entry.name, &entry.raw))?;
            println!("allowed '{name}'");
        }
        Cmd::Tools => println!("{}", tools(&cwd).await),
        Cmd::Doctor { name, deep } => {
            println!("{}", crate::doctor::report(&cwd, deep, name.as_deref()).await)
        }
        Cmd::Serve => crate::server::serve_stdio().await?,
    }
    Ok(())
}

fn target_path(project: bool, cwd: &Path) -> anyhow::Result<PathBuf> {
    if project { Ok(config::project_path(cwd)) } else { config::user_path() }
}

fn read_doc(path: &Path) -> anyhow::Result<Value> {
    match std::fs::read_to_string(path) {
        Ok(text) => serde_json::from_str(&text).with_context(|| format!("{}: invalid JSON", path.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(json!({})),
        Err(e) => Err(e).with_context(|| format!("read {}", path.display())),
    }
}

fn write_doc(path: &Path, doc: &Value) -> anyhow::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let mut text = serde_json::to_string_pretty(doc)?;
    text.push('\n');
    std::fs::write(path, text).with_context(|| format!("write {}", path.display()))
}

fn kv_map(items: &[String], what: &str) -> anyhow::Result<Map<String, Value>> {
    items
        .iter()
        .map(|kv| {
            let (k, v) = kv.split_once('=').ok_or_else(|| anyhow!("{what} '{kv}' must be K=V"))?;
            Ok((k.to_string(), Value::String(v.to_string())))
        })
        .collect()
}

/// Build the `mcpServers.<name>` object from the `add` flags.
pub fn build_entry(
    url: Option<String>,
    command: Vec<String>,
    env: Vec<String>,
    header: Vec<String>,
    timeout: Option<u64>,
) -> anyhow::Result<Value> {
    let mut obj = Map::new();
    match (url, command.split_first()) {
        (Some(url), None) => {
            obj.insert("url".into(), Value::String(url));
            let headers = kv_map(&header, "--header")?;
            if !headers.is_empty() {
                obj.insert("headers".into(), Value::Object(headers));
            }
            if !env.is_empty() {
                anyhow::bail!("--env only applies to command servers");
            }
        }
        (None, Some((cmd, args))) => {
            obj.insert("command".into(), Value::String(cmd.clone()));
            if !args.is_empty() {
                obj.insert("args".into(), json!(args));
            }
            let env = kv_map(&env, "--env")?;
            if !env.is_empty() {
                obj.insert("env".into(), Value::Object(env));
            }
            if !header.is_empty() {
                anyhow::bail!("--header only applies to --url servers");
            }
        }
        (Some(_), Some(_)) => anyhow::bail!("give either --url or a command after --, not both"),
        (None, None) => anyhow::bail!("give --url <url> or a command after --"),
    }
    if let Some(t) = timeout {
        obj.insert("timeout".into(), json!(t));
    }
    Ok(Value::Object(obj))
}

/// Insert/replace `mcpServers.<name>`, keeping every other key of `doc`.
pub fn edit_add(doc: Value, name: &str, entry: Value) -> Value {
    let mut root = match doc {
        Value::Object(m) => m,
        _ => Map::new(),
    };
    let servers = root.entry("mcpServers").or_insert_with(|| json!({}));
    if !servers.is_object() {
        *servers = json!({});
    }
    servers.as_object_mut().unwrap().insert(name.to_string(), entry);
    Value::Object(root)
}

/// Remove `mcpServers.<name>`; `Err` when it is not there.
pub fn edit_remove(mut doc: Value, name: &str) -> anyhow::Result<Value> {
    let removed = doc.get_mut("mcpServers").and_then(Value::as_object_mut).and_then(|m| m.remove(name));
    if removed.is_none() {
        anyhow::bail!("no server named '{name}'");
    }
    Ok(doc)
}

fn consent_state(e: &config::ServerEntry, consent: &ConsentStore) -> &'static str {
    match &e.source {
        Source::User => "allowed",
        Source::Project(dir) => {
            if consent.is_allowed(&ConsentStore::key(dir, &e.name, &e.raw)) {
                "allowed"
            } else {
                "needs consent"
            }
        }
    }
}

fn source_label(s: &Source) -> &'static str {
    match s {
        Source::User => "user",
        Source::Project(_) => "project",
    }
}

fn list_json(cfg: &Config, consent: &ConsentStore) -> Value {
    let servers: Vec<Value> = cfg
        .servers
        .iter()
        .map(|e| {
            json!({
                "name": e.name,
                "source": source_label(&e.source),
                "transport": e.transport.describe(),
                "timeout_secs": e.timeout.as_secs(),
                "disabled": e.disabled,
                "consent": consent_state(e, consent),
            })
        })
        .collect();
    json!({"servers": servers, "warnings": cfg.warnings})
}

/// Human table: `name  source  state  transport`, then warnings.
pub fn render_list(cfg: &Config, consent: &ConsentStore, cwd: &Path) -> String {
    let mut lines = Vec::new();
    if cfg.servers.is_empty() {
        lines.push(format!(
            "no MCP servers configured (user: {}, project: {})",
            config::user_path().map(|p| p.display().to_string()).unwrap_or_else(|_| "?".into()),
            config::project_path(cwd).display()
        ));
    } else {
        let rows: Vec<[String; 4]> = cfg
            .servers
            .iter()
            .map(|e| {
                let state = if e.disabled { "disabled" } else { consent_state(e, consent) };
                [e.name.clone(), source_label(&e.source).into(), state.into(), e.transport.describe()]
            })
            .collect();
        let mut w = [0usize; 3];
        for r in &rows {
            for (i, width) in w.iter_mut().enumerate() {
                *width = (*width).max(r[i].len());
            }
        }
        for r in rows {
            lines.push(format!(
                "{:<w0$}  {:<w1$}  {:<w2$}  {}",
                r[0],
                r[1],
                r[2],
                r[3],
                w0 = w[0],
                w1 = w[1],
                w2 = w[2]
            ));
        }
    }
    for warn in &cfg.warnings {
        lines.push(format!("warning: {warn}"));
    }
    lines.join("\n")
}

/// Connect to the allowed servers and list their tools.
async fn tools(cwd: &Path) -> String {
    let cfg = config::load(cwd);
    let consent = ConsentStore::load(ConsentStore::default_path().unwrap_or_default());
    let hub = Hub::new(cfg.servers.clone(), crate::hub::real_connect());
    for e in &cfg.servers {
        if consent_state(e, &consent) != "allowed" {
            hub.mark_pending(&e.name);
        }
    }
    hub.start().await;
    let mut rx = hub.subscribe();
    let deadline = tokio::time::Instant::now() + TOOLS_WAIT;
    while hub.status().iter().any(|(_, s)| matches!(s, State::Connecting)) {
        if tokio::time::timeout_at(deadline, rx.changed()).await.is_err() {
            break;
        }
    }
    let mut out = vec![crate::sidecar::listing(&hub)];
    let tools = hub.tools();
    if !tools.is_empty() {
        out.push(String::new());
    }
    for t in tools {
        let d: String = t["description"].as_str().unwrap_or("").chars().take(80).collect();
        out.push(format!("{}  {d}", t["name"].as_str().unwrap_or("")));
    }
    out.join("\n")
}

#[cfg(test)]
#[path = "cli_tests.rs"]
mod cli_tests;
