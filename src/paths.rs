//! 配置目录与 token —— 桥和 MCP server 共用的落盘约定。
//! 与 Node 原版保持同一套路径，两个实现可以互相接管（bridge.json 通用）。
use serde::{Deserialize, Serialize};
use std::fs;
use std::io::Write;
use std::path::PathBuf;

pub const DEFAULT_PORT: u16 = 8899;
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
/// 桥 ↔ 扩展 ↔ MCP 三方协议号，与原版 extension/compat.js 的 PROTOCOL 对齐
pub const PROTOCOL: u32 = 1;

pub fn home() -> PathBuf {
    // HUASHU_CHROME_HOME 供测试隔离：测试桥不碰用户真实的 ~/.huashu-chrome
    if let Ok(h) = std::env::var("HUASHU_CHROME_HOME") {
        return PathBuf::from(h);
    }
    dirs::home_dir().unwrap_or_else(|| PathBuf::from(".")).join(".huashu-chrome")
}
pub fn bridge_file() -> PathBuf {
    home().join("bridge.json")
}
pub fn lock_file() -> PathBuf {
    home().join("bridge.lock")
}
pub fn audit_file() -> PathBuf {
    home().join("audit.jsonl")
}
pub fn allowlist_file() -> PathBuf {
    home().join("allowlist.json")
}
pub fn log_file() -> PathBuf {
    home().join("bridge.log")
}

pub fn ensure_home() -> PathBuf {
    let h = home();
    let _ = fs::create_dir_all(&h);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(&h, fs::Permissions::from_mode(0o700));
    }
    h
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BridgeInfo {
    pub port: u16,
    pub token: String,
    pub pid: u32,
    pub version: String,
    #[serde(default)]
    pub started_at: Option<String>,
    /// "rust" = Rust 桥；缺省 = Node 原版桥。
    /// 版本换代只踢同类：两个实现协议 v1 兼容，Rust CLI 连 Node 桥（或反之）
    /// 是合法状态，互相踢会变成换代战争。
    #[serde(default)]
    pub kind: Option<String>,
}

/// 桥启动时写：端口、token、pid。token 每次启动轮换。
/// 原子写（tmp + rename），权限 0600。
pub fn write_bridge_info(info: &BridgeInfo) -> std::io::Result<()> {
    ensure_home();
    let tmp = bridge_file().with_extension("json.tmp");
    let mut f = fs::File::create(&tmp)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = f.set_permissions(fs::Permissions::from_mode(0o600));
    }
    f.write_all(serde_json::to_string_pretty(info)?.as_bytes())?;
    fs::rename(&tmp, bridge_file())?;
    Ok(())
}

pub fn read_bridge_info() -> Option<BridgeInfo> {
    let raw = fs::read_to_string(bridge_file()).ok()?;
    serde_json::from_str(&raw).ok()
}

pub fn new_token() -> String {
    use rand::RngCore;
    let mut b = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut b);
    b.iter().map(|x| format!("{:02x}", x)).collect()
}

/// 常数时间比较，避免 token 被计时侧信道试探出来
pub fn token_equals(a: &str, b: &str) -> bool {
    let (ab, bb) = (a.as_bytes(), b.as_bytes());
    if ab.len() != bb.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in ab.iter().zip(bb.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

/// 审计日志：一行一条 JSON，出事能查。
pub fn audit(entry: serde_json::Value) {
    let mut obj = entry;
    if let Some(o) = obj.as_object_mut() {
        o.insert("ts".into(), serde_json::Value::String(now_iso()));
        let line = serde_json::to_string(o).unwrap_or_default();
        if let Ok(mut f) = fs::OpenOptions::new().create(true).append(true).open(audit_file()) {
            let _ = writeln!(f, "{}", line);
        }
    }
}

pub fn now_iso() -> String {
    chrono::Local::now().format("%Y-%m-%dT%H:%M:%S%.3f%:z").to_string()
}

pub fn now_clock() -> String {
    chrono::Local::now().format("%H:%M:%S").to_string()
}
