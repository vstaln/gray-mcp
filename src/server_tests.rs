use super::{list_sessions, parse_ndjson, read_session};

#[test]
fn parse_ndjson_result_and_session() {
    let rows = concat!(
        r#"{"type":"progress","protocol":1,"turn_id":"t1","session_id":"abc-123"}"#,
        "\n",
        r#"{"type":"result","text":"hi","usage":{"in":1},"session_id":"abc-123"}"#,
        "\n"
    );
    let out = parse_ndjson(rows).unwrap();
    assert_eq!(out.text, "hi");
    assert_eq!(out.session_id.as_deref(), Some("abc-123"));
    assert_eq!(out.usage["in"], 1);
}

#[test]
fn parse_ndjson_error_row_is_err() {
    let rows = r#"{"type":"error","code":"session_locked","message":"busy","pid":42,"session_id":null}"#;
    let err = parse_ndjson(rows).unwrap_err().to_string();
    assert!(err.contains("session_locked") && err.contains("busy") && err.contains("42"), "{err}");
    assert!(parse_ndjson("").unwrap_err().to_string().contains("no result row"));
}

#[test]
fn parse_ndjson_ignores_garbage_lines() {
    let rows = concat!(
        "warning: something on stdout\n",
        r#"{"type":"result","text":"first"}"#,
        "\nnot json {\n",
        r#"{"type":"result","text":"last","session_id":"s"}"#,
        "\n"
    );
    let out = parse_ndjson(rows).unwrap();
    assert_eq!(out.text, "last");
    assert_eq!(out.session_id.as_deref(), Some("s"));
    assert!(out.usage.is_null());
}

fn write_session(dir: &std::path::Path, id: &str, body: &[&str]) {
    let mut text = format!(r#"{{"version":1,"id":"{id}","timestamp":1700000000,"cwd":"/w/{id}","model":"m"}}"#);
    text.push('\n');
    for line in body {
        text.push_str(line);
        text.push('\n');
    }
    std::fs::write(dir.join(format!("{id}.jsonl")), text).unwrap();
}

#[test]
fn list_sessions_newest_first_and_limit() {
    let dir = tempfile::tempdir().unwrap();
    for id in ["a", "b", "c"] {
        write_session(dir.path(), id, &[]);
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    std::fs::write(dir.path().join("c.lock"), "1").unwrap();
    let all = list_sessions(dir.path(), 10).unwrap();
    let ids: Vec<&str> = all.iter().map(|r| r["id"].as_str().unwrap()).collect();
    assert_eq!(ids, ["c", "b", "a"]);
    assert_eq!(all[0]["cwd"], "/w/c");
    assert_eq!(all[0]["model"], "m");
    assert_eq!(all[0]["started_at"], 1700000000);
    assert!(all[0]["path"].as_str().unwrap().ends_with("c.jsonl"));
    assert_eq!(list_sessions(dir.path(), 2).unwrap().len(), 2);
    assert!(list_sessions(&dir.path().join("missing"), 2).unwrap().is_empty());
}

#[test]
fn read_session_rejects_traversal() {
    let dir = tempfile::tempdir().unwrap();
    for bad in ["../x", "a/b", "", "a.b", "x y"] {
        let err = read_session(dir.path(), bad, 5).unwrap_err().to_string();
        assert!(err.contains("invalid session id"), "{bad:?}: {err}");
    }
}

fn entry(id: u64, role: &str, content: &str) -> String {
    format!(
        r#"{{"entry_id":{id},"parent_id":null,"timestamp":{id},"message":{{"role":"{role}","content":[{content}]}}}}"#
    )
}

#[test]
fn read_session_last_n_renders_roles() {
    let dir = tempfile::tempdir().unwrap();
    let e1 = entry(1, "user", r#"{"type":"text","text":"hello"}"#);
    let e2 = entry(2, "assistant", r#"{"type":"tool_use","id":"x","name":"bash","input":{}}"#);
    let e3 = entry(3, "assistant", r#"{"type":"text","text":"a"},{"type":"text","text":"b"}"#);
    let e4 = entry(4, "user", r#"{"type":"text","text":"bye"}"#);
    write_session(dir.path(), "s1", &[&e1, &e2, &e3, &e4]);
    assert_eq!(read_session(dir.path(), "s1", 10).unwrap(), "user: hello\nassistant: a\nb\nuser: bye\n");
    assert_eq!(read_session(dir.path(), "s1", 1).unwrap(), "user: bye\n");
    assert!(read_session(dir.path(), "nope", 1).is_err());
}
