use std::path::{Path, PathBuf};
use std::time::Duration;

use clap::Parser;
use serde_json::json;

use super::{Cli, Cmd, build_entry, edit_add, edit_remove, render_list};
use crate::config::{Config, ServerEntry, Source, Transport};
use crate::consent::ConsentStore;

#[test]
fn edit_add_creates_mcpservers_and_keeps_other_keys() {
    let doc = json!({"other": 1});
    let out = edit_add(doc, "fs", json!({"command": "npx"}));
    assert_eq!(out["other"], 1);
    assert_eq!(out["mcpServers"]["fs"]["command"], "npx");
    let out = edit_add(out, "fs", json!({"url": "http://x"}));
    assert_eq!(out["mcpServers"]["fs"], json!({"url": "http://x"}));
    assert_eq!(edit_add(json!(null), "a", json!({}))["mcpServers"]["a"], json!({}));
}

#[test]
fn edit_remove_missing_is_err() {
    assert!(edit_remove(json!({}), "fs").is_err());
    assert!(edit_remove(json!({"mcpServers": {"x": {}}}), "fs").is_err());
    let out = edit_remove(json!({"mcpServers": {"fs": {}, "x": {}}, "k": true}), "fs").unwrap();
    assert_eq!(out, json!({"mcpServers": {"x": {}}, "k": true}));
}

fn entry(name: &str, source: Source) -> ServerEntry {
    ServerEntry {
        name: name.into(),
        source,
        transport: Transport::Stdio {
            command: "srv".into(),
            args: vec!["--x".into()],
            env: Default::default(),
            env_file: None,
        },
        timeout: Duration::from_secs(60),
        disabled: false,
        raw: "{}".into(),
    }
}

#[test]
fn render_list_marks_project_pending_consent() {
    let dir = tempfile::tempdir().unwrap();
    let consent = ConsentStore::load(dir.path().join("consent.json"));
    let proj = PathBuf::from("/proj");
    let cfg = Config {
        servers: vec![entry("u", Source::User), entry("p", Source::Project(proj.clone()))],
        warnings: vec!["bad thing".into()],
    };
    let out = render_list(&cfg, &consent, Path::new("/proj"));
    let lines: Vec<&str> = out.lines().collect();
    assert!(lines[0].starts_with("u  user     allowed"), "{out}");
    assert!(lines[1].starts_with("p  project  needs consent"), "{out}");
    assert!(lines[1].ends_with("srv --x"), "{out}");
    assert_eq!(lines[2], "warning: bad thing");

    let mut consent = consent;
    consent.allow(ConsentStore::key(&proj, "p", "{}")).unwrap();
    let out = render_list(&cfg, &consent, Path::new("/proj"));
    assert!(out.lines().nth(1).unwrap().contains("allowed"), "{out}");

    let empty = Config { servers: vec![], warnings: vec![] };
    assert!(render_list(&empty, &consent, Path::new("/proj")).starts_with("no MCP servers configured"));
}

#[test]
fn clap_parses_add_with_double_dash() {
    let cli = Cli::try_parse_from(["gray-mcp", "add", "fs", "--env", "A=1", "--", "npx", "-y", "srv"]).unwrap();
    let Cmd::Add { name, project, url, env, command, .. } = cli.cmd else { panic!("not add") };
    assert_eq!(name, "fs");
    assert!(!project && url.is_none());
    assert_eq!(env, vec!["A=1"]);
    assert_eq!(command, vec!["npx", "-y", "srv"]);
    let e = build_entry(None, command, env, vec![], Some(30)).unwrap();
    assert_eq!(e, json!({"command": "npx", "args": ["-y", "srv"], "env": {"A": "1"}, "timeout": 30}));

    let cli = Cli::try_parse_from(["gray-mcp", "add", "web", "--project", "--url", "http://h/mcp", "--header", "X=y"])
        .unwrap();
    let Cmd::Add { project, url, header, command, .. } = cli.cmd else { panic!("not add") };
    assert!(project);
    let e = build_entry(url, command, vec![], header, None).unwrap();
    assert_eq!(e, json!({"url": "http://h/mcp", "headers": {"X": "y"}}));

    assert!(build_entry(None, vec![], vec![], vec![], None).is_err());
    assert!(build_entry(None, vec!["c".into()], vec!["novalue".into()], vec![], None).is_err());
}
