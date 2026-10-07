use super::*;

#[test]
fn manifest_has_required_shape() {
    let m = manifest();
    assert_eq!(m["name"], "mcp");
    assert_eq!(m["protocol"], "1.3");
    assert_eq!(m["tools"], serde_json::json!([]));
    assert_eq!(m["commands"], serde_json::json!(["/mcp"]));
    let caps = m["capabilities"].as_array().unwrap();
    assert!(caps.contains(&serde_json::json!("host.ask")));
    let comp = m["completion"].as_array().unwrap();
    assert!(comp.contains(&serde_json::json!("serve")));
}

#[test]
fn sidecar_detection() {
    assert!(is_sidecar_invocation(&[], false));
    assert!(!is_sidecar_invocation(&[], true));
    assert!(!is_sidecar_invocation(&["list".into()], false));
}

#[test]
fn gray_home_respects_env() {
    let p = gray_home_from(Some("/x/g".into()), Some("/h".into())).unwrap();
    assert_eq!(p, PathBuf::from("/x/g"));
    let p = gray_home_from(Some("  ".into()), Some("/h".into())).unwrap();
    assert_eq!(p, PathBuf::from("/h/.gray"));
    assert!(gray_home_from(None, None).is_err());
}
