//! 审计脱敏 —— 与原版 src/bridge.js 的 redact + extension/redact.js 的 scrubProse 对齐。
//!
//! 按键名递归脱敏（不是按路径点名）：act 把动作嵌在 steps[] 里整条穿过，
//! 按路径点名的版本实测漏了 5 条疑似密码、27 个手机号的明文。
//! 正文层再按词挖凭据：`ask` 的 prompt 是写给人看的一段话，键名脱敏对它结构性无效。
use once_cell::sync::Lazy;
use regex::Regex;
use serde_json::Value;

const SECRET_KEYS: &[&str] = &["text", "value", "password", "token", "secret", "code"];
const CRED_MIN: usize = 12;

static ASCII_RUN: Lazy<Regex> = Lazy::new(|| Regex::new(r"[!-~]+").unwrap());
/// rust 的 regex 不支持 lookaround，手机号用「模式 + 手写数字边界检查」实现
static PHONE_CORE: Lazy<Regex> = Lazy::new(|| Regex::new(r"(?:(?:00|\+)?86)?1[3-9][0-9]{9}").unwrap());

fn redact_phones(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = String::new();
    let mut last = 0usize;
    for m in PHONE_CORE.find_iter(s) {
        // 卡数字边界：19 位推文 ID 里嵌着 11 位「像手机号」的片段不该被挖
        let prev_digit = m.start() > 0 && bytes[m.start() - 1].is_ascii_digit();
        let next_digit = m.end() < bytes.len() && bytes[m.end()].is_ascii_digit();
        if prev_digit || next_digit {
            continue;
        }
        out.push_str(&s[last..m.start()]);
        out.push_str("<手机号>");
        last = m.end();
    }
    out.push_str(&s[last..]);
    out
}

fn is_cred_token(w: &str) -> bool {
    if w.len() < CRED_MIN || w.len() > 256 {
        return false;
    }
    if w.contains('/') || w.contains('\\') || w.contains('@') {
        return false; // URL / 路径 / 邮箱整条放行
    }
    w.chars().any(|c| c.is_ascii_digit()) && w.chars().any(|c| c.is_ascii_alphabetic())
}

/// 正文层脱敏：无空白 = 标识符，不动；有空白才当正文，按 ASCII 可见串成词挖凭据 + 手机号。
pub fn scrub_prose(s: &str) -> String {
    if !s.chars().any(|c| c.is_whitespace() || c == '\u{3000}') {
        return s.to_string();
    }
    let after_cred = ASCII_RUN
        .replace_all(s, |caps: &regex::Captures| {
            let w = &caps[0];
            if is_cred_token(w) {
                format!("<凭据{}字>", w.chars().count())
            } else {
                w.to_string()
            }
        })
        .to_string();
    redact_phones(&after_cred)
}
fn scrub(v: Value, depth: usize) -> Value {
    if depth > 8 {
        return v;
    }
    match v {
        Value::Array(arr) => Value::Array(arr.into_iter().map(|x| scrub(x, depth + 1)).collect()),
        Value::Object(map) => {
            let mut out = serde_json::Map::new();
            for (k, val) in map {
                if let Value::String(s) = &val {
                    if SECRET_KEYS.contains(&k.as_str()) {
                        out.insert(k, Value::String(format!("<{}字>", s.chars().count())));
                        continue;
                    }
                    if k == "base64" {
                        out.insert(
                            k,
                            Value::String(format!("<{}KB>", (s.len() as f64 * 0.75 / 1024.0).round() as i64)),
                        );
                        continue;
                    }
                    if k == "expr" && s.chars().count() > 200 {
                        let t: String = s.chars().take(200).collect();
                        out.insert(k, Value::String(format!("{}…", t)));
                        continue;
                    }
                }
                out.insert(k, scrub(val, depth + 1));
            }
            Value::Object(out)
        }
        Value::String(s) => Value::String(scrub_prose(&s)),
        other => other,
    }
}

pub fn redact(params: &Value) -> Value {
    match params {
        Value::Object(_) | Value::Array(_) => scrub(params.clone(), 0),
        other => other.clone(),
    }
}

/// 写操作回执的效果分类进审计（只取类别，不存回执正文）。
pub fn effect_of(data: &Value) -> Option<&'static str> {
    let t = match data {
        Value::String(s) => s.clone(),
        Value::Object(o) => o.get("text")?.as_str()?.to_string(),
        _ => return None,
    };
    let head: String = t.chars().take(1200).collect();
    if head.contains("act 停在") {
        return Some("act-stopped");
    }
    if head.contains("act 完成") {
        return Some(if data.get("recipe").is_some() { "act-done+draft" } else { "act-done" });
    }
    if head.contains("操作已发出，但页面完全没有反应") {
        return Some("none");
    }
    if head.contains("没有可归因于这次操作的变化") {
        return Some("unattributable");
    }
    if head.contains("发出了请求：") {
        return Some("request-only");
    }
    if head.contains("效果：") {
        return Some(if head.contains("请求：") { "changed+request" } else { "changed" });
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secret_keys_are_redacted_recursively() {
        let v = serde_json::json!({"steps": [{"do": "type", "text": "hunter2secret"}], "ref": "e3"});
        let r = redact(&v);
        assert_eq!(r["steps"][0]["text"], serde_json::json!("<13字>"));
        assert_eq!(r["ref"], serde_json::json!("e3"));
    }

    #[test]
    fn prose_credentials_are_scrubbed() {
        // 带空白才当正文（原版规则：无空白 = 标识符，不动）
        let s = "密码我填好了（918273SiteAuth! ），你去点验证码";
        let r = scrub_prose(s);
        assert!(r.contains("<凭据"), "got: {}", r);
        assert!(r.contains("验证码"));
    }

    #[test]
    fn identifier_untouched() {
        let s = "#submit-button";
        assert_eq!(scrub_prose(s), s);
    }

    #[test]
    fn phone_redacted_with_boundary() {
        let s = "联系 13800138000 或 status/2025091650171031";
        let r = scrub_prose(s);
        assert!(r.contains("<手机号>"));
        assert!(r.contains("2025091650171031"), "推文 ID 不该被挖: {}", r);
    }
}
