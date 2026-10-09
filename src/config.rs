//! `mcp.json` loading: the user file (`~/.gray/mcp.json`) merged with the
//! project file (`<cwd>/.mcp.json`), project winning on a name clash.
//!
//! Shape (Claude Code compatible):
//! `{"mcpServers": {"<name>": {"command", "args", "env", "env_file", "url", "headers",
//! "timeout", "disabled"}}}`. Exactly one of `command` / `url`. `${VAR}` is
//! expanded in `command`, `args`, `env` values, `env_file`, `url` and
//! `headers` values; an unset variable invalidates the entry (skipped with a
//! warning).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::Value;

/// Default `timeout` (seconds) when the entry has none.
pub const DEFAULT_TIMEOUT_SECS: u64 = 120;
/// `timeout` is clamped to this range (seconds).
pub const TIMEOUT_RANGE: std::ops::RangeInclusive<u64> = 1..=300;

/// Where an entry came from. `Project` carries the canonical directory of
/// the `.mcp.json` so consent can be keyed on it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Source {
    User,
    Project(PathBuf),
}

/// How to reach the server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Transport {
    Stdio {
        command: String,
        args: Vec<String>,
        env: BTreeMap<String, String>,
        /// `KEY=VAL` file loaded into the spawned env; secrets live there,
        /// not in this config. Entries in `env` win over the file.
        env_file: Option<String>,
    },
    Http {
        url: String,
        headers: BTreeMap<String, String>,
    },
}

impl Transport {
    /// One-line human description (`cmd arg…` or the URL) for prompts.
    pub fn describe(&self) -> String {
        match self {
            Transport::Stdio { command, args, .. } => {
                let mut s = command.clone();
                for a in args {
                    s.push(' ');
                    s.push_str(a);
                }
                s
            }
            Transport::Http { url, .. } => url.clone(),
        }
    }
}

/// One validated server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerEntry {
    pub name: String,
    pub source: Source,
    pub transport: Transport,
    pub timeout: Duration,
    pub disabled: bool,
    /// Canonical JSON of the *unexpanded* entry object; the consent hash
    /// input, so secrets pulled from the environment never reach disk.
    pub raw: String,
}

/// Merged configuration. `load` never fails: problems become `warnings`.
#[derive(Debug, Default, Clone)]
pub struct Config {
    pub servers: Vec<ServerEntry>,
    pub warnings: Vec<String>,
}

/// `~`/`~/x` expand to `$HOME`; anything else passes through.
pub fn expand_home(p: &str) -> PathBuf {
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

/// `~/.gray/mcp.json`.
pub fn user_path() -> anyhow::Result<PathBuf> {
    Ok(crate::gray_home()?.join("mcp.json"))
}

/// `<cwd>/.mcp.json`.
pub fn project_path(cwd: &Path) -> PathBuf {
    cwd.join(".mcp.json")
}

/// Load and merge both files for `cwd`.
pub fn load(cwd: &Path) -> Config {
    let mut warnings = Vec::new();
    let read = |p: &Path, warnings: &mut Vec<String>| -> Option<String> {
        if !p.exists() {
            return None;
        }
        match std::fs::read_to_string(p) {
            Ok(s) => Some(s),
            Err(e) => {
                warnings.push(format!("{}: {e}", p.display()));
                None
            }
        }
    };
    let user = match user_path() {
        Ok(p) => read(&p, &mut warnings),
        Err(e) => {
            warnings.push(e.to_string());
            None
        }
    };
    let proj_file = project_path(cwd);
    let proj_dir = cwd.canonicalize().unwrap_or_else(|_| cwd.to_path_buf());
    let project = read(&proj_file, &mut warnings).map(|s| (s, proj_dir));
    let mut cfg = load_from(user.as_deref(), project.as_ref().map(|(s, d)| (s.as_str(), d.as_path())), &|k| {
        std::env::var(k).ok()
    });
    warnings.append(&mut cfg.warnings);
    cfg.warnings = warnings;
    cfg
}

/// `load` without the filesystem: `user` is the user file text, `project`
/// the project file text plus its canonical directory.
pub fn load_from(user: Option<&str>, project: Option<(&str, &Path)>, env: &dyn Fn(&str) -> Option<String>) -> Config {
    let mut servers: Vec<ServerEntry> = Vec::new();
    let mut warnings = Vec::new();
    if let Some(text) = user {
        let (s, mut w) = parse(text, Source::User, env);
        servers.extend(s);
        warnings.append(&mut w);
    }
    if let Some((text, dir)) = project {
        let (s, mut w) = parse(text, Source::Project(dir.to_path_buf()), env);
        for entry in s {
            servers.retain(|e| e.name != entry.name);
            servers.push(entry);
        }
        warnings.append(&mut w);
    }
    Config { servers, warnings }
}

/// Parse one file's text. Invalid entries are skipped with a warning that
/// names them; invalid JSON yields no entries and one warning.
pub fn parse(text: &str, source: Source, env: &dyn Fn(&str) -> Option<String>) -> (Vec<ServerEntry>, Vec<String>) {
    let label = match &source {
        Source::User => "mcp.json".to_string(),
        Source::Project(d) => d.join(".mcp.json").display().to_string(),
    };
    let root: Value = match serde_json::from_str(text) {
        Ok(v) => v,
        Err(e) => return (vec![], vec![format!("{label}: {e}")]),
    };
    let Some(map) = root.get("mcpServers").and_then(Value::as_object) else {
        return (vec![], vec![format!("{label}: missing \"mcpServers\" object")]);
    };
    let mut out = Vec::new();
    let mut warnings = Vec::new();
    for (name, entry) in map {
        match parse_entry(name, entry, &source, env) {
            Ok(e) => out.push(e),
            Err(msg) => warnings.push(format!("{label}: server '{name}' skipped: {msg}")),
        }
    }
    (out, warnings)
}

fn parse_entry(
    name: &str,
    entry: &Value,
    source: &Source,
    env: &dyn Fn(&str) -> Option<String>,
) -> Result<ServerEntry, String> {
    let obj = entry.as_object().ok_or("not an object")?;
    let str_field = |k: &str| -> Result<Option<String>, String> {
        match obj.get(k) {
            None | Some(Value::Null) => Ok(None),
            Some(Value::String(s)) => Ok(Some(s.clone())),
            Some(_) => Err(format!("\"{k}\" must be a string")),
        }
    };
    let str_map = |k: &str| -> Result<BTreeMap<String, String>, String> {
        match obj.get(k) {
            None | Some(Value::Null) => Ok(BTreeMap::new()),
            Some(Value::Object(m)) => m
                .iter()
                .map(|(kk, v)| match v {
                    Value::String(s) => Ok((kk.clone(), s.clone())),
                    _ => Err(format!("\"{k}.{kk}\" must be a string")),
                })
                .collect(),
            Some(_) => Err(format!("\"{k}\" must be an object of strings")),
        }
    };
    let ex = |s: String| expand(&s, env).map_err(|v| format!("environment variable {v} is not set"));
    let ex_map = |m: BTreeMap<String, String>| -> Result<BTreeMap<String, String>, String> {
        m.into_iter().map(|(k, v)| Ok((k, ex(v)?))).collect()
    };

    let command = str_field("command")?;
    let url = str_field("url")?;
    let transport = match (command, url) {
        (Some(_), Some(_)) => return Err("has both \"command\" and \"url\"".into()),
        (None, None) => return Err("needs \"command\" or \"url\"".into()),
        (Some(command), None) => {
            let args = match obj.get("args") {
                None | Some(Value::Null) => vec![],
                Some(Value::Array(a)) => a
                    .iter()
                    .map(|v| match v {
                        Value::String(s) => ex(s.clone()),
                        _ => Err("\"args\" must be strings".into()),
                    })
                    .collect::<Result<Vec<_>, _>>()?,
                Some(_) => return Err("\"args\" must be an array".into()),
            };
            Transport::Stdio {
                command: ex(command)?,
                args,
                env: ex_map(str_map("env")?)?,
                env_file: str_field("env_file")?.map(ex).transpose()?,
            }
        }
        (None, Some(url)) => Transport::Http { url: ex(url)?, headers: ex_map(str_map("headers")?)? },
    };
    let timeout = match obj.get("timeout") {
        None | Some(Value::Null) => DEFAULT_TIMEOUT_SECS,
        Some(v) => v
            .as_u64()
            .or_else(|| v.as_f64().filter(|f| *f >= 0.0).map(|f| f as u64))
            .ok_or("\"timeout\" must be a non-negative number of seconds")?
            .clamp(*TIMEOUT_RANGE.start(), *TIMEOUT_RANGE.end()),
    };
    let disabled = match obj.get("disabled") {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(_) => return Err("\"disabled\" must be a boolean".into()),
    };
    Ok(ServerEntry {
        name: name.to_string(),
        source: source.clone(),
        transport,
        timeout: Duration::from_secs(timeout),
        disabled,
        raw: canonical(entry),
    })
}

/// Canonical JSON (sorted keys: serde_json objects are BTreeMap-backed
/// without `preserve_order`).
pub fn canonical(v: &Value) -> String {
    serde_json::to_string(v).unwrap_or_default()
}

/// Replace every `${NAME}` with `env(NAME)`; `Err(NAME)` on the first unset
/// one. Anything that is not a well-formed `${NAME}` is left as is.
pub fn expand(s: &str, env: &dyn Fn(&str) -> Option<String>) -> Result<String, String> {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = rest.find("${") {
        out.push_str(&rest[..i]);
        let after = &rest[i + 2..];
        let name_len = after
            .char_indices()
            .take_while(|(j, c)| c.is_ascii_alphanumeric() || *c == '_' || (*j == 0 && c.is_ascii_alphabetic()))
            .count();
        let name = &after[..name_len];
        let valid =
            !name.is_empty() && !name.starts_with(|c: char| c.is_ascii_digit()) && after[name_len..].starts_with('}');
        if !valid {
            out.push_str("${");
            rest = after;
            continue;
        }
        match env(name) {
            Some(v) => out.push_str(&v),
            None => return Err(name.to_string()),
        }
        rest = &after[name_len + 1..];
    }
    out.push_str(rest);
    Ok(out)
}

#[cfg(test)]
#[path = "config_tests.rs"]
mod config_tests;
