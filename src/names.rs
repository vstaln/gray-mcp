//! Tool naming (`mcp__<server>__<tool>`) and input-schema normalisation.

use std::collections::HashSet;

use serde_json::{Value, json};

/// Model-facing tool names are capped here (Anthropic/OpenAI limit).
pub const MAX_NAME_LEN: usize = 64;
pub const PREFIX: &str = "mcp__";

/// Keep `[A-Za-z0-9_]`, everything else becomes `_`.
pub fn sanitise(s: &str) -> String {
    s.chars().map(|c| if c.is_ascii_alphanumeric() || c == '_' { c } else { '_' }).collect()
}

/// `mcp__<server>__<tool>`, at most `MAX_NAME_LEN` bytes, made unique
/// against `taken` with `_2`, `_3`, … (truncating first so the suffix
/// fits). The chosen name is inserted into `taken`.
pub fn tool_name(server: &str, tool: &str, taken: &mut HashSet<String>) -> String {
    let base = format!("{PREFIX}{}__{}", sanitise(server), sanitise(tool));
    let mut n = 1usize;
    loop {
        let suffix = if n == 1 { String::new() } else { format!("_{n}") };
        let keep = MAX_NAME_LEN.saturating_sub(suffix.len());
        let mut name: String = base.chars().take(keep).collect();
        name.push_str(&suffix);
        if taken.insert(name.clone()) {
            return name;
        }
        n += 1;
    }
}

/// The schema gray/LLM providers accept: an object with `properties`.
/// Non-object schemas (or none) become the empty object schema; object
/// schemas missing `properties` get an empty one.
pub fn fix_schema(schema: Option<Value>) -> Value {
    let default = || json!({"type": "object", "properties": {}});
    let Some(Value::Object(mut m)) = schema else { return default() };
    if m.get("type").and_then(Value::as_str) != Some("object") {
        return default();
    }
    if !m.get("properties").is_some_and(Value::is_object) {
        m.insert("properties".into(), json!({}));
    }
    Value::Object(m)
}

#[cfg(test)]
#[path = "names_tests.rs"]
mod names_tests;
