use std::path::Path;
use std::time::Duration;

use serde_json::json;

use super::*;
use crate::config::{Source, Transport};

fn entry(name: &str) -> ServerEntry {
    ServerEntry {
        name: name.into(),
        source: Source::Project("/proj".into()),
        transport: Transport::Stdio {
            command: "npx".into(),
            args: vec!["srv".into()],
            env: Default::default(),
            env_file: None,
        },
        timeout: Duration::from_secs(120),
        disabled: false,
        raw: r#"{"command":"npx"}"#.into(),
    }
}

#[test]
fn key_is_stable_and_sensitive() {
    let a = ConsentStore::key(Path::new("/p"), "s", "{}");
    assert_eq!(a, ConsentStore::key(Path::new("/p"), "s", "{}"));
    assert_eq!(a.len(), 64);
    assert_ne!(a, ConsentStore::key(Path::new("/p"), "s", "{\"x\":1}"));
    assert_ne!(a, ConsentStore::key(Path::new("/q"), "s", "{}"));
    assert_ne!(a, ConsentStore::key(Path::new("/p"), "t", "{}"));
}

#[test]
fn allow_persists_and_reloads() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("nested").join("consent.json");
    let mut s = ConsentStore::load(path.clone());
    assert!(!s.is_allowed("k"));
    s.allow("k".into()).unwrap();
    assert!(s.is_allowed("k"));
    let again = ConsentStore::load(path.clone());
    assert!(again.is_allowed("k"));
    assert!(!again.is_allowed("other"));
    let text = std::fs::read_to_string(path).unwrap();
    assert!(text.contains("\"allowed\"") && text.ends_with('\n'));
}

#[test]
fn corrupt_file_loads_empty() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("consent.json");
    std::fs::write(&path, "{nope").unwrap();
    let s = ConsentStore::load(path);
    assert!(!s.is_allowed("k"));
}

#[test]
fn ask_params_shape() {
    let e = entry("fs");
    let p = ask_params(&[&e]);
    assert_eq!(p["blocking"], json!(true));
    let q = &p["questions"][0];
    assert_eq!(q["id"], "fs");
    assert_eq!(q["header"], "MCP server");
    let text = q["question"].as_str().unwrap();
    assert!(text.contains("npx srv") && text.contains("/proj/.mcp.json"), "{text}");
    let labels: Vec<&str> = q["options"].as_array().unwrap().iter().map(|o| o["label"].as_str().unwrap()).collect();
    assert_eq!(labels, vec!["Allow", "Deny"]);
}

#[test]
fn ask_params_caps_at_three() {
    let es: Vec<ServerEntry> = ["a", "b", "c", "d"].iter().map(|n| entry(n)).collect();
    let refs: Vec<&ServerEntry> = es.iter().collect();
    assert_eq!(ask_params(&refs)["questions"].as_array().unwrap().len(), 3);
}

#[test]
fn parse_ask_result_maps_labels() {
    let r = json!({"answers": {
        "a": {"answers": ["Allow", "user_note: fine"]},
        "b": {"answers": ["Deny"]},
        "c": {"answers": ["allow"]}
    }});
    assert_eq!(
        parse_ask_result(&r, &["a", "b", "c", "missing"]),
        vec![("a".into(), true), ("b".into(), false), ("c".into(), false), ("missing".into(), false)]
    );
    assert_eq!(parse_ask_result(&json!({}), &["a"]), vec![("a".into(), false)]);
}
