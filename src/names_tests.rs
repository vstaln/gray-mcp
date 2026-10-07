use std::collections::HashSet;

use serde_json::json;

use super::*;

#[test]
fn sanitises_dots_and_dashes() {
    let mut t = HashSet::new();
    assert_eq!(tool_name("my-srv", "a.b", &mut t), "mcp__my_srv__a_b");
    assert!(t.contains("mcp__my_srv__a_b"));
}

#[test]
fn truncates_to_64() {
    let mut t = HashSet::new();
    let long = "x".repeat(100);
    let n = tool_name("s", &long, &mut t);
    assert_eq!(n.len(), 64);
    assert!(n.starts_with("mcp__s__xxx"));
}

#[test]
fn collision_suffixes() {
    let mut t = HashSet::new();
    assert_eq!(tool_name("s", "a", &mut t), "mcp__s__a");
    assert_eq!(tool_name("s", "a", &mut t), "mcp__s__a_2");
    assert_eq!(tool_name("s", "a", &mut t), "mcp__s__a_3");
    let long = "y".repeat(100);
    tool_name("s", &long, &mut t);
    let second = tool_name("s", &long, &mut t);
    assert_eq!(second.len(), 64);
    assert!(second.ends_with("_2"));
}

#[test]
fn fix_schema_forces_object_and_properties() {
    assert_eq!(fix_schema(Some(json!({"type": "string"}))), json!({"type": "object", "properties": {}}));
    assert_eq!(fix_schema(None), json!({"type": "object", "properties": {}}));
    assert_eq!(fix_schema(Some(json!([1]))), json!({"type": "object", "properties": {}}));
    let ok = json!({"type": "object", "properties": {"a": {}}, "required": ["a"]});
    assert_eq!(fix_schema(Some(ok.clone())), ok);
    assert_eq!(fix_schema(Some(json!({"type": "object"}))), json!({"type": "object", "properties": {}}));
}
