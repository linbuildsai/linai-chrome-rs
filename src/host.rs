//! 宿主识别 —— 侧栏、页面小头像和审计日志里「谁在操控」那个名字的来源。
//! 规则与原版 src/lib/host.js 一致：MCP initialize 握手里宿主自报的 clientInfo.name
//! 为准，--client 参数只在宿主没报或报了泛称时兜底。
use once_cell::sync::Lazy;
use serde_json::Value;

struct Agent {
    client: &'static str,
    name: &'static str,
}

/// 已知 agent 表。原版承诺「加一个 agent 只加一行」，这里同样。
const AGENTS: &[Agent] = &[
    Agent { client: "claude-code", name: "Claude Code" },
    Agent { client: "claude-desktop", name: "Claude Desktop" },
    Agent { client: "codex", name: "Codex CLI" },
    Agent { client: "cursor", name: "Cursor" },
    Agent { client: "gemini", name: "Gemini CLI" },
    Agent { client: "windsurf", name: "Windsurf" },
    Agent { client: "cline", name: "Cline" },
    Agent { client: "roo-code", name: "Roo Code" },
    Agent { client: "workbuddy", name: "WorkBuddy" },
    Agent { client: "codebuddy", name: "CodeBuddy" },
    Agent { client: "kimi-code", name: "Kimi Code" },
    Agent { client: "lingma", name: "通义灵码" },
    Agent { client: "minimax-mavis", name: "MiniMax Mavis" },
    Agent { client: "trae", name: "Trae" },
    Agent { client: "doubao", name: "豆包" },
    Agent { client: "qwen-code", name: "Qwen Code" },
    Agent { client: "qoder", name: "Qoder" },
    Agent { client: "deepseek", name: "DeepSeek" },
    Agent { client: "iflow", name: "iFlow" },
    Agent { client: "opencode", name: "OpenCode" },
    Agent { client: "openclaw", name: "OpenClaw" },
];

static ALIAS: Lazy<std::collections::HashMap<&'static str, &'static str>> =
    Lazy::new(|| [("claude-ai", "claude-desktop")].into_iter().collect());

static GENERIC: Lazy<std::collections::HashSet<&'static str>> =
    Lazy::new(|| ["", "mcp", "client", "unknown", "anonymous"].into_iter().collect());

/// "Codex-MCP-Client" → codex；"Roo Code" → roo-code
pub fn slug_of(name: &str) -> String {
    let s = name.trim().to_lowercase();
    let s = regex::Regex::new(r"[\s_]+").unwrap().replace_all(&s, "-").to_string();
    let s = regex::Regex::new(r"(-mcp)?-(client|host)$").unwrap().replace(&s, "").to_string();
    let s = s.trim_matches('-').to_string();
    ALIAS.get(s.as_str()).map(|v| v.to_string()).unwrap_or(s)
}

pub struct Host {
    pub client: String,
    pub label: Option<String>,
    pub raw: Option<String>,
    pub source: &'static str,
}

pub fn resolve_host(client_info_name: Option<&str>, flag: Option<&str>) -> Host {
    let raw = client_info_name.map(|s| s.to_string());
    let slug = slug_of(raw.as_deref().unwrap_or(""));
    if !GENERIC.contains(slug.as_str()) {
        let known = AGENTS.iter().find(|a| slug == a.client || slug.starts_with(&format!("{}-", a.client)));
        return Host {
            client: known.map(|a| a.client.to_string()).unwrap_or_else(|| slug.clone()),
            label: known.map(|a| a.name.to_string()),
            raw,
            source: "clientInfo",
        };
    }
    let f = slug_of(flag.unwrap_or(""));
    let known = AGENTS.iter().find(|a| f == a.client);
    Host {
        client: if GENERIC.contains(f.as_str()) { "unknown".into() } else { f },
        label: known.map(|a| a.name.to_string()),
        raw,
        source: "flag",
    }
}

/// 从 MCP initialize 的 params.clientInfo.name 里取自报名
pub fn client_info_name(init_params: &Value) -> Option<String> {
    init_params.get("clientInfo")?.get("name")?.as_str().map(|s| s.to_string())
}
