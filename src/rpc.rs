//! MCP server / CLI 侧的桥客户端：确保桥活着（不在就拉起），连上，发 cmd 收 res。
//! 与原版 src/lib/rpc.js 行为对齐：排它文件锁收敛启动权，版本不符当场换桥。
use crate::paths::{self, BridgeInfo, DEFAULT_PORT, VERSION};
use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::net::TcpStream;
use tokio::sync::Mutex as AsyncMutex;
use tokio_tungstenite::{connect_async, MaybeTlsStream, WebSocketStream};
use tungstenite::Message;

/// 会话身份：扩展用它认领「我的受控标签页」，必须比桥活得久。
/// 用父进程 pid：同一个 agent 窗口里 MCP server 重启也不变，不同窗口天然互异。
pub fn make_session_id(client: &str) -> String {
    let ppid = unsafe { libc::getppid() };
    if ppid <= 1 {
        let n = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .subsec_nanos();
        format!("{}:r{:x}", client, n)
    } else {
        format!("{}:p{}", client, ppid)
    }
}

type Ws = WebSocketStream<MaybeTlsStream<TcpStream>>;
type Sink = futures_util::stream::SplitSink<Ws, Message>;

#[derive(Debug, Clone)]
pub struct ClientError {
    pub code: String,
    pub message: String,
}
impl std::fmt::Display for ClientError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "[{}] {}", self.code, self.message)
    }
}
impl std::error::Error for ClientError {}

pub struct BridgeClient {
    pub client: String,
    pub label: Option<String>,
    pub session_id: String,
    pinned_sid: bool,
    inner: Arc<Inner>,
}

struct Inner {
    sink: AsyncMutex<Option<Arc<AsyncMutex<Sink>>>>,
    connected: Mutex<bool>,
    seq: Mutex<u64>,
    waiting: Mutex<HashMap<String, tokio::sync::oneshot::Sender<Result<Value, ClientError>>>>,
    ext: Mutex<ExtState>,
}

#[derive(Default, Clone)]
pub struct ExtState {
    pub online: bool,
    pub version: Option<String>,
    pub mismatch: bool,
}

impl BridgeClient {
    pub fn new(client: &str, session_id: Option<String>, label: Option<String>) -> Self {
        Self {
            client: client.to_string(),
            label,
            session_id: session_id.clone().unwrap_or_else(|| make_session_id(client)),
            pinned_sid: session_id.is_some(),
            inner: Arc::new(Inner {
                sink: AsyncMutex::new(None),
                connected: Mutex::new(false),
                seq: Mutex::new(0),
                waiting: Mutex::new(HashMap::new()),
                ext: Mutex::new(ExtState::default()),
            }),
        }
    }

    /// 宿主的真名要到 MCP initialize 之后才知道；连上桥之前允许改口。
    pub fn identify(&mut self, client: &str, label: Option<String>) -> bool {
        if *self.inner.connected.lock().unwrap() {
            return false;
        }
        self.client = client.to_string();
        self.label = label;
        if !self.pinned_sid {
            self.session_id = make_session_id(client);
        }
        true
    }

    pub fn ext_state(&self) -> ExtState {
        self.inner.ext.lock().unwrap().clone()
    }

    pub fn is_connected(&self) -> bool {
        *self.inner.connected.lock().unwrap()
    }

    pub async fn connect(&self, timeout: Duration) -> Result<(), ClientError> {
        let deadline = tokio::time::Instant::now() + timeout;
        let mut spawned = false;
        while tokio::time::Instant::now() < deadline {
            let info = paths::read_bridge_info();
            if let Some(info) = &info {
                // 桥是长驻单例，不读磁盘代码：版本对不上就当场换掉。
                // 但只换**同类**桥——Node 原版桥和 Rust 桥协议兼容，共存合法，
                // 互相踢会变成换代战争（两边都认为对方版本不对）。
                let same_kind = info.kind.as_deref() == Some("rust");
                if same_kind && info.version != VERSION && !spawned {
                    stop_bridge(info);
                    spawned = try_start_bridge();
                    tokio::time::sleep(Duration::from_millis(400)).await;
                    continue;
                }
                if self.open(info).await.is_ok() {
                    return Ok(());
                }
            }
            if !spawned {
                spawned = try_start_bridge();
            }
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
        Err(ClientError {
            code: "INTERNAL".into(),
            message: "连不上桥。跑 `linai-chrome-rs doctor` 看看哪儿卡住了".into(),
        })
    }

    async fn open(&self, info: &BridgeInfo) -> Result<(), ClientError> {
        // 原生 WS 客户端不发 Origin 头 —— 桥正是靠这一点区分 agent 和扩展
        let url = format!("ws://127.0.0.1:{}", if info.port == 0 { DEFAULT_PORT } else { info.port });
        let (ws, _) = connect_async(&url)
            .await
            .map_err(|e| ClientError { code: "INTERNAL".into(), message: format!("桥连接失败: {}", e) })?;
        let (mut sink, mut stream) = ws.split();
        let hello = json!({
            "type": "hello", "role": "agent", "token": info.token,
            "client": self.client, "label": self.label, "sessionId": self.session_id, "v": 1
        });
        sink.send(Message::Text(hello.to_string().into()))
            .await
            .map_err(|e| ClientError { code: "INTERNAL".into(), message: e.to_string() })?;

        let welcome = tokio::time::timeout(Duration::from_secs(4), async {
            while let Some(m) = stream.next().await {
                let m = match m {
                    Ok(m) => m,
                    Err(_) => return None,
                };
                if let Ok(t) = m.into_text() {
                    if let Ok(v) = serde_json::from_str::<Value>(&t) {
                        if v.get("type").and_then(|t| t.as_str()) == Some("welcome") {
                            return Some(v);
                        }
                    }
                }
            }
            None
        })
        .await
        .map_err(|_| ClientError { code: "INTERNAL".into(), message: "握手超时".into() })?
        .ok_or_else(|| ClientError { code: "INTERNAL".into(), message: "握手被拒".into() })?;

        {
            let mut g = self.inner.ext.lock().unwrap();
            g.online = welcome.get("extensionOnline").and_then(|v| v.as_bool()).unwrap_or(false);
            g.version = welcome.get("extensionVersion").and_then(|v| v.as_str()).map(|s| s.to_string());
            g.mismatch = welcome.get("versionMismatch").and_then(|v| v.as_bool()).unwrap_or(false);
        }

        let sink = Arc::new(AsyncMutex::new(sink));
        *self.inner.sink.lock().await = Some(sink.clone());
        *self.inner.connected.lock().unwrap() = true;

        let inner = self.inner.clone();
        tokio::spawn(async move {
            while let Some(m) = stream.next().await {
                let m = match m {
                    Ok(m) => m,
                    Err(_) => break,
                };
                let Ok(t) = m.into_text() else { continue };
                let Ok(v) = serde_json::from_str::<Value>(&t) else { continue };
                match v.get("type").and_then(|t| t.as_str()).unwrap_or("") {
                    "res" => {
                        let id = v.get("id").and_then(|i| i.as_str()).unwrap_or("").to_string();
                        let w = inner.waiting.lock().unwrap().remove(&id);
                        if let Some(w) = w {
                            let ok = v.get("ok").and_then(|o| o.as_bool()).unwrap_or(false);
                            let res = if ok {
                                Ok(v.get("data").cloned().unwrap_or(Value::Null))
                            } else {
                                Err(ClientError {
                                    code: v
                                        .get("error")
                                        .and_then(|e| e.get("code"))
                                        .and_then(|c| c.as_str())
                                        .unwrap_or("INTERNAL")
                                        .to_string(),
                                    message: v
                                        .get("error")
                                        .and_then(|e| e.get("message"))
                                        .and_then(|m| m.as_str())
                                        .unwrap_or("命令失败")
                                        .to_string(),
                                })
                            };
                            let _ = w.send(res);
                        }
                    }
                    "event" => {
                        let ev = v.get("event").and_then(|e| e.as_str()).unwrap_or("");
                        let mut g = inner.ext.lock().unwrap();
                        if ev == "extension_online" {
                            g.online = true;
                        }
                        if ev == "extension_offline" {
                            g.online = false;
                        }
                    }
                    _ => {}
                }
            }
            // 读泵退出 = 连接断了
            *inner.connected.lock().unwrap() = false;
            *inner.sink.lock().await = None;
            let waiting = std::mem::take(&mut *inner.waiting.lock().unwrap());
            for (_, w) in waiting {
                let _ = w.send(Err(ClientError { code: "INTERNAL".into(), message: "桥连接已断开".into() }));
            }
        });
        Ok(())
    }

    pub fn close(&self) {
        *self.inner.connected.lock().unwrap() = false;
        let waiting = std::mem::take(&mut *self.inner.waiting.lock().unwrap());
        for (_, w) in waiting {
            let _ = w.send(Err(ClientError { code: "INTERNAL".into(), message: "客户端主动关闭".into() }));
        }
    }

    /// 桥侧已有 30s 超时兜底；这里的 35s 只防「桥自己也没了」
    pub async fn call(&self, cmd: &str, params: Value, tab_id: Option<u64>, timeout: Duration) -> Result<Value, ClientError> {
        if !self.is_connected() {
            self.connect(Duration::from_secs(15)).await?;
        }
        let id = {
            let mut g = self.inner.seq.lock().unwrap();
            *g += 1;
            format!("c{}", g)
        };
        let (tx, rx) = tokio::sync::oneshot::channel();
        self.inner.waiting.lock().unwrap().insert(id.clone(), tx);
        let msg = json!({
            "type": "cmd", "id": id, "cmd": cmd, "params": params, "tabId": tab_id,
            "timeout": timeout.as_millis().saturating_sub(3000) as u64
        });
        {
            let guard = self.inner.sink.lock().await;
            let Some(sink) = guard.as_ref() else {
                self.inner.waiting.lock().unwrap().remove(&id);
                return Err(ClientError { code: "INTERNAL".into(), message: "桥未连接".into() });
            };
            if sink.lock().await.send(Message::Text(msg.to_string().into())).await.is_err() {
                self.inner.waiting.lock().unwrap().remove(&id);
                return Err(ClientError { code: "INTERNAL".into(), message: "桥连接已断开".into() });
            }
        }
        tokio::time::timeout(timeout, rx)
            .await
            .map_err(|_| {
                self.inner.waiting.lock().unwrap().remove(&id);
                ClientError { code: "TIMEOUT".into(), message: "桥无响应".into() }
            })?
            .map_err(|_| ClientError { code: "INTERNAL".into(), message: "桥连接已断开".into() })?
    }
}

/// 停掉旧桥：只认 bridge.json 里记的 pid，发 SIGTERM——桥会先把在途命令回成错误再退
pub fn stop_bridge(info: &BridgeInfo) -> bool {
    if info.pid == 0 {
        return false;
    }
    unsafe { libc::kill(info.pid as i32, libc::SIGTERM) == 0 }
}

/// 抢锁开桥：拿到锁的进程 spawn detached 桥，其余轮询等它起来
pub fn try_start_bridge() -> bool {
    paths::ensure_home();
    let lock = paths::lock_file();
    let fd = match std::fs::OpenOptions::new().write(true).create_new(true).open(&lock) {
        Ok(f) => f,
        Err(_) => {
            if let Ok(m) = std::fs::metadata(&lock) {
                if let Ok(modified) = m.modified() {
                    if modified.elapsed().unwrap_or_default() > Duration::from_secs(30) {
                        let _ = std::fs::remove_file(&lock); // 上次崩在这儿留下的死锁
                    }
                }
            }
            return false;
        }
    };
    drop(fd);
    let exe = std::env::current_exe().unwrap_or_else(|_| "linai-chrome-rs".into());
    let log = std::fs::OpenOptions::new().create(true).append(true).open(paths::log_file());
    let (out, err) = match log {
        Ok(f) => match f.try_clone() {
            Ok(f2) => (std::process::Stdio::from(f), std::process::Stdio::from(f2)),
            Err(_) => (std::process::Stdio::null(), std::process::Stdio::null()),
        },
        Err(_) => (std::process::Stdio::null(), std::process::Stdio::null()),
    };
    let _ = std::process::Command::new(exe)
        .arg("bridge")
        .stdin(std::process::Stdio::null())
        .stdout(out)
        .stderr(err)
        .spawn();
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_secs(3));
        let _ = std::fs::remove_file(&lock);
    });
    true
}
