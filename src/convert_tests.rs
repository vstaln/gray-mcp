use rmcp::model::{CallToolResult, ContentBlock, ResourceContents};
use serde_json::json;

use super::*;

#[test]
fn text_blocks_join_with_blank_line() {
    let r = CallToolResult::success(vec![ContentBlock::text("a"), ContentBlock::text("b")]);
    let v = to_reply(&r);
    assert_eq!(v["content"], "a\n\nb");
    assert!(v.get("is_error").is_none() && v.get("images").is_none() && v.get("media").is_none());
}

#[test]
fn image_goes_to_images_with_placeholder() {
    let r = CallToolResult::success(vec![ContentBlock::image("AAAA", "image/png")]);
    let v = to_reply(&r);
    assert_eq!(v["content"], "[image image/png]");
    assert_eq!(v["images"], json!([{"mime": "image/png", "data_base64": "AAAA"}]));
}

#[test]
fn text_resource_appended_with_uri_header() {
    let r = CallToolResult::success(vec![ContentBlock::resource(ResourceContents::TextResourceContents {
        uri: "file:///x.txt".into(),
        mime_type: None,
        text: "body".into(),
        meta: None,
    })]);
    assert_eq!(to_reply(&r)["content"], "[resource file:///x.txt]\nbody");
}

#[test]
fn blob_resource_goes_to_media_with_fallback() {
    let r = CallToolResult::success(vec![ContentBlock::resource(ResourceContents::BlobResourceContents {
        uri: "file:///a.pdf".into(),
        mime_type: Some("application/pdf".into()),
        blob: "QUJD".into(),
        meta: None,
    })]);
    let v = to_reply(&r);
    assert_eq!(v["content"], "[resource file:///a.pdf (application/pdf) attached]");
    assert_eq!(v["media"], json!([{"mime": "application/pdf", "data_base64": "QUJD", "name": "file:///a.pdf"}]));
}

#[test]
fn is_error_propagates() {
    let r = CallToolResult::error(vec![ContentBlock::text("boom")]);
    let v = to_reply(&r);
    assert_eq!(v["content"], "boom");
    assert_eq!(v["is_error"], json!(true));
}

#[test]
fn empty_content_placeholder() {
    let v = to_reply(&CallToolResult::success(vec![]));
    assert_eq!(v["content"], NO_CONTENT);
    assert!(v.get("is_error").is_none());
}

#[test]
fn audio_is_omitted_marker() {
    let v = to_reply(&CallToolResult::success(vec![ContentBlock::audio("AA", "audio/wav")]));
    assert_eq!(v["content"], "[audio omitted]");
}
