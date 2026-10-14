//! 一键安装：探测本机 agent、备份并写入各家 MCP 配置。移植自原版 src/install.js。
//!
//! 一条铁律：**绝不静默改用户的配置文件**。先展示要做什么，备份，再写。
//! 探测不到就说清楚，绝不猜。
use regex::Regex;
use serde_json::{json, Value};
use std::fs;
use std::path::{Path, PathBuf};

const SERVER_NAME: &str = "linai-chrome";
const REPO: &str = "https://github.com/linbuildsai/linai-chrome-rs";

struct Spec {
    name: &'static str,
    client: &'static str,
    kind: &'static str, // json | toml
    paths: &'static [&'static str],
}

/// 已知 agent 表（同原版 agents.json 的约定：加一行就多支持一个）
const AGENTS: &[Spec] = &[
    Spec { name: "Claude Code", client: "claude-code", kind: "json", paths: &["~/.claude.json", "~/.claude/mcp.json"] },
    Spec { name: "Codex CLI", client: "codex", kind: "toml", paths: &["~/.codex/config.toml"] },
    Spec { name: "Cursor", client: "cursor", kind: "json", paths: &["~/.cursor/mcp.json"] },
    Spec { name: "Gemini CLI", client: "gemini", kind: "json", paths: &["~/.gemini/settings.json"] },
    Spec { name: "Claude Desktop", client: "claude-desktop", kind: "json", paths: &["$APPDATA/Claude/claude_desktop_config.json", "~/.config/claude/claude_desktop_config.json"] },
    Spec { name: "Windsurf", client: "windsurf", kind: "json", paths: &["~/.codeium/windsurf/mcp_config.json"] },
    Spec { name: "Cline (VS Code)", client: "cline", kind: "json", paths: &["$APPDATA/Code/User/globalStorage/saoudrizwan.claude-dev/settings/cline_mcp_settings.json"] },
    Spec { name: "Roo Code", client: "roo", kind: "json", paths: &["$APPDATA/Code/User/globalStorage/rooveterinaryinc.roo-cline/settings/mcp_settings.json"] },
    Spec { name: "WorkBuddy", client: "workbuddy", kind: "json", paths: &["~/.workbuddy/mcp.json", "~/.workbuddy/.mcp.json"] },
    Spec { name: "CodeBuddy", client: "codebuddy", kind: "json", paths: &["~/.codebuddy/mcp.json"] },
    Spec { name: "Kimi Code", client: "kimi-code", kind: "json", paths: &["~/.kimi-code/mcp.json"] },
    Spec { name: "通义灵码 Lingma", client: "lingma", kind: "json", paths: &["~/.lingma/lingma_mcp.json", "~/.lingma/extension/local/mcp.json"] },
    Spec { name: "MiniMax Mavis", client: "mavis", kind: "json", paths: &["~/.mavis/mcp/mcp.json"] },
    Spec { name: "Trae", client: "trae", kind: "json", paths: &["~/.trae/mcp.json", "$APPDATA/Trae/User/mcp.json"] },
    Spec { name: "豆包 / Doubao", client: "doubao", kind: "json", paths: &["~/.doubao/mcp.json"] },
    Spec { name: "千问 / Qwen Code", client: "qwen", kind: "json", paths: &["~/.qwen/settings.json", "~/.qwen/mcp.json"] },
    Spec { name: "Qoder", client: "qoder", kind: "json", paths: &["~/.qoder/mcp.json"] },
    Spec { name: "DeepSeek", client: "deepseek", kind: "json", paths: &["~/.deepseek/mcp.json"] },
    Spec { name: "iFlow", client: "iflow", kind: "json", paths: &["~/.iflow/settings.json"] },
    Spec { name: "OpenClaw", client: "openclaw", kind: "json", paths: &["~/.qclaw/mcp.json", "~/.openclaw/mcp.json"] },
];

const DISCOVERY_FILENAMES: &[&str] = &["mcp.json", ".mcp.json", "mcp_config.json", "mcp/mcp.json", "settings.json", "config.json"];
const SKIP_DIRS: &[&str] = &[".git", ".npm", ".cache", ".Trash", ".vscode", ".docker", "node_modules"];

#[derive(Clone)]
struct Found {
    name: String,
    client: String,
    kind: String,
    file: PathBuf,
    discovered: bool,
}

fn home() -> PathBuf {
    dirs::home_dir().unwrap_or_else(|| PathBuf::from("."))
}

fn app_data() -> PathBuf {
    #[cfg(target_os = "windows")]
    {
        if let Ok(d) = std::env::var("APPDATA") {
            return PathBuf::from(d);
        }
        return home().join("AppData/Roaming");
    }
    #[cfg(target_os = "macos")]
    {
        return home().join("Library/Application Support");
    }
    #[cfg(all(not(target_os = "windows"), not(target_os = "macos")))]
    {
        if let Ok(d) = std::env::var("XDG_CONFIG_HOME") {
            return PathBuf::from(d);
        }
        return home().join(".config");
    }
}

fn expand(p: &str) -> PathBuf {
    let s = p.replacen('~', &home().to_string_lossy().to_string(), 1);
    let s = s.replace("$APPDATA", &app_data().to_string_lossy());
    PathBuf::from(s)
}

/// MCP server 启动命令：当前二进制自身 + mcp 子命令。
/// 不用 npx——Rust 版没有 npm 分发；二进制路径是稳定入口（不像 node 版本目录会漂移）。
fn launcher(client: &str) -> (String, Vec<String>) {
    let exe = std::env::current_exe()
        .ok()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(|| "linai-chrome-rs".into());
    (exe, vec!["mcp".into(), "--client".into(), client.into()])
}

fn looks_like_mcp(f: &Path) -> bool {
    let Ok(raw) = fs::read_to_string(f) else { return false };
    if !raw.contains("\"mcpServers\"") && !raw.contains("[mcp_servers") {
        return false;
    }
    if f.extension().map(|e| e == "json").unwrap_or(false) {
        return serde_json::from_str::<Value>(&raw).is_ok(); // 坏 JSON 不碰
    }
    true
}

fn known_agents() -> Vec<Found> {
    AGENTS
        .iter()
        .filter_map(|a| {
            a.paths.iter().map(|p| expand(p)).find(|f| f.exists()).map(|file| Found {
                name: a.name.into(),
                client: a.client.into(),
                kind: a.kind.into(),
                file,
                discovered: false,
            })
        })
        .collect()
}

/// 自动发现：home 的点目录里翻常见 MCP 配置文件名。
/// 国产 agent 一个月冒一个，硬编码列表永远追不上；只要它遵循 {mcpServers:{}} 惯例就能被认出来。
fn discover(known_files: &std::collections::HashSet<PathBuf>) -> Vec<Found> {
    let mut out = vec![];
    // 排除范围用表里**所有候选路径**算，不能只用命中的那个——
    // 否则同一个 agent 的第二个配置位置会被当成新 agent 再配一遍
    let mut claimed: std::collections::HashSet<PathBuf> = std::collections::HashSet::new();
    for a in AGENTS {
        for p in a.paths {
            let f = expand(p);
            if let Some(parent) = f.parent() {
                claimed.insert(parent.to_path_buf());
            }
            claimed.insert(f);
        }
    }
    let Ok(entries) = fs::read_dir(home()) else { return out };
    for e in entries.flatten() {
        let name = e.file_name().to_string_lossy().to_string();
        if !e.file_type().map(|t| t.is_dir()).unwrap_or(false) || !name.starts_with('.') {
            continue;
        }
        if SKIP_DIRS.contains(&name.as_str()) {
            continue;
        }
        for fname in DISCOVERY_FILENAMES {
            let f = e.path().join(fname);
            if known_files.contains(&f) || claimed.contains(&f) {
                continue;
            }
            if let Some(parent) = f.parent() {
                if claimed.contains(parent) {
                    continue;
                }
            }
            if out.iter().any(|o| o.file == f) || !f.exists() || !looks_like_mcp(&f) {
                continue;
            }
            let short = name.trim_start_matches('.').to_string();
            out.push(Found {
                name: short.clone(),
                client: short,
                kind: if fname.ends_with(".toml") { "toml".into() } else { "json".into() },
                file: f,
                discovered: true,
            });
        }
    }
    out
}

/// 中日韩字符在终端占两格，padEnd 数的是码元——带中文名的行会歪
fn pad(s: &str, width: usize) -> String {
    let cjk = Regex::new(r"[\u{1100}-\u{115f}\u{2e80}-\u{a4cf}\u{ac00}-\u{d7a3}\u{f900}-\u{faff}\u{fe30}-\u{fe6f}\u{ff00}-\u{ff60}\u{ffe0}-\u{ffe6}]").unwrap();
    let mut w = 0usize;
    for ch in s.chars() {
        w += if cjk.is_match(&ch.to_string()) { 2 } else { 1 };
    }
    format!("{}{}", s, " ".repeat(width.saturating_sub(w)))
}

fn already_configured(t: &Found) -> bool {
    fs::read_to_string(&t.file).map(|s| s.contains(SERVER_NAME)).unwrap_or(false)
}

fn write_json_cfg(t: &Found) -> Result<(), String> {
    let raw = fs::read_to_string(&t.file).map_err(|e| e.to_string())?;
    let mut cfg: Value = serde_json::from_str(&raw).map_err(|e| format!("这个文件不是合法 JSON（{}），不敢动它", e))?;
    let (command, args) = launcher(&t.client);
    let entry = json!({"command": command, "args": args});
    let obj = cfg.as_object_mut().ok_or("配置不是 JSON 对象")?;
    let servers = obj.entry("mcpServers").or_insert_with(|| json!({}));
    servers.as_object_mut().ok_or("mcpServers 不是对象")?.insert(SERVER_NAME.into(), entry);
    fs::write(&t.file, serde_json::to_string_pretty(&cfg).map_err(|e| e.to_string())? + "\n").map_err(|e| e.to_string())
}

/// TOML 不做完整解析——用户 config.toml 里的注释和自定义格式，解析再序列化会全抹掉。只在末尾追加。
fn write_toml_cfg(t: &Found) -> Result<(), String> {
    let (command, args) = launcher(&t.client);
    let args_str = args.iter().map(|a| format!("{:?}", a)).collect::<Vec<_>>().join(", ");
    let block = format!(
        "\n# --- {} (由 linai-chrome-rs install 添加) ---\n[mcp_servers.{}]\ncommand = {:?}\nargs = [{}]\n",
        SERVER_NAME, SERVER_NAME, command, args_str
    );
    let prev = fs::read_to_string(&t.file).map_err(|e| e.to_string())?;
    let sep = if prev.ends_with('\n') { "" } else { "\n" };
    fs::write(&t.file, format!("{}{}{}", prev, sep, block)).map_err(|e| e.to_string())
}

pub fn run(dry_run: bool, only: Option<&str>) {
    println!("\nlinai-chrome-rs 安装\n");
    let all = known_agents();
    let known_files: std::collections::HashSet<PathBuf> = all.iter().map(|a| a.file.clone()).collect();
    let discovered = discover(&known_files);
    let mut found: Vec<Found> = all.into_iter().chain(discovered).collect();

    if let Some(only) = only {
        found.retain(|a| a.client == only);
        if found.is_empty() {
            println!("  这台机器上没找到 {}。\n", only);
            return;
        }
    }

    if found.is_empty() {
        println!("  没在这台机器上找到任何 agent 的 MCP 配置。\n");
        println!("  已知的会自动配置：{}", AGENTS.iter().map(|a| a.name).collect::<Vec<_>>().join("、"));
        println!("  没列出来的 agent 也会被自动发现，只要它把 MCP 配置写在 ~/.<名字>/ 下。\n");
        println!("  都不匹配的话，把下面这段填进它的 MCP 配置：\n");
        let (command, args) = launcher("custom");
        let snippet = serde_json::to_string_pretty(&json!({"mcpServers": {SERVER_NAME: {"command": command, "args": args}}})).unwrap();
        for line in snippet.lines() {
            println!("    {}", line);
        }
        println!();
        print_extension_step();
        return;
    }

    println!("检测到这些 agent：");
    let mut plan = vec![];
    for t in &found {
        let done = already_configured(t);
        let tag = if t.discovered { " (自动发现)" } else { "" };
        println!(
            "  {} {} {}",
            if done { '·' } else { '+' },
            pad(&format!("{}{}", t.name, tag), 26),
            if done { "已配置，跳过" } else { "将写入 MCP 配置" }
        );
        if !done {
            plan.push(t.clone());
        }
    }

    if plan.is_empty() {
        println!("\n所有检测到的 agent 都已经配好了。");
    } else {
        println!("\n将修改 {} 个配置文件，每个都会先备份成 <文件名>.bak-<时间戳>。", plan.len());
        if dry_run {
            println!("\n（--dry-run：只看不写。去掉它重跑就会写入）\n");
            print_extension_step();
            return;
        }
        let mut ok = 0;
        for t in &plan {
            let stamp = chrono::Local::now().format("%Y%m%d%H%M%S%3f");
            let backup = t.file.with_extension(format!("{}.bak-{}", t.file.extension().map(|e| e.to_string_lossy().to_string()).unwrap_or_default(), stamp));
            let result = fs::copy(&t.file, &backup)
                .map_err(|e| e.to_string())
                .and_then(|_| if t.kind == "toml" { write_toml_cfg(t) } else { write_json_cfg(t) });
            match result {
                Ok(()) => {
                    println!("  ✅ {}（原文件已备份为 {}）", t.name, backup.file_name().map(|f| f.to_string_lossy().to_string()).unwrap_or_default());
                    ok += 1;
                }
                Err(e) => {
                    println!("  ❌ {}：{}", t.name, e);
                    println!("     手动加进 {} 也可以，格式见 README", t.file.display());
                }
            }
        }
        if ok > 0 {
            println!("\n{} 个 agent 配好了。它们需要重启一次才会加载新的 MCP server。", ok);
        }
    }

    print_extension_step();
    println!("\n装完扩展后跑 `linai-chrome-rs doctor` 验证。");
    println!("\n如果它帮到你，欢迎给仓库点个 star：{}\n", REPO);
}

fn print_extension_step() {
    println!("\n还差一步：装 Chrome 扩展");
    println!("  扩展目录在仓库的 extension/（或运行 `linai-chrome-rs extension` 打印路径）");
    println!("  chrome://extensions → 开发者模式 → 加载已解压的扩展程序");
    println!("  （浏览器不允许脚本代装扩展，这一下必须你自己点）");
}

/// 供 doctor 复用的「本机有哪些 agent 配置」快照
pub fn found_agents_summary() -> Vec<(String, PathBuf, bool)> {
    let all = known_agents();
    let known_files: std::collections::HashSet<PathBuf> = all.iter().map(|a| a.file.clone()).collect();
    let discovered = discover(&known_files);
    all.into_iter()
        .chain(discovered)
        .map(|t| {
            let done = already_configured(&t);
            (t.name, t.file, done)
        })
        .collect()
}
