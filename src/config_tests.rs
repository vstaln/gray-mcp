use std::path::Path;
use std::time::Duration;

use super::*;

fn env(k: &str) -> Option<String> {
    match k {
        "A" => Some("1".into()),
        "TOKEN" => Some("sekrit".into()),
        _ => None,
    }
}

fn one(json: &str) -> (Vec<ServerEntry>, Vec<String>) {
    parse(json, Source::User, &env)
}

#[test]
fn expand_replaces_set_vars_and_errors_on_unset() {
    assert_eq!(expand("${A}/x", &env).unwrap(), "1/x");
    assert_eq!(expand("${NOPE}", &env), Err("NOPE".to_string()));
    assert_eq!(expand("$A and ${ and ${1x}", &env).unwrap(), "$A and ${ and ${1x}");
    assert_eq!(expand("a${A}b${A}c", &env).unwrap(), "a1b1c");
}

#[test]
fn parse_rejects_both_and_neither() {
    let (s, w) = one(r#"{"mcpServers":{"x":{"command":"c","url":"http://u"},"y":{}}}"#);
    assert!(s.is_empty());
    assert_eq!(w.len(), 2);
    assert!(w[0].contains("'x'") && w[0].contains("both"), "{w:?}");
    assert!(w[1].contains("'y'"), "{w:?}");
}

#[test]
fn parse_clamps_timeout() {
    let (s, _) = one(
        r#"{"mcpServers":{"a":{"command":"c","timeout":0},"b":{"command":"c","timeout":1000},"c":{"command":"c"}}}"#,
    );
    let by = |n: &str| s.iter().find(|e| e.name == n).unwrap().timeout;
    assert_eq!(by("a"), Duration::from_secs(1));
    assert_eq!(by("b"), Duration::from_secs(300));
    assert_eq!(by("c"), Duration::from_secs(120));
}

#[test]
fn parse_skips_entry_with_unset_var_and_names_it() {
    let (s, w) = one(r#"{"mcpServers":{"x":{"url":"http://h","headers":{"Authorization":"Bearer ${MISSING}"}}}}"#);
    assert!(s.is_empty());
    assert_eq!(w.len(), 1);
    assert!(w[0].contains("MISSING") && w[0].contains("'x'"), "{w:?}");
}

#[test]
fn parse_expands_and_keeps_raw_unexpanded() {
    let (s, w) = one(
        r#"{"mcpServers":{"x":{"command":"${A}bin","args":["--t","${TOKEN}"],"env":{"K":"${TOKEN}"},"disabled":true}}}"#,
    );
    assert!(w.is_empty(), "{w:?}");
    let e = &s[0];
    assert!(e.disabled);
    match &e.transport {
        Transport::Stdio { command, args, env, .. } => {
            assert_eq!(command, "1bin");
            assert_eq!(args, &vec!["--t".to_string(), "sekrit".to_string()]);
            assert_eq!(env.get("K").unwrap(), "sekrit");
        }
        other => panic!("{other:?}"),
    }
    assert!(e.raw.contains("${TOKEN}") && !e.raw.contains("sekrit"));
}

#[test]
fn merge_project_wins() {
    let user = r#"{"mcpServers":{"shared":{"command":"user-cmd"},"only_user":{"url":"http://u"}}}"#;
    let proj = r#"{"mcpServers":{"shared":{"command":"proj-cmd"}}}"#;
    let dir = Path::new("/proj");
    let cfg = load_from(Some(user), Some((proj, dir)), &env);
    assert!(cfg.warnings.is_empty(), "{:?}", cfg.warnings);
    assert_eq!(cfg.servers.len(), 2);
    let shared = cfg.servers.iter().find(|e| e.name == "shared").unwrap();
    assert_eq!(shared.source, Source::Project(dir.to_path_buf()));
    assert_eq!(shared.transport.describe(), "proj-cmd");
    assert_eq!(cfg.servers.iter().find(|e| e.name == "only_user").unwrap().source, Source::User);
}

#[test]
fn raw_is_canonical() {
    let (a, _) = one(r#"{"mcpServers":{"x":{"command":"c","args":["1"]}}}"#);
    let (b, _) = one(r#"{"mcpServers":{"x":{"args":["1"],"command":"c"}}}"#);
    assert_eq!(a[0].raw, b[0].raw);
}

#[test]
fn load_reports_bad_json_as_warning() {
    let cfg = load_from(Some("{not json"), None, &env);
    assert!(cfg.servers.is_empty());
    assert_eq!(cfg.warnings.len(), 1);
    let cfg = load_from(Some(r#"{"servers":{}}"#), None, &env);
    assert!(cfg.warnings[0].contains("mcpServers"));
}
