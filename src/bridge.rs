//! 桥 Daemon —— 全机单例，坐在 agent 和 Chrome 扩展中间。
//!
//! 职责边界（与原版写死的一致，别扩）：
//!   路由 cmd/res、维护扩展连接、站点白名单裁决、审计落盘。
//!   它不解析页面、不认识 ref、不知道 snapshot 是什么——那些全在扩展侧。
//!
//! 安全边界就在握手前的 Origin 校验：浏览器发起 WS 时强制带 Origin 且不可伪造，
//! 网页的 Origin 是自己的域名，扩展的是 chrome-extension://<id>。只放后者进来。
use crate::compat;
use crate::paths::{self, BridgeInfo, VERSION};
use crate::redact;
use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::net::TcpListener;
use tokio::sync::{broadcast, mpsc, oneshot};
use tokio_tungstenite::accept_hdr_async;
use tungstenite::handshake::server::{Callback, ErrorResponse, Request, Response};

const HELLO_TIMEOUT: Duration = Duration::from_secs(5);
const CMD_TIMEOUT: Duration = Duration::from_secs(30);
const IDLE_EXIT: Duration = Duration::from_secs(30 * 60);
const EXT_SILENCE: Duration = Duration::from_secs(50);
const EXT_PROBE: Duration = Duration::from_secs(15);
const ORPHAN_GRACE: Duration = Duration::from_secs(15);
const WAIT_CAP: usize = 64;
const WAIT_MAX: Duration = Duration::from_secs(40);

const NO_EXT_MSG: &str = "扩展没连上桥。桥已经替你等过一轮（最多 40s），它还没回来。\
让用户点一下浏览器工具栏的 huashu-chrome 图标 → 「重连」（插件没消失，只是这条连接断了）；\
Chrome 没开就先开；改过扩展代码才需要去 chrome://extensions 重载。";

type ConnId = u64;

#[derive(Clone)]
pub struct Peer {
    tx: mpsc::UnboundedSender<tokio_tungstenite::tungstenite::Message>,
    close_tx: Arc<Mutex<Option<oneshot::Sender<()>>>>,
    kind: PeerKind,
    state: Arc<Mutex<PeerState>>,
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum PeerKind {
    Agent,
    Extension,
}

pub struct PeerState {
    pub helloed: bool,
    pub client: Option<String>,
    pub label: Option<String>,
    pub sid: Option<String>,
    pub instance_id: Option<String>,
    pub ext_version: Option<String>,
    pub ext_protocol: Option<u32>,
    pub chrome_version: Option<String>,
    pub headless: bool,
    pub last_rx: Instant,
    pub probed_at: Option<Instant>,
    pub closed: bool,
}

impl Default for PeerState {
    fn default() -> Self {
        Self {
            helloed: false,
            client: None,
            label: None,
            sid: None,
            instance_id: None,
            ext_version: None,
            ext_protocol: None,
            chrome_version: None,
            headless: false,
            last_rx: Instant::now(),
            probed_at: None,
            closed: false,
        }
    }
}

impl Peer {
    pub fn send(&self, v: &Value) {
        let _ = self.tx.send(tokio_tungstenite::tungstenite::Message::Text(v.to_string().into()));
    }
    /// 带自定义码的 close 帧（hello 失败：4000/4001/4003/4008）
    pub fn close_code(&self, code: u16, reason: &str) {
        use tungstenite::protocol::frame::coding::CloseCode;
        use tungstenite::protocol::CloseFrame;
        let _ = self.tx.send(tokio_tungstenite::tungstenite::Message::Close(Some(CloseFrame {
            code: CloseCode::from(code),
            reason: reason.to_string().into(),
        })));
    }
    pub fn close(&self) {
        if let Some(tx) = self.close_tx.lock().unwrap().take() {
            let _ = tx.send(());
        }
    }
    pub fn with<F: FnOnce(&mut PeerState)>(&self, f: F) {
        f(&mut self.state.lock().unwrap());
    }
    pub fn get<F: FnOnce(&PeerState) -> T, T>(&self, f: F) -> T {
        f(&self.state.lock().unwrap())
    }
    pub fn same(&self, o: &Peer) -> bool {
        Arc::ptr_eq(&self.state, &o.state)
    }
}

struct Pending {
    agent: Peer,
    ext: Peer,
    cmd: String,
    id: String,
    started_at: Instant,
    timer: tokio::task::JoinHandle<()>,
    orphan: Option<tokio::task::JoinHandle<()>>,
}

struct Waiting {
    agent: Peer,
    msg: Value,
    timer: tokio::task::JoinHandle<()>,
}

pub struct Bridge {
    pub agents: Mutex<HashSet<ConnId>>,
    pub peers: Mutex<HashMap<ConnId, Peer>>,
    pub pending: Mutex<HashMap<String, Pending>>,
    pub waiting: Mutex<Vec<Waiting>>,
    conn_seq: AtomicU64,
    last_activity: Mutex<Instant>,
    pub token: String,
    pub port: u16,
    pub write_info: bool,
    agent_left: broadcast::Sender<ConnId>,
}

impl Bridge {
    pub fn new(port: u16, token: String, write_info: bool) -> Arc<Self> {
        let (tx, _rx) = broadcast::channel(64);
        Arc::new(Self {
            agents: Mutex::new(HashSet::new()),
            peers: Mutex::new(HashMap::new()),
            pending: Mutex::new(HashMap::new()),
            waiting: Mutex::new(Vec::new()),
            conn_seq: AtomicU64::new(0),
            last_activity: Mutex::new(Instant::now()),
            token,
            port,
            write_info,
            agent_left: tx,
        })
    }

    pub fn live_extensions(&self) -> Vec<Peer> {
        self.peers
            .lock()
            .unwrap()
            .values()
            .filter(|p| p.kind == PeerKind::Extension && !p.get(|s| s.closed))
            .cloned()
            .collect()
    }

    pub fn live_agents(&self) -> Vec<Peer> {
        let ids = self.agents.lock().unwrap();
        let peers = self.peers.lock().unwrap();
        ids.iter().filter_map(|id| peers.get(id).cloned()).collect()
    }

    pub fn live_sessions(&self) -> Vec<String> {
        let mut v: Vec<String> = self.live_agents().iter().filter_map(|p| p.get(|s| s.sid.clone())).collect();
        v.sort();
        v.dedup();
        v
    }

    /// 命令发给谁：有窗口的实例优先，同档取最近有动静的
    pub fn primary(&self) -> Option<Peer> {
        let live = self.live_extensions();
        if live.is_empty() {
            return None;
        }
        let headed: Vec<_> = live.iter().filter(|p| !p.get(|s| s.headless)).cloned().collect();
        let pool = if !headed.is_empty() { headed } else { live };
        pool.into_iter().max_by_key(|p| p.get(|s| s.last_rx))
    }

    fn broadcast_agents(&self, v: &Value) {
        for a in self.live_agents() {
            a.send(v);
        }
    }

    fn log(&self, m: &str) {
        println!("[{}] {}", paths::now_clock(), m);
    }

    fn ext_label(p: &Peer) -> String {
        let (cv, hl, ev) = p.get(|s| (s.chrome_version.clone(), s.headless, s.ext_version.clone()));
        format!(
            "Chrome {}{} · 扩展 {}",
            cv.unwrap_or_else(|| "?".into()),
            if hl { " · headless" } else { "" },
            ev.unwrap_or_else(|| "?".into())
        )
    }

    pub async fn run(self: &Arc<Self>) -> anyhow::Result<()> {
        paths::ensure_home();
        let listener = TcpListener::bind(("127.0.0.1", self.port)).await?;
        if self.write_info {
            paths::write_bridge_info(&BridgeInfo {
                port: self.port,
                token: self.token.clone(),
                pid: std::process::id(),
                version: VERSION.to_string(),
                started_at: Some(paths::now_iso()),
            })?;
        }
        self.log(&format!("桥已启动 ws://127.0.0.1:{}", self.port));

        // 空闲自杀 + 扩展静默探活（先 ping 再杀，睡醒的机器不该被误判）
        let me = self.clone();
        tokio::spawn(async move {
            let mut iv = tokio::time::interval(Duration::from_secs(20));
            loop {
                iv.tick().await;
                let now = Instant::now();
                for e in me.live_extensions() {
                    let (silent, probed) = e.get(|s| (now.duration_since(s.last_rx), s.probed_at));
                    if silent <= EXT_SILENCE {
                        e.with(|s| s.probed_at = None);
                        continue;
                    }
                    match probed {
                        None => {
                            e.with(|s| s.probed_at = Some(now));
                            e.send(&json!({"type": "ping"}));
                        }
                        Some(at) if now.duration_since(at) >= EXT_PROBE => {
                            me.log(&format!("扩展静默超时，探了 {}s 也没回音，判定这条连接已死（{}）", now.duration_since(at).as_secs(), Self::ext_label(&e)));
                            e.close();
                        }
                        Some(_) => {}
                    }
                }
                let idle = {
                    let la = *me.last_activity.lock().unwrap();
                    me.agents.lock().unwrap().is_empty() && me.live_extensions().is_empty() && now.duration_since(la) > IDLE_EXIT
                };
                if me.write_info && idle {
                    me.log("空闲超时，桥自行退出");
                    std::process::exit(0);
                }
            }
        });

        // agent 断线 → 向每个扩展实例推 sessions 名单（摘页面标记用）
        let me2 = self.clone();
        let mut left = self.agent_left.subscribe();
        tokio::spawn(async move {
            while let Ok(_conn) = left.recv().await {
                let live = me2.live_sessions();
                for e in me2.live_extensions() {
                    e.send(&json!({"type": "event", "event": "sessions", "live": live}));
                }
            }
        });

        loop {
            let (stream, _) = listener.accept().await?;
            let me = self.clone();
            tokio::spawn(async move {
                let _ = me.handle_conn(stream).await;
            });
        }
    }

    async fn handle_conn(self: Arc<Self>, stream: tokio::net::TcpStream) -> anyhow::Result<()> {
        let origin_holder: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
        let oh = origin_holder.clone();
        struct Cb {
            oh: Arc<Mutex<Option<String>>>,
        }
        impl Callback for Cb {
            fn on_request(self, request: &Request, response: Response) -> Result<Response, ErrorResponse> {
                let o = request.headers().get("Origin").map(|v| v.to_str().unwrap_or("").to_string());
                match &o {
                    Some(origin) if origin.starts_with("chrome-extension://") => {
                        *self.oh.lock().unwrap() = Some(origin.clone());
                    }
                    Some(origin) => {
                        // 网页想连桥——这就是攻击面，堵在这里
                        paths::audit(json!({"ev": "reject_origin", "origin": origin}));
                        let resp: ErrorResponse = Response::builder()
                            .status(403)
                            .body(Some("forbidden origin".to_string()))
                            .expect("build 403 response");
                        return Err(resp);
                    }
                    None => {} // Node/Rust 侧客户端不发 Origin，进握手后验 token
                }
                Ok(response)
            }
        }
        let ws = accept_hdr_async(stream, Cb { oh }).await?;
        let is_extension = origin_holder.lock().unwrap().as_ref().map(|o| o.starts_with("chrome-extension://")).unwrap_or(false);

        let conn_id = self.conn_seq.fetch_add(1, Ordering::SeqCst) + 1;
        let (tx, mut rx) = mpsc::unbounded_channel::<tokio_tungstenite::tungstenite::Message>();
        let (close_tx, mut close_rx) = oneshot::channel::<()>();
        let peer = Peer {
            tx,
            close_tx: Arc::new(Mutex::new(Some(close_tx))),
            kind: if is_extension { PeerKind::Extension } else { PeerKind::Agent },
            state: Arc::new(Mutex::new(PeerState { last_rx: Instant::now(), ..Default::default() })),
        };
        self.peers.lock().unwrap().insert(conn_id, peer.clone());

        let (mut ws_sink, mut ws_stream) = ws.split();
        let pump = tokio::spawn(async move {
            loop {
                tokio::select! {
                    biased;
                    item = rx.recv() => {
                        match item {
                            Some(m) => {
                                let is_close = matches!(m, tokio_tungstenite::tungstenite::Message::Close(_));
                                if ws_sink.send(m).await.is_err() { break; }
                                if is_close { break; } // close 帧发完即退，收口不等它
                            }
                            None => break,
                        }
                    }
                    _ = &mut close_rx => break,
                }
            }
            let _ = ws_sink.close().await;
        });

        let hello_peer = peer.clone();
        let hello_timer = tokio::spawn(async move {
            tokio::time::sleep(HELLO_TIMEOUT).await;
            if !hello_peer.get(|s| s.helloed) {
                hello_peer.close_code(4008, "hello timeout");
            }
        });

        while let Some(msg) = ws_stream.next().await {
            let msg = match msg {
                Ok(m) => m,
                Err(_) => break,
            };
            if msg.is_close() {
                break;
            }
            let text = match msg.into_text() {
                Ok(t) => t.to_string(),
                Err(_) => continue,
            };
            *self.last_activity.lock().unwrap() = Instant::now();
            peer.with(|s| s.last_rx = Instant::now());
            let v: Value = match serde_json::from_str(&text) {
                Ok(v) => v,
                Err(_) => {
                    peer.send(&json!({"type": "res", "ok": false, "error": {"code": "INTERNAL", "message": "非法 JSON"}}));
                    continue;
                }
            };
            if !peer.get(|s| s.helloed) {
                hello_timer.abort();
                if let Err(code) = self.handle_hello(conn_id, &peer, &v) {
                    peer.close_code(code, "hello rejected");
                    break;
                }
                continue;
            }
            self.route(&peer, &v);
        }

        // ——— 收口 ———
        hello_timer.abort();
        peer.close();
        peer.with(|s| s.closed = true);
        let was_agent = self.agents.lock().unwrap().remove(&conn_id);
        self.peers.lock().unwrap().remove(&conn_id);
        if was_agent {
            self.waiting.lock().unwrap().retain(|it| !it.agent.same(&peer));
            let _ = self.agent_left.send(conn_id);
        }
        if peer.kind == PeerKind::Extension {
            self.log(&format!("扩展断开（{}）", Self::ext_label(&peer)));
            // 在途命令宽限：断线多半是瞬断，扩展带着回执回来照常按 __k 配对
            let keys: Vec<String> = self
                .pending
                .lock()
                .unwrap()
                .iter()
                .filter(|(_, p)| p.ext.same(&peer) && p.orphan.is_none())
                .map(|(k, _)| k.clone())
                .collect();
            for k in keys {
                let me = self.clone();
                let key = k.clone();
                let h = tokio::spawn(async move {
                    tokio::time::sleep(ORPHAN_GRACE).await;
                    let p = me.pending.lock().unwrap().remove(&key);
                    if let Some(p) = p {
                        p.timer.abort();
                        p.agent.send(&json!({
                            "type": "res", "id": p.id, "ok": false,
                            "error": {"code": "NO_EXTENSION",
                              "message": format!("扩展在命令执行中断开，{} 秒内没回来。这条命令可能已经在页面上生效了——重试前先 snapshot 看一眼，别把一次点击做成两次。", ORPHAN_GRACE.as_secs())}
                        }));
                    }
                });
                if let Some(p) = self.pending.lock().unwrap().get_mut(&k) {
                    p.orphan = Some(h);
                }
            }
            if self.live_extensions().is_empty() {
                self.broadcast_agents(&json!({"type": "event", "event": "extension_offline"}));
            }
        }
        // 给泵一点时间把 close 帧发出去（hello 被拒的路径靠它传达 4001/4003/4008）
        let _ = tokio::time::timeout(Duration::from_millis(300), pump).await;
        Ok(())
    }

    fn handle_hello(self: &Arc<Self>, conn_id: ConnId, peer: &Peer, msg: &Value) -> Result<(), u16> {
        if msg.get("type").and_then(|t| t.as_str()) != Some("hello") {
            return Err(4000);
        }
        let role = msg.get("role").and_then(|r| r.as_str()).unwrap_or("");
        match (role, peer.kind) {
            ("extension", PeerKind::Extension) => {
                peer.with(|s| {
                    s.helloed = true;
                    s.ext_version = msg.get("version").and_then(|v| v.as_str()).map(|x| x.to_string());
                    s.ext_protocol = msg.get("v").and_then(|v| v.as_u64()).map(|x| x as u32);
                    s.chrome_version = msg.get("chrome").and_then(|v| v.as_str()).map(|x| x.to_string());
                    s.headless = msg.get("headless").and_then(|v| v.as_bool()).unwrap_or(false);
                    s.instance_id = Some(msg.get("instanceId").and_then(|v| v.as_str()).map(|x| x.to_string()).unwrap_or_else(|| {
                        format!("ext:{}", msg.get("extId").and_then(|v| v.as_str()).unwrap_or("?"))
                    }));
                });
                // 同一个实例又连了一条：断线重连或重载扩展，只顶同实例的
                let my_id = peer.get(|s| s.instance_id.clone());
                let olds: Vec<Peer> = self
                    .live_extensions()
                    .into_iter()
                    .filter(|e| e.get(|s| s.instance_id == my_id) && !e.same(peer))
                    .collect();
                for o in olds {
                    o.close();
                }
                let level = {
                    let (ev, ep) = peer.get(|s| (s.ext_version.clone(), s.ext_protocol));
                    compat::compat(ev.as_deref().unwrap_or(""), VERSION, ep.unwrap_or(compat::PROTOCOL), compat::PROTOCOL)
                };
                if level != "same" && level != "patch" {
                    let (ev, ep) = peer.get(|s| (s.ext_version.clone(), s.ext_protocol));
                    self.log(&format!("⚠️  {}", compat::compat_note(level, ev.as_deref().unwrap_or("?"), VERSION)));
                    let _ = ep;
                }
                self.log(&format!("扩展已连接（{}）", Self::ext_label(peer)));
                let live = self.live_extensions();
                if live.len() > 1 {
                    if let Some(p) = self.primary() {
                        self.log(&format!("⚠️  当前有 {} 个 Chrome 实例连着，命令路由到：{}", live.len(), Self::ext_label(&p)));
                    }
                }
                peer.send(&json!({"type": "welcome", "bridge": VERSION, "v": compat::PROTOCOL}));
                self.broadcast_agents(&json!({"type": "event", "event": "extension_online"}));
                self.flush_waiting();
                Ok(())
            }
            ("agent", PeerKind::Agent) => {
                let tok = msg.get("token").and_then(|t| t.as_str()).unwrap_or("");
                if !paths::token_equals(tok, &self.token) {
                    paths::audit(json!({"ev": "reject_token", "client": msg.get("client")}));
                    return Err(4001);
                }
                peer.with(|s| {
                    s.helloed = true;
                    s.client = Some(msg.get("client").and_then(|v| v.as_str()).unwrap_or("unknown").to_string());
                    s.label = msg.get("label").and_then(|v| v.as_str()).map(|x| x.chars().take(40).collect());
                    s.sid = Some(msg.get("sessionId").and_then(|v| v.as_str()).map(|x| x.to_string()).unwrap_or_else(|| format!("conn:{}", conn_id)));
                });
                self.agents.lock().unwrap().insert(conn_id);
                let (label, client, sid) = peer.get(|s| (s.label.clone(), s.client.clone(), s.sid.clone()));
                self.log(&format!(
                    "agent 已连接：{}（{}，会话 {}）",
                    label.clone().unwrap_or_else(|| client.clone().unwrap_or_default()),
                    client.clone().unwrap_or_default(),
                    sid.clone().unwrap_or_default()
                ));
                let ext = self.primary();
                let exts: Vec<Value> = self
                    .live_extensions()
                    .iter()
                    .map(|e| {
                        let is_primary = ext.as_ref().map(|p| p.same(e)).unwrap_or(false);
                        json!({
                            "chrome": e.get(|s| s.chrome_version.clone()),
                            "version": e.get(|s| s.ext_version.clone()),
                            "headless": e.get(|s| s.headless),
                            "primary": is_primary,
                        })
                    })
                    .collect();
                let (ext_online, ext_ver, ext_proto) = match &ext {
                    Some(e) => (true, e.get(|s| s.ext_version.clone()), e.get(|s| s.ext_protocol)),
                    None => (false, None, None),
                };
                let mismatch = match &ext {
                    Some(e) => {
                        let (ev, ep) = e.get(|s| (s.ext_version.clone(), s.ext_protocol));
                        let lvl = compat::compat(ev.as_deref().unwrap_or(""), VERSION, ep.unwrap_or(compat::PROTOCOL), compat::PROTOCOL);
                        lvl != "same" && lvl != "patch"
                    }
                    None => false,
                };
                peer.send(&json!({
                    "type": "welcome",
                    "bridge": VERSION,
                    "v": compat::PROTOCOL,
                    "extensionOnline": ext_online,
                    "extensionVersion": ext_ver,
                    "versionMismatch": mismatch,
                    "extensionProtocol": ext_proto,
                    "extensions": exts,
                }));
                Ok(())
            }
            _ => Err(4003),
        }
    }

    fn route(self: &Arc<Self>, peer: &Peer, msg: &Value) {
        match msg.get("type").and_then(|t| t.as_str()).unwrap_or("") {
            "cmd" => {
                if peer.kind != PeerKind::Agent {
                    return;
                }
                match self.primary() {
                    Some(target) => self.dispatch(peer, msg, target),
                    None => self.enqueue(peer, msg),
                }
            }
            "ping" => peer.send(&json!({"type": "pong"})),
            "pong" => {}
            "res" => {
                if peer.kind == PeerKind::Extension {
                    self.route_back(peer, msg);
                }
            }
            "event" => {
                if peer.kind == PeerKind::Extension {
                    self.broadcast_agents(msg);
                }
            }
            _ => {}
        }
    }

    fn enqueue(self: &Arc<Self>, agent: &Peer, msg: &Value) {
        let id = msg.get("id").and_then(|i| i.as_str()).unwrap_or("?").to_string();
        let cmd = msg.get("cmd").and_then(|c| c.as_str()).unwrap_or("?").to_string();
        let client = agent.get(|s| s.client.clone());
        let mut w = self.waiting.lock().unwrap();
        if w.len() >= WAIT_CAP {
            agent.send(&json!({"type": "res", "id": id, "ok": false, "error": {"code": "NO_EXTENSION", "message": NO_EXT_MSG}}));
            return;
        }
        let ms = msg
            .get("timeout")
            .and_then(|t| t.as_u64())
            .map(Duration::from_millis)
            .unwrap_or(WAIT_MAX)
            .min(WAIT_MAX);
        let a2 = agent.clone();
        let id2 = id.clone();
        let timer = tokio::spawn(async move {
            tokio::time::sleep(ms).await;
            a2.send(&json!({"type": "res", "id": id2, "ok": false, "error": {"code": "NO_EXTENSION", "message": NO_EXT_MSG}}));
        });
        paths::audit(json!({"ev": "queued", "cmd": cmd, "client": client, "waitMs": ms.as_millis() as u64}));
        w.push(Waiting { agent: agent.clone(), msg: msg.clone(), timer });
    }

    fn flush_waiting(self: &Arc<Self>) {
        let q: Vec<Waiting> = self.waiting.lock().unwrap().drain(..).collect();
        let n = q.len();
        for it in q {
            it.timer.abort();
            if !it.agent.get(|s| s.closed) {
                if let Some(target) = self.primary() {
                    self.dispatch(&it.agent, &it.msg, target);
                }
            }
        }
        if n > 0 {
            self.log(&format!("扩展回来了，补发 {} 条排队的命令", n));
        }
    }

    fn dispatch(self: &Arc<Self>, agent: &Peer, msg: &Value, target: Peer) {
        let id = msg.get("id").and_then(|i| i.as_str()).unwrap_or("?").to_string();
        let cmd = msg.get("cmd").and_then(|c| c.as_str()).unwrap_or("?").to_string();

        if let Some(gate) = self.check_site(msg) {
            paths::audit(json!({"ev": "blocked", "cmd": cmd, "client": agent.get(|s| s.client.clone()), "reason": gate["code"].clone()}));
            agent.send(&json!({"type": "res", "id": id, "ok": false, "error": gate}));
            return;
        }

        let ms = msg
            .get("timeout")
            .and_then(|t| t.as_u64())
            .map(Duration::from_millis)
            .unwrap_or(CMD_TIMEOUT)
            .clamp(Duration::from_secs(1), Duration::from_secs(600));

        let conn_id = {
            let peers = self.peers.lock().unwrap();
            peers.iter().find(|(_, p)| p.same(agent)).map(|(i, _)| *i).unwrap_or(0)
        };
        let key = format!("{}:{}", conn_id, id);

        // 超时兜底：到点从 pending 摘掉并回错
        {
            let me = self.clone();
            let key2 = key.clone();
            let a2 = agent.clone();
            let id2 = id.clone();
            let timer = tokio::spawn(async move {
                tokio::time::sleep(ms).await;
                let p = me.pending.lock().unwrap().remove(&key2);
                if let Some(p) = p {
                    if let Some(o) = p.orphan {
                        o.abort();
                    }
                    a2.send(&json!({"type": "res", "id": id2, "ok": false, "error": {"code": "TIMEOUT", "message": format!("扩展 {}s 未响应", ms.as_secs())}}));
                }
            });
            let (sid, client, label) = agent.get(|s| (s.sid.clone(), s.client.clone(), s.label.clone()));
            paths::audit(json!({
                "ev": "cmd", "id": key, "cmd": cmd, "client": client, "sid": sid,
                "params": redact::redact(msg.get("params").unwrap_or(&Value::Null))
            }));
            let mut fwd = msg.clone();
            if let Some(o) = fwd.as_object_mut() {
                o.insert("__k".into(), json!(key.clone()));
                o.insert("sid".into(), json!(sid));
                o.insert("client".into(), json!(client));
                o.insert("label".into(), json!(label));
                o.insert("live".into(), json!(self.live_sessions()));
            }
            target.send(&fwd);
            self.pending.lock().unwrap().insert(
                key,
                Pending { agent: agent.clone(), ext: target, cmd, id, started_at: Instant::now(), timer, orphan: None },
            );
        }
    }

    fn route_back(&self, ext: &Peer, msg: &Value) {
        let key = msg.get("__k").and_then(|k| k.as_str()).map(|s| s.to_string());
        let id = msg.get("id").and_then(|i| i.as_str()).unwrap_or("?").to_string();
        let k = {
            let pend = self.pending.lock().unwrap();
            match &key {
                Some(k) if pend.contains_key(k) => Some(k.clone()),
                _ => pend.iter().find(|(_, p)| p.id == id).map(|(k, _)| k.clone()),
            }
        };
        let k = match k {
            Some(k) => k,
            None => return, // 已超时，丢弃
        };
        let p = self.pending.lock().unwrap().remove(&k).unwrap();
        p.timer.abort();
        if let Some(o) = p.orphan {
            o.abort();
        }
        let ok = msg.get("ok").and_then(|o| o.as_bool()).unwrap_or(false);
        let data = msg.get("data").cloned().unwrap_or(Value::Null);
        paths::audit(json!({
            "ev": "res", "id": k, "cmd": p.cmd, "ok": ok,
            "ms": p.started_at.elapsed().as_millis() as u64,
            "error": msg.get("error").and_then(|e| e.get("code")).cloned().unwrap_or(Value::Null),
            "bytes": if ok { json!(data.to_string().len()) } else { Value::Null },
            "effect": if ok { redact::effect_of(&data).map(|s| json!(s)).unwrap_or(Value::Null) } else { Value::Null },
        }));
        let mut clean = msg.clone();
        if let Some(o) = clean.as_object_mut() {
            o.remove("__k");
        }
        p.agent.send(&clean);
        let _ = ext;
    }

    fn check_site(&self, msg: &Value) -> Option<Value> {
        let url = msg.get("params")?.get("url")?.as_str()?;
        let raw = std::fs::read_to_string(paths::allowlist_file()).ok()?;
        let list: Value = serde_json::from_str(&raw).ok()?;
        if list.get("mode").and_then(|m| m.as_str()) != Some("allowlist") {
            return None;
        }
        let host = url::Url::parse(url).ok()?.host_str()?.to_string();
        let sites = list.get("sites").and_then(|s| s.as_array()).cloned().unwrap_or_default();
        let ok = sites.iter().any(|s| {
            let s = s.as_str().unwrap_or("");
            host == s || host.ends_with(&format!(".{}", s))
        });
        if ok {
            None
        } else {
            Some(json!({"code": "SITE_NOT_ALLOWED", "message": format!("{} 不在授权站点列表里，请在扩展弹窗里授权", host)}))
        }
    }
}
