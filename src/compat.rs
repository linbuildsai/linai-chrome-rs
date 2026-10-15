//! 扩展和本地端兼不兼容 —— 判定的**唯一真源是协议号**（PROTOCOL）。
//!
//! 为什么不比产品版本号：原版里桥和扩展同属一个 npm 包、版本号同源，
//! 拿 semver 比是对的。但本项目桥是 Rust 实现、扩展原样复用原版，
//! 两条版本号来自不相干的发布线（本地端 0.2.0 vs 扩展 1.2.0），
//! 比较毫无意义——只会永远误报「本地端旧」。协议号一致就是兼容，
//! 这才是三方约定的契约。版本号只用于展示，不参与兼容判定。
//!
//! 分级：same（协议一致，兼容）/ protocol（协议不同，真不兼容）。
pub const PROTOCOL: u32 = crate::paths::PROTOCOL;

/// 只按协议号判兼容：一致 → same，不一致 → protocol。
/// 扩展版本号与本地端版本号不再参与判定（它们来自不同发布线，不可比）。
pub fn compat(ext_protocol: u32, cli_protocol: u32) -> &'static str {
    if ext_protocol == cli_protocol {
        "same"
    } else {
        "protocol"
    }
}

pub const UPDATE_EXT: &str = "商店装的扩展 Chrome 会在几小时内自动更新；等不及就打开 chrome://extensions，开「开发者模式」后点「更新」。手动加载的扩展要去那一页点「重新加载」。";
pub const UPDATE_CLI: &str = "更新 linai-chrome-rs 到匹配协议号的版本（git pull 后 cargo build --release），然后重启你的 agent。";

/// 给人看的一句话。same 返回空串——协议一致就别说话。
pub fn compat_note(level: &str, ext: &str, cli: &str, ext_protocol: u32, cli_protocol: u32) -> String {
    if level != "protocol" {
        return String::new();
    }
    format!(
        "扩展（v{}，协议 v{}）和本地端（v{}，协议 v{}）的通信协议不兼容，命令可能失败。{}",
        ext, ext_protocol, cli, cli_protocol,
        if ext_protocol < cli_protocol { UPDATE_EXT } else { UPDATE_CLI }
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn same_protocol_is_compatible() {
        assert_eq!(compat(1, 1), "same");
    }

    #[test]
    fn different_protocol_is_incompatible() {
        assert_eq!(compat(1, 2), "protocol");
        assert_eq!(compat(2, 1), "protocol");
    }

    #[test]
    fn version_numbers_do_not_trigger_warning() {
        // 回归：本地端 0.2.0 与扩展 1.2.0，协议都是 1 —— 必须兼容、无警告。
        // 这正是用户遇到的误报场景。
        assert_eq!(compat(1, 1), "same");
        assert!(compat_note("same", "1.2.0", "0.2.0", 1, 1).is_empty());
    }

    #[test]
    fn protocol_mismatch_message_points_to_the_right_side() {
        // 扩展协议更旧 → 提示更新扩展
        let note = compat_note("protocol", "1.2.0", "0.2.0", 1, 2);
        assert!(note.contains("不兼容"));
        assert!(note.contains("chrome://extensions"), "应指向扩展更新: {}", note);
        // 本地端协议更旧 → 提示更新本地端
        let note2 = compat_note("protocol", "1.2.0", "0.2.0", 2, 1);
        assert!(note2.contains("cargo build --release"), "应指向本地端更新: {}", note2);
    }
}
