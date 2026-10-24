//! All configured servers in one place: connections, per-server state, the
//! flattened tool table (`mcp__<server>__<tool>` → server/tool) and a watch
//! channel that bumps whenever the model-facing tool list changes.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, RwLock};
use std::time::Duration;

use rmcp::model::{CallToolResult, Tool};
use serde_json::{Value, json};
use tokio::sync::mpsc::{self, UnboundedSender};
use tokio::sync::watch;

use crate::client::McpClient;
use crate::config::ServerEntry;
use crate::{convert, names};

/// A live server connection, abstracted so the hub can be tested without
/// child processes.
#[async_trait::async_trait]
pub trait Conn: Send + Sync {
    async fn list_tools(&self) -> anyhow::Result<Vec<Tool>>;
    async fn call(&self, tool: &str, args: Value, timeout: Duration) -> anyhow::Result<CallToolResult>;
    async fn close(&self) {}
}

#[async_trait::async_trait]
impl Conn for McpClient {
    async fn list_tools(&self) -> anyhow::Result<Vec<Tool>> {
        McpClient::list_tools(self).await
    }
    async fn call(&self, tool: &str, args: Value, timeout: Duration) -> anyhow::Result<CallToolResult> {
        McpClient::call(self, tool, args, timeout).await
    }
    async fn close(&self) {
        McpClient::close(self);
    }
}

pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;
pub type ConnectFn = Arc<
    dyn for<'a> Fn(&'a ServerEntry, UnboundedSender<String>) -> BoxFuture<'a, anyhow::Result<Arc<dyn Conn>>>
        + Send
        + Sync,
>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum State {
    Connecting,
    /// Project server awaiting the operator's consent.
    Pending,
    Ready {
        tools: usize,
    },
    Failed {
        error: String,
    },
    Denied,
    Disabled,
    /// Cached tools advertised; upstream spawns on first `tool/call`.
    Dormant,
}

/// Reconnect backoff: `base * 2^n`, capped at `MAX_BACKOFF`, at most
/// `MAX_ATTEMPTS` tries before the server stays `Failed`.
pub const MAX_ATTEMPTS: u32 = 10;
pub const MAX_BACKOFF: Duration = Duration::from_secs(60);

pub struct Hub {
    servers: RwLock<Vec<ServerEntry>>,
    conns: RwLock<HashMap<String, Arc<dyn Conn>>>,
    states: RwLock<BTreeMap<String, State>>,
    table: RwLock<BTreeMap<String, (String, String)>>,
    defs: RwLock<Vec<Value>>,
    changed_tx: watch::Sender<u64>,
    /// Sender handed to connections so they can report `tools/list_changed`;
    /// set by `start`, used by `start_server`.
    /// `~/.gray/mcp-tools-cache.json` — rebuilt server defs persist here
    /// so the next sidecar can seed dormant. `None` disables persistence.
    cache_path: RwLock<Option<std::path::PathBuf>>,
    /// Dormant seeds per server: `rebuild` rewrites table+defs from live
    /// conns, so a still-dormant server's advertised rows must be
    /// re-appended or the first unrelated rebuild would un-advertise them.
    seeds: RwLock<HashMap<String, Vec<crate::toolcache::CachedTool>>>,
    notify_tx: RwLock<Option<UnboundedSender<String>>>,
    connect: ConnectFn,
    backoff_base: Duration,
    /// Bumped on every `reload`; connect tasks from an older generation
    /// stop touching state when they notice.
    generation: RwLock<u64>,
}

/// The production connect function.
pub fn real_connect() -> ConnectFn {
    Arc::new(|entry, changed| {
        Box::pin(async move {
            let c = McpClient::connect(entry, changed).await?;
            Ok(Arc::new(c) as Arc<dyn Conn>)
        })
    })
}

impl Hub {
    pub fn new(servers: Vec<ServerEntry>, connect: ConnectFn) -> Arc<Self> {
        Arc::new(Self::build(servers, connect, Duration::from_secs(1)))
    }

    /// Like `new` with a custom backoff base (tests use milliseconds).
    pub fn with_backoff_base(servers: Vec<ServerEntry>, connect: ConnectFn, base: Duration) -> Arc<Self> {
        Arc::new(Self::build(servers, connect, base))
    }

    fn build(servers: Vec<ServerEntry>, connect: ConnectFn, backoff_base: Duration) -> Self {
        let states = servers
            .iter()
            .map(|s| (s.name.clone(), if s.disabled { State::Disabled } else { State::Connecting }))
            .collect();
        let (changed_tx, _) = watch::channel(0);
        Self {
            servers: RwLock::new(servers),
            conns: RwLock::new(HashMap::new()),
            states: RwLock::new(states),
            table: RwLock::new(BTreeMap::new()),
            defs: RwLock::new(Vec::new()),
            changed_tx,
            notify_tx: RwLock::new(None),
            cache_path: RwLock::new(None),
            seeds: RwLock::new(HashMap::new()),
            connect,
            backoff_base,
            generation: RwLock::new(0),
        }
    }

    pub fn subscribe(&self) -> watch::Receiver<u64> {
        self.changed_tx.subscribe()
    }

    pub fn deny(&self, name: &str) {
        self.set_state(name, State::Denied);
    }

    pub fn mark_disabled(&self, name: &str) {
        self.set_state(name, State::Disabled);
    }

    /// Hold a server back until consent arrives (`start` skips it).
    pub fn mark_pending(&self, name: &str) {
        self.set_state(name, State::Pending);
    }

    /// Persist rebuilt defs for the next sidecar's dormant seeding.
    pub fn set_cache_path(&self, path: std::path::PathBuf) {
        *self.cache_path.write().unwrap() = Some(path);
    }

    /// Advertise cached tools without connecting: table + defs get the
    /// stored rows and the server goes [`State::Dormant`]. Only applies
    /// while the server is still `Connecting` — consent-pending or
    /// disabled servers are never seeded.
    pub fn seed_dormant(&self, server: &str, tools: &[crate::toolcache::CachedTool]) {
        if self.state_of(server) != Some(State::Connecting) {
            return;
        }
        {
            let mut table = self.table.write().unwrap();
            let mut defs = self.defs.write().unwrap();
            for t in tools {
                table.insert(t.mcp.clone(), (server.to_string(), t.raw.clone()));
                defs.push(t.def.clone());
            }
        }
        self.seeds.write().unwrap().insert(server.to_string(), tools.to_vec());
        self.set_state(server, State::Dormant);
    }

    fn set_state(&self, name: &str, state: State) {
        self.states.write().unwrap().insert(name.to_string(), state);
    }

    fn state_of(&self, name: &str) -> Option<State> {
        self.states.read().unwrap().get(name).cloned()
    }

    fn current_gen(&self) -> u64 {
        *self.generation.read().unwrap()
    }

    /// Spawn a connect task for every server still `Connecting` plus the
    /// listener that re-lists a server when it announces a tool change.
    pub async fn start(self: &Arc<Self>) {
        let (tx, mut rx) = mpsc::unbounded_channel::<String>();
        *self.notify_tx.write().unwrap() = Some(tx.clone());
        let generation = self.current_gen();
        let pending: Vec<ServerEntry> = self
            .servers
            .read()
            .unwrap()
            .iter()
            .filter(|s| self.state_of(&s.name) == Some(State::Connecting))
            .cloned()
            .collect();
        for entry in pending {
            let hub = Arc::clone(self);
            let tx = tx.clone();
            tokio::spawn(async move { hub.connect_loop(entry, tx, generation).await });
        }
        let hub = Arc::clone(self);
        tokio::spawn(async move {
            while let Some(name) = rx.recv().await {
                if hub.current_gen() != generation {
                    break;
                }
                log::info!("{name}: tool list changed");
                hub.rebuild().await;
            }
        });
    }

    /// Connect one server now (after consent or `/mcp allow`); a no-op for
    /// names that are not configured. Calls `start` first if it never ran.
    pub async fn start_server(self: &Arc<Self>, name: &str) {
        let entry = self.servers.read().unwrap().iter().find(|s| s.name == name).cloned();
        let Some(entry) = entry else { return };
        let tx = self.notify_tx.read().unwrap().clone();
        let tx = match tx {
            Some(tx) => tx,
            None => {
                self.set_state(name, State::Pending);
                self.start().await;
                self.notify_tx.read().unwrap().clone().expect("start sets notify_tx")
            }
        };
        self.set_state(name, State::Connecting);
        let hub = Arc::clone(self);
        let generation = self.current_gen();
        tokio::spawn(async move { hub.connect_loop(entry, tx, generation).await });
    }

    async fn connect_loop(self: Arc<Self>, entry: ServerEntry, tx: UnboundedSender<String>, generation: u64) {
        for attempt in 0..MAX_ATTEMPTS {
            if self.current_gen() != generation {
                return;
            }
            match (self.connect)(&entry, tx.clone()).await {
                Ok(conn) => {
                    if self.current_gen() != generation {
                        conn.close().await;
                        return;
                    }
                    self.conns.write().unwrap().insert(entry.name.clone(), conn);
                    self.set_state(&entry.name, State::Ready { tools: 0 });
                    self.rebuild().await;
                    return;
                }
                Err(e) => {
                    log::warn!("{}: connect failed (attempt {}): {e:#}", entry.name, attempt + 1);
                    self.set_state(&entry.name, State::Failed { error: format!("{e:#}") });
                    let delay = self.backoff_base.saturating_mul(1u32 << attempt.min(6)).min(MAX_BACKOFF);
                    tokio::time::sleep(delay).await;
                }
            }
        }
    }

    /// Re-list every connected server and rebuild table + defs; bumps the
    /// watch channel only when the model-facing defs actually changed.
    pub async fn rebuild(&self) {
        let order: Vec<String> = self.servers.read().unwrap().iter().map(|s| s.name.clone()).collect();
        let conns: Vec<(String, Arc<dyn Conn>)> = {
            let map = self.conns.read().unwrap();
            order.iter().filter_map(|n| map.get(n).map(|c| (n.clone(), Arc::clone(c)))).collect()
        };
        let mut taken = HashSet::new();
        let mut table = BTreeMap::new();
        let mut defs = Vec::new();
        for (server, conn) in conns {
            let tools = match conn.list_tools().await {
                Ok(t) => t,
                Err(e) => {
                    log::warn!("{server}: tools/list failed: {e:#}");
                    self.set_state(&server, State::Failed { error: format!("tools/list: {e:#}") });
                    continue;
                }
            };
            self.set_state(&server, State::Ready { tools: tools.len() });
            let mut records = Vec::with_capacity(tools.len());
            for t in tools {
                let name = names::tool_name(&server, &t.name, &mut taken);
                table.insert(name.clone(), (server.clone(), t.name.to_string()));
                let def = json!({
                    "name": name,
                    "description": t.description.as_deref().unwrap_or(""),
                    "parameters": names::fix_schema(Some(t.schema_as_json_value())),
                });
                records.push(crate::toolcache::CachedTool { mcp: name, raw: t.name.to_string(), def: def.clone() });
                defs.push(def);
            }
            // Persist this server's defs so the next sidecar can seed
            // dormant instead of spawning just to re-list tools.
            self.cache_store(&server, records);
        }
        // Still-dormant servers keep their seeded rows: rebuild rewrites
        // the table from live conns only, so without this a connected
        // server's rebuild would silently un-advertise every sleeper.
        for (server, tools) in self.seeds.read().unwrap().iter() {
            if self.state_of(server) == Some(State::Dormant) {
                for t in tools {
                    table.insert(t.mcp.clone(), (server.clone(), t.raw.clone()));
                    defs.push(t.def.clone());
                }
            }
        }
        *self.table.write().unwrap() = table;
        let changed = {
            let mut cur = self.defs.write().unwrap();
            if *cur == defs {
                false
            } else {
                *cur = defs;
                true
            }
        };
        if changed {
            self.changed_tx.send_modify(|v| *v += 1);
        }
    }

    /// Write one server's rebuilt defs to the tool cache (merge into the
    /// existing file; other servers' entries are preserved).
    fn cache_store(&self, server: &str, tools: Vec<crate::toolcache::CachedTool>) {
        let Some(path) = self.cache_path.read().unwrap().clone() else {
            return;
        };
        let entry = self.servers.read().unwrap().iter().find(|s| s.name == server).cloned();
        let Some(entry) = entry else { return };
        let mut cache = crate::toolcache::ToolCache::load(&path);
        cache.set(server, crate::toolcache::identity(&entry), tools);
        cache.save(&path);
    }

    pub fn tools(&self) -> Vec<Value> {
        self.defs.read().unwrap().clone()
    }

    /// Route a model tool call; every failure becomes an `is_error` reply.
    pub async fn call(self: &Arc<Self>, mcp_name: &str, args: Value) -> Value {
        let Some((server, tool)) = self.table.read().unwrap().get(mcp_name).cloned() else {
            return error_reply(format!("unknown tool {mcp_name}"));
        };
        // Dormant servers spawn on first use: `start_server` kicks the
        // connect loop, then we wait for Ready/Failed before the call.
        if self.conns.read().unwrap().get(&server).is_none() {
            if self.state_of(&server) == Some(State::Dormant) {
                self.start_server(&server).await;
            }
            let wait_until = tokio::time::Instant::now() + Duration::from_secs(30);
            loop {
                match self.state_of(&server) {
                    Some(State::Ready { .. }) => break,
                    Some(State::Failed { .. }) | Some(State::Denied) | Some(State::Disabled) => {
                        return error_reply(format!("{server}: not connected"));
                    }
                    _ => {}
                }
                if tokio::time::Instant::now() >= wait_until {
                    return error_reply(format!("{server}: connect timed out"));
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        }
        let conn = self.conns.read().unwrap().get(&server).cloned();
        let Some(conn) = conn else {
            return error_reply(format!("{server}: not connected"));
        };
        let timeout = self
            .servers
            .read()
            .unwrap()
            .iter()
            .find(|s| s.name == server)
            .map(|s| s.timeout)
            .unwrap_or(Duration::from_secs(crate::config::DEFAULT_TIMEOUT_SECS));
        match conn.call(&tool, args, timeout).await {
            Ok(r) => convert::to_reply(&r),
            Err(e) => error_reply(format!("{server}/{tool}: {e:#}")),
        }
    }

    /// `(name, state)` in config order.
    pub fn status(&self) -> Vec<(String, State)> {
        let states = self.states.read().unwrap();
        self.servers
            .read()
            .unwrap()
            .iter()
            .map(|s| (s.name.clone(), states.get(&s.name).cloned().unwrap_or(State::Connecting)))
            .collect()
    }

    /// Close everything, swap in `servers`, and start again. Servers in
    /// `pending` stay `Pending` (consent gate) instead of connecting.
    pub async fn reload(self: &Arc<Self>, servers: Vec<ServerEntry>, pending: &[String]) {
        *self.generation.write().unwrap() += 1;
        *self.notify_tx.write().unwrap() = None;
        let old: Vec<Arc<dyn Conn>> = self.conns.write().unwrap().drain().map(|(_, c)| c).collect();
        for c in old {
            c.close().await;
        }
        {
            let mut states = self.states.write().unwrap();
            states.clear();
            for s in &servers {
                let state = if pending.contains(&s.name) {
                    State::Pending
                } else if s.disabled {
                    State::Disabled
                } else {
                    State::Connecting
                };
                states.insert(s.name.clone(), state);
            }
            *self.servers.write().unwrap() = servers;
            self.seeds.write().unwrap().clear();
        }
        self.rebuild().await;
        self.start().await;
    }
}

fn error_reply(msg: String) -> Value {
    json!({ "content": msg, "is_error": true })
}

#[cfg(test)]
#[path = "hub_tests.rs"]
mod hub_tests;
