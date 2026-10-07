use std::io::Write;
use std::path::Path;
use std::sync::{Arc, Mutex};

use serde_json::{Value, json};

use super::{Io, handle};
use crate::hub::{ConnectFn, Hub};

#[derive(Clone, Default)]
struct Sink(Arc<Mutex<Vec<u8>>>);

impl Write for Sink {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl Sink {
    fn frames(&self) -> Vec<Value> {
        let bytes = self.0.lock().unwrap().clone();
        String::from_utf8(bytes).unwrap().lines().map(|l| serde_json::from_str(l).unwrap()).collect()
    }
}

fn io() -> (Arc<Io>, Sink) {
    let sink = Sink::default();
    (Arc::new(Io::new(Box::new(sink.clone()))), sink)
}

fn failing_connect() -> ConnectFn {
    Arc::new(|_, _| Box::pin(async { anyhow::bail!("never connects") }))
}

#[tokio::test]
async fn io_request_matches_response_by_id() {
    let (io, sink) = io();
    let io2 = Arc::clone(&io);
    let task = tokio::spawn(async move { io2.request("host/ask", json!({"q": 1})).await });
    tokio::task::yield_now().await;
    let sent = sink.frames();
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0]["method"], "host/ask");
    let id = sent[0]["id"].as_str().unwrap().to_string();
    assert!(id.starts_with("mcp-ask-"));
    assert!(!io.on_frame(&json!({"id": "other", "result": {}})));
    assert!(io.on_frame(&json!({"id": id, "result": {"answers": {}}})));
    let res = task.await.unwrap().unwrap();
    assert_eq!(res, json!({"answers": {}}));
}

#[tokio::test]
async fn io_request_err_on_host_error() {
    let (io, sink) = io();
    let io2 = Arc::clone(&io);
    let task = tokio::spawn(async move { io2.request("host/ask", json!({})).await });
    tokio::task::yield_now().await;
    let id = sink.frames()[0]["id"].clone();
    assert!(io.on_frame(&json!({"id": id, "result": {"error": "capability_not_granted: host.ask"}})));
    let err = task.await.unwrap().unwrap_err().to_string();
    assert!(err.contains("capability_not_granted"), "{err}");
}

#[tokio::test]
async fn handle_manifest_and_unknown_method() {
    let (io, sink) = io();
    let hub = Hub::new(vec![], failing_connect());
    let m = handle(&json!({"id": 1, "method": "plugin/manifest"}), &hub, &io, Path::new("/tmp")).await.unwrap();
    assert_eq!(m["name"], "mcp");
    let none = handle(&json!({"id": 2, "method": "nope/x", "params": {}}), &hub, &io, Path::new("/tmp")).await;
    assert!(none.is_none());
    let frames = sink.frames();
    assert_eq!(frames.len(), 1);
    assert_eq!(frames[0]["id"], 2);
    assert_eq!(frames[0]["error"]["code"], -32601);
    assert!(
        handle(
            &json!({"method": "plugin/shutdown", "params": {"reason": "session_end"}}),
            &hub,
            &io,
            Path::new("/tmp")
        )
        .await
        .is_none()
    );
}

#[tokio::test]
async fn tool_call_unknown_tool_is_error() {
    let (io, _sink) = io();
    let hub = Hub::new(vec![], failing_connect());
    let req = json!({"id": 3, "method": "tool/call", "params": {"name": "mcp__x__y", "args": {}, "session": {"id": "", "cwd": "/tmp"}}});
    let res = handle(&req, &hub, &io, Path::new("/tmp")).await.unwrap();
    assert_eq!(res["is_error"], true);
    assert!(res["content"].as_str().unwrap().contains("mcp__x__y"));
    let tools =
        handle(&json!({"id": 4, "method": "plugin/tools", "params": {}}), &hub, &io, Path::new("/tmp")).await.unwrap();
    assert_eq!(tools["tools"], json!([]));
}
