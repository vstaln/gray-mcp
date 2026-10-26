use std::collections::BTreeMap;

use serde_json::{Map, Value, json};

pub const SERVER_BUDGET: usize = 32 * 1024;
pub const SCHEMA_TOOL: &str = "mcp_schema";
const DESC_MAX: usize = 240;
const ENUM_MAX: usize = 8;

pub fn schema_tool_def() -> Value {
    json!({
        "name": SCHEMA_TOOL,
        "description": "Full description and argument schema of an MCP tool whose listing is abbreviated. Call before using one with nested arguments.",
        "parameters": {
            "type": "object",
            "properties": {"tool": {"type": "string"}},
            "required": ["tool"],
        },
        "read_only": true,
    })
}

fn name_of(def: &Value) -> Option<&str> {
    def.get("name").and_then(Value::as_str)
}

pub fn publish(defs: &[Value], server_of: impl Fn(&str) -> Option<String>) -> (Vec<Value>, bool) {
    let mut size: BTreeMap<String, usize> = BTreeMap::new();
    for d in defs {
        if let Some(s) = name_of(d).and_then(&server_of) {
            *size.entry(s).or_default() += d.to_string().len();
        }
    }
    let mut slimmed = false;
    let mut out = Vec::with_capacity(defs.len());
    for d in defs {
        let over = name_of(d).and_then(&server_of).is_some_and(|s| size.get(&s).is_some_and(|&n| n > SERVER_BUDGET));
        if over {
            slimmed = true;
            out.push(slim_def(d));
        } else {
            out.push(d.clone());
        }
    }
    (out, slimmed)
}

fn short_description(text: &str) -> String {
    let first = text.split("\n").next().unwrap_or("");
    let first = match first.find(". ") {
        Some(i) => &first[..=i],
        None => first,
    };
    let mut s: String = first.chars().take(DESC_MAX).collect();
    if s.len() < first.len() {
        s.push('…');
    }
    s
}

fn slim_def(def: &Value) -> Value {
    let desc = def.get("description").and_then(Value::as_str).unwrap_or("");
    let mut out = json!({
        "name": def.get("name").cloned().unwrap_or(Value::Null),
        "description": format!("{} Full schema: {SCHEMA_TOOL}.", short_description(desc)).trim_start().to_string(),
        "parameters": slim_schema(def.get("parameters").unwrap_or(&Value::Null)),
    });
    if let Some(ro) = def.get("read_only") {
        out["read_only"] = ro.clone();
    }
    out
}

fn slim_prop(prop: &Value) -> Value {
    let mut out = Map::new();
    if let Some(t) = prop.get("type") {
        out.insert("type".into(), t.clone());
    }
    if let Some(e) = prop.get("enum").and_then(Value::as_array).filter(|e| e.len() <= ENUM_MAX) {
        out.insert("enum".into(), Value::Array(e.clone()));
    }
    Value::Object(out)
}

fn slim_schema(schema: &Value) -> Value {
    let mut props = Map::new();
    let mut add = |s: &Value| {
        if let Some(p) = s.get("properties").and_then(Value::as_object) {
            for (k, v) in p {
                props.entry(k.clone()).or_insert_with(|| slim_prop(v));
            }
        }
    };
    add(schema);
    let variants = ["anyOf", "oneOf", "allOf"];
    let mut branched = false;
    for key in variants {
        if let Some(vs) = schema.get(key).and_then(Value::as_array) {
            branched |= key != "allOf";
            vs.iter().for_each(&mut add);
        }
    }
    let mut out = json!({"type": "object", "properties": props});
    if !branched && let Some(req) = schema.get("required").and_then(Value::as_array) {
        out["required"] = Value::Array(req.clone());
    }
    out
}
