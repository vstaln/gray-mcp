use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use rmcp::model::{CallToolResult, ContentBlock, Tool};
use serde_json::{Value, json};
use tokio::sync::mpsc::UnboundedSender;

use super::*;
use crate::config::{Source, Transport};

fn entry(name: &str) -> ServerEntry {
    ServerEntry {
        name: name.into(),
        source: Source::User,
        transport: Transport::Stdio { command: "x".into(), args: vec![], env: BTreeMap::new(), env_file: None },
        timeout: Duration::from_secs(5),
        disabled: false,
        raw: "{}".into(),
    }
}

fn tool(name: &str) -> Tool {
    Tool::new(name.to_string(), "d", serde_json::Map::new())
}

struct FakeConn {
    server: String,
    tools: Arc<Mutex<Vec<Tool>>>,
}

#[async_trait::async_trait]
impl Conn for FakeConn {
    async fn list_tools(&self) -> anyhow::Result<Vec<Tool>> {
        Ok(self.tools.lock().unwrap().clone())
    }
    async fn call(&self, tool: &str, args: Value, _t: Duration) -> anyhow::Result<CallToolResult> {
        Ok(CallToolResult::success(vec![ContentBlock::text(format!("{}:{tool}:{args}", self.server))]))
    }
}

type Tools = Arc<Mutex<BTreeMap<String, Arc<Mutex<Vec<Tool>>>>>>;

/// Connect fn serving `tools[server]`; servers absent from the map fail.
fn fake_connect(tools: Tools, senders: Arc<Mutex<Vec<UnboundedSender<String>>>>) -> ConnectFn {
    Arc::new(move |entry, changed| {
        let tools = tools.clone();
        let senders = senders.clone();
        Box::pin(async move {
            let list = tools.lock().unwrap().get(&entry.name).cloned();
            let Some(list) = list else { anyhow::bail!("no such server") };
            senders.lock().unwrap().push(changed);
            Ok(Arc::new(FakeConn { server: entry.name.clone(), tools: list }) as Arc<dyn Conn>)
        })
    })
}

async fn ready(hub: &Arc<Hub>, want: usize) {
    let mut rx = hub.subscribe();
    for _ in 0..50 {
        if hub.tools().len() >= want {
            return;
        }
        let _ = tokio::time::timeout(Duration::from_millis(50), rx.changed()).await;
    }
    panic!("hub never reached {want} tools: {:?}", hub.tools());
}

fn setup(servers: &[(&str, &[&str])]) -> (Arc<Hub>, Tools, Arc<Mutex<Vec<UnboundedSender<String>>>>) {
    let tools: Tools = Default::default();
    for (s, ts) in servers {
        tools.lock().unwrap().insert(s.to_string(), Arc::new(Mutex::new(ts.iter().map(|t| tool(t)).collect())));
    }
    let senders = Arc::new(Mutex::new(Vec::new()));
    let hub = Hub::with_backoff_base(
        servers.iter().map(|(s, _)| entry(s)).collect(),
        fake_connect(tools.clone(), senders.clone()),
        Duration::from_millis(10),
    );
    (hub, tools, senders)
}

#[tokio::test]
async fn start_lists_tools_and_prefixes_names() {
    let (hub, _, _) = setup(&[("a", &["x"]), ("b", &["y"])]);
    hub.start().await;
    ready(&hub, 2).await;
    let names: Vec<String> = hub.tools().iter().map(|d| d["name"].as_str().unwrap().to_string()).collect();
    assert_eq!(names, vec!["mcp__a__x", "mcp__b__y"]);
    assert_eq!(hub.tools()[0]["parameters"], json!({"type": "object", "properties": {}}));
    assert_eq!(hub.status(), vec![("a".into(), State::Ready { tools: 1 }), ("b".into(), State::Ready { tools: 1 })]);
}

#[tokio::test]
async fn call_routes_to_right_server() {
    let (hub, _, _) = setup(&[("a", &["x"]), ("b", &["x"])]);
    hub.start().await;
    ready(&hub, 2).await;
    let r = hub.call("mcp__b__x", json!({"k": 1})).await;
    assert_eq!(r["content"], "b:x:{\"k\":1}");
    assert!(r.get("is_error").is_none());
}

#[tokio::test]
async fn unknown_tool_is_error() {
    let (hub, _, _) = setup(&[("a", &["x"])]);
    let r = hub.call("mcp__nope__x", json!({})).await;
    assert_eq!(r["is_error"], json!(true));
    assert!(r["content"].as_str().unwrap().contains("unknown tool"));
}

#[tokio::test]
async fn rebuild_bumps_watch_only_on_change() {
    let (hub, _, _) = setup(&[("a", &["x"])]);
    hub.start().await;
    ready(&hub, 1).await;
    let rx = hub.subscribe();
    let before = *rx.borrow();
    hub.rebuild().await;
    hub.rebuild().await;
    assert_eq!(*rx.borrow(), before);
}

#[tokio::test]
async fn tools_changed_triggers_relist() {
    let (hub, tools, senders) = setup(&[("a", &["x"])]);
    hub.start().await;
    ready(&hub, 1).await;
    tools.lock().unwrap()["a"].lock().unwrap().push(tool("z"));
    let tx = senders.lock().unwrap()[0].clone();
    tx.send("a".into()).unwrap();
    ready(&hub, 2).await;
    assert_eq!(hub.tools()[1]["name"], "mcp__a__z");
    assert_eq!(hub.status()[0].1, State::Ready { tools: 2 });
}

#[tokio::test]
async fn failed_connect_sets_failed_state() {
    let (hub, _, _) = setup(&[("a", &["x"])]);
    hub.deny("a");
    let tools: Tools = Default::default();
    let hub2 = Hub::with_backoff_base(
        vec![entry("ghost")],
        fake_connect(tools, Arc::new(Mutex::new(Vec::new()))),
        Duration::from_millis(10),
    );
    hub2.start().await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(matches!(hub2.status()[0].1, State::Failed { .. }), "{:?}", hub2.status());
    assert_eq!(hub.status()[0].1, State::Denied);
}

#[tokio::test]
async fn pending_server_starts_on_demand() {
    let (hub, _, _) = setup(&[("a", &["x"]), ("p", &["y"])]);
    hub.mark_pending("p");
    hub.start().await;
    ready(&hub, 1).await;
    assert_eq!(hub.status()[1].1, State::Pending);
    hub.start_server("p").await;
    ready(&hub, 2).await;
    assert_eq!(hub.status()[1].1, State::Ready { tools: 1 });
    hub.start_server("nope").await;
    assert_eq!(hub.status().len(), 2);
}

#[tokio::test]
async fn reload_replaces_servers() {
    let (hub, _, _) = setup(&[("a", &["x"])]);
    hub.start().await;
    ready(&hub, 1).await;
    hub.reload(vec![]).await;
    assert!(hub.tools().is_empty());
    assert!(hub.status().is_empty());
}
