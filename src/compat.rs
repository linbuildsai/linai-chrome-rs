//! 扩展和本地端兼不兼容 —— 与原版 extension/compat.js 对齐。
//! 分级：same / patch（静默）/ ext-old / cli-old / protocol（真不兼容）。
pub const PROTOCOL: u32 = crate::paths::PROTOCOL;

fn parse(v: &str) -> [u32; 3] {
    let mut out = [0u32; 3];
    for (i, part) in v.split('.').take(3).enumerate() {
        out[i] = part.trim().parse().unwrap_or(0);
    }
    out
}

pub fn cmp_version(a: &str, b: &str) -> i32 {
    let (x, y) = (parse(a), parse(b));
    for i in 0..3 {
        if x[i] != y[i] {
            return if x[i] < y[i] { -1 } else { 1 };
        }
    }
    0
}

pub fn compat(ext: &str, cli: &str, ext_protocol: u32, cli_protocol: u32) -> &'static str {
    if ext.is_empty() || cli.is_empty() {
        return "same";
    }
    if ext_protocol != cli_protocol {
        return "protocol";
    }
    let c = cmp_version(ext, cli);
    if c == 0 {
        return "same";
    }
    let (a, b) = (parse(ext), parse(cli));
    if a[0] == b[0] && a[1] == b[1] {
        return "patch";
    }
    if c < 0 {
        "ext-old"
    } else {
        "cli-old"
    }
}

pub const UPDATE_EXT: &str = "商店装的扩展 Chrome 会在几小时内自动更新；等不及就打开 chrome://extensions，开「开发者模式」后点「更新」。手动加载的扩展要去那一页点「重新加载」。";
pub const UPDATE_CLI: &str = "在终端跑一次 linai-chrome-rs（cargo build --release）更新二进制，然后重启你的 agent。";

pub fn compat_note(level: &str, ext: &str, cli: &str) -> String {
    match level {
        "protocol" => format!(
            "扩展 v{} 和本地端 v{} 的通信协议不兼容，命令可能失败。{}",
            ext,
            cli,
            if cmp_version(ext, cli) < 0 { UPDATE_EXT } else { UPDATE_CLI }
        ),
        "ext-old" => format!("扩展 v{} 比本地端 v{} 旧，新功能可能还用不了。{}", ext, cli, UPDATE_EXT),
        "cli-old" => format!("本地端 v{} 比扩展 v{} 旧，有新功能没用上。{}", cli, ext, UPDATE_CLI),
        _ => String::new(),
    }
}
