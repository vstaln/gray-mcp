//! MCP `CallToolResult` → gray `tool/call` reply
//! (`{"content", "is_error"?, "images"?, "media"?}`, protocol 1.3).

use rmcp::model::{CallToolResult, ContentBlock, ResourceContents};
use serde_json::{Value, json};

/// Text for the model when the result carries nothing.
pub const NO_CONTENT: &str = "(no content)";

pub fn to_reply(result: &CallToolResult) -> Value {
    let mut parts: Vec<String> = Vec::new();
    let mut images: Vec<Value> = Vec::new();
    let mut media: Vec<Value> = Vec::new();
    for block in &result.content {
        match block {
            ContentBlock::Text(t) => parts.push(t.text.clone()),
            ContentBlock::Image(i) => {
                parts.push(format!("[image {}]", i.mime_type));
                images.push(json!({"mime": i.mime_type, "data_base64": i.data}));
            }
            ContentBlock::Resource(r) => match &r.resource {
                ResourceContents::TextResourceContents { uri, text, .. } => {
                    parts.push(format!("[resource {uri}]\n{text}"));
                }
                ResourceContents::BlobResourceContents { uri, mime_type, blob, .. } => {
                    let mime = mime_type.clone().unwrap_or_else(|| "application/octet-stream".into());
                    parts.push(format!("[resource {uri} ({mime}) attached]"));
                    media.push(json!({"mime": mime, "data_base64": blob, "name": uri}));
                }
                _ => parts.push("[resource of unknown kind omitted]".into()),
            },
            ContentBlock::Audio(_) => parts.push("[audio omitted]".into()),
            ContentBlock::ResourceLink(l) => parts.push(format!("[resource link {} omitted]", l.uri)),
            _ => parts.push("[content of unknown kind omitted]".into()),
        }
    }
    if parts.is_empty() {
        parts.push(NO_CONTENT.into());
    }
    let mut reply = json!({ "content": parts.join("\n\n") });
    if result.is_error == Some(true) {
        reply["is_error"] = json!(true);
    }
    if !images.is_empty() {
        reply["images"] = Value::Array(images);
    }
    if !media.is_empty() {
        reply["media"] = Value::Array(media);
    }
    reply
}

#[cfg(test)]
#[path = "convert_tests.rs"]
mod convert_tests;
