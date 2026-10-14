//! linai-chrome-rs —— huashu-chrome 的 Rust 改写。
//! 让任何 MCP-capable AI agent 操控你自己的 Chrome——带着全部登录态。
//! 桥（bridge）+ MCP server（stdio）+ CLI（call/doctor/audit），协议与原版兼容。
mod bridge;
mod compat;
mod host;
mod install;
mod learnings;
mod mcp;
mod paths;
mod redact;
mod rpc;

use clap::{Parser, Subcommand};
use serde_json::{json, Value};
use std::time::Duration;

#[derive(Parser)]
#[command(name = "linai-chrome-rs", version, about = "让任何 AI agent 操控你自己的 Chrome（Rust 版桥 + MCP server）")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// 前台跑桥（常驻 127.0.0.1:8899）
    Bridge {
        #[arg(long, default_value_t = 8899)]
        port: u16,
    },
    /// 启动 MCP server（stdio，agent 配置里填这条）
    Mcp {
        #[arg(long)]
        client: Option<String>,
    },
    /// 手动发一条命令：linai-chrome-rs call snapshot / call tabs '{"action":"list"}'
    Call {
        tool: String,
        params: Option<String>,
        #[arg(long)]
        tab: Option<u64>,
    },
    /// 诊断连接问题
    Doctor,
    /// 看最近的浏览器操作记录
    Audit {
        #[arg(short = 'n', long, default_value_t = 30)]
        n: usize,
    },
    /// 打印扩展加载步骤
    Extension,
    /// 一键安装：探测本机 agent，备份并写入各家 MCP 配置
    Install {
        /// 只看会改哪些配置，不写
        #[arg(long)]
        dry_run: bool,
        /// 只配这一个 agent（按 client slug）
        #[arg(long)]
        only: Option<String>,
    },
}

fn main() {
    let cli = Cli::parse();
    let rt = tokio::runtime::Builder::new_multi_thread().enable_all().build().unwrap();
    match cli.cmd {
        Cmd::Bridge { port } => rt.block_on(async {
            let b = bridge::Bridge::new(port, paths::new_token(), true);
            if let Err(e) = b.run().await {
                eprintln!("桥启动失败: {}", e);
                std::process::exit(3);
            }
        }),
        Cmd::Mcp { client } => rt.block_on(mcp::run(client)),
        Cmd::Call { tool, params, tab } => rt.block_on(async {
            let params: Value = params.as_deref().map(|s| serde_json::from_str(s).unwrap_or(json!({}))).unwrap_or(json!({}));
            let c = rpc::BridgeClient::new("cli", None, None);
            match c.call(&tool, params, tab, Duration::from_secs(60)).await {
                Ok(v) => {
                    let text = match &v {
                        Value::String(s) => s.clone(),
                        o => o.get("text").and_then(|t| t.as_str()).map(|s| s.to_string()).unwrap_or_else(|| serde_json::to_string_pretty(o).unwrap_or_default()),
                    };
                    println!("{}", text);
                }
                Err(e) => {
                    eprintln!("{}", e);
                    std::process::exit(1);
                }
            }
            c.close();
        }),
        Cmd::Doctor => rt.block_on(doctor()),
        Cmd::Audit { n } => {
            let Ok(raw) = std::fs::read_to_string(paths::audit_file()) else {
                println!("（还没有审计日志）");
                return;
            };
            let lines: Vec<&str> = raw.lines().collect();
            for l in lines.iter().rev().take(n).rev() {
                println!("{}", l);
            }
        }
        Cmd::Extension => {
            let dir = std::env::current_dir().unwrap_or_default().join("extension");
            println!("扩展目录：{}", dir.display());
            println!("chrome://extensions → 打开右上角「开发者模式」→「加载已解压的扩展程序」→ 选择上面的目录。");
            println!("装好后点工具栏的 huashu-chrome 图标，看到「已连接」即可。");
        }
        Cmd::Install { dry_run, only } => install::run(dry_run, only.as_deref()),
    }
}

async fn doctor() {
    println!("linai-chrome-rs 体检（v{}）", paths::VERSION);
    println!();
    println!("配置目录");
    println!("  ✅ {}", paths::home().display());
    println!();
    println!("桥");
    let info = paths::read_bridge_info();
    match &info {
        None => {
            println!("  ❌ bridge.json 不存在——桥没在跑。运行 `linai-chrome-rs bridge` 启动。");
            return;
        }
        Some(i) => {
            let alive = unsafe { libc::kill(i.pid as i32, 0) == 0 };
            if !alive {
                println!("  ❌ bridge.json 记着的 pid {} 已经没了。重新运行 `linai-chrome-rs bridge`。", i.pid);
                return;
            }
            println!("  ✅ 进程活着（pid {}，v{}）", i.pid, i.version);
        }
    }
    let c = rpc::BridgeClient::new("doctor", None, None);
    match tokio::time::timeout(Duration::from_secs(6), c.connect(Duration::from_secs(5))).await {
        Ok(Ok(())) => {
            let st = c.ext_state();
            if st.online {
                println!("  ✅ 握手正常 · Chrome 扩展在线（扩展 v{}）", st.version.unwrap_or_else(|| "?".into()));
            } else {
                println!("  ⚠️  握手正常 · 扩展不在线：点工具栏 huashu-chrome 图标 →「重连」；Chrome 没开就先开。");
            }
            c.close();
        }
        _ => println!("  ❌ 连不上桥（握手超时）。看 {} 的最后几行。", paths::log_file().display()),
    }
    println!();
    println!("日志");
    println!("  桥日志   {}", paths::log_file().display());
    println!("  操作审计 {}", paths::audit_file().display());
}
