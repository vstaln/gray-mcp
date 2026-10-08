use serde_json::json;

use super::*;
use crate::config::{Source, parse};

fn entry(name: &str, obj: Value) -> ServerEntry {
    entry_env(name, obj, &|_| None)
}

fn entry_env(name: &str, obj: Value, env: &dyn Fn(&str) -> Option<String>) -> ServerEntry {
    let text = json!({ "mcpServers": { name: obj } }).to_string();
    let (mut servers, warnings) = parse(&text, Source::User, env);
    assert!(warnings.is_empty(), "{warnings:?}");
    servers.remove(0)
}

#[test]
fn stdio_command_on_path_is_ok() {
    let e = entry("s", json!({"command": "sh", "args": ["-c", "x"]}));
    let row = static_check(&e);
    assert_eq!(row.level, Level::Ok);
    assert!(row.notes.is_empty());
}

#[test]
fn missing_command_fails() {
    let e = entry("s", json!({"command": "definitely-not-a-real-binary-xyz"}));
    let row = static_check(&e);
    assert_eq!(row.level, Level::Fail);
    assert!(row.notes[0].contains("command not found"));
}

#[test]
fn missing_tilde_arg_path_warns() {
    let e = entry("s", json!({"command": "sh", "args": ["~/definitely-missing-dir-xyz/script.py"]}));
    let row = static_check(&e);
    assert_eq!(row.level, Level::Warn);
    assert!(row.notes[0].contains("arg path does not exist"));
}

#[test]
fn http_url_must_parse() {
    let e = entry("h", json!({"url": "https://mcp.example.com/sse"}));
    assert_eq!(static_check(&e).level, Level::Ok);
    let e = entry("h", json!({"url": "not a url"}));
    assert_eq!(static_check(&e).level, Level::Fail);
}

#[test]
fn hardcoded_secret_header_warns_but_var_is_fine() {
    let e = entry("h", json!({"url": "https://x.example", "headers": {"Authorization": "Bearer sk-literal"}}));
    let row = static_check(&e);
    assert_eq!(row.level, Level::Warn);
    assert!(row.notes[0].contains("hardcoded secret"));

    let e =
        entry_env("h", json!({"url": "https://x.example", "headers": {"Authorization": "Bearer ${API_KEY}"}}), &|_| {
            Some("v".into())
        });
    assert_eq!(static_check(&e).level, Level::Ok);
}

#[test]
fn hardcoded_env_secret_warns() {
    let e = entry("s", json!({"command": "sh", "env": {"GITHUB_TOKEN": "ghp_literalvalue"}}));
    let row = static_check(&e);
    assert_eq!(row.level, Level::Warn);
    assert!(row.notes[0].contains("env.GITHUB_TOKEN"));
}

#[test]
fn suspicious_tool_names() {
    for n in ["exec", "run_shell", "bash_exec", "eval_js", "rm", "delete_file", "write_remote", "git_write_remote"] {
        assert!(suspicious_tool(n), "{n}");
    }
    for n in ["read_file", "format", "warm_cache", "list", "remote_write"] {
        assert!(!suspicious_tool(n), "{n}");
    }
}

#[test]
fn render_makes_a_table() {
    let rows = vec![
        Row { name: "a".into(), level: Level::Ok, notes: vec![] },
        Row { name: "bb".into(), level: Level::Fail, notes: vec!["gone".into()] },
    ];
    let out = render(&rows, &["w1".into()]);
    let mut lines = out.lines();
    assert_eq!(lines.next(), Some("a   ok    -"));
    assert_eq!(lines.next(), Some("bb  fail  gone"));
    assert!(out.contains("warning: w1"));
}
