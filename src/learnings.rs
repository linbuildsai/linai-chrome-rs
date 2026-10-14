//! 经验库 —— 站点操作经验的读写 + act 剧本草稿。移植自原版 src/lib/learnings.js。
//!
//! 两层：
//!   出厂经验  <exe目录或CARGO_MANIFEST_DIR>/docs/经验/<域名>.md   随包分发，升级即更新
//!   本机经验  ~/.huashu-chrome/learnings/<域名>.md   agent 干活时学到的增量，永不被升级覆盖
//!
//! 剧本草稿：act 完整跑通后扩展把 steps 整理成 recipe（ref→find、输入→占位符），
//! 这里落成草稿；确认可复用后由 agent 用 adopt 收进经验正文（先草稿、确认后入库）。
use crate::paths;
use once_cell::sync::Lazy;
use regex::Regex;
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

const HINT: &str = "[经验仅供参考，不是规则。它记录的是过去某个时点的实况——站点会改版、环境各不相同。与页面实际不符时，以你观察到的实际为准，并在收工时用 learnings(domain, save) 改写。]";

static ALIAS: Lazy<HashMap<&'static str, &'static str>> = Lazy::new(|| {
    [
        ("tmall.com", "taobao.com"),
        ("twitter.com", "x.com"),
        ("larksuite.com", "feishu.cn"),
        ("qpic.cn", "weixin.qq.com"),
        ("weibo.com", "weibo.cn"),
    ]
    .into_iter()
    .collect()
});

static SHARED_HOSTS: Lazy<HashSet<&'static str>> = Lazy::new(|| {
    [
        "github.io", "vercel.app", "netlify.app", "pages.dev", "web.app", "firebaseapp.com",
        "herokuapp.com", "blogspot.com", "gitbook.io", "notion.site", "feishu.cn", "larksuite.com",
    ]
    .into_iter()
    .collect()
});

pub fn learnings_dir() -> PathBuf {
    if let Ok(d) = std::env::var("HUASHU_CHROME_LEARNINGS_DIR") {
        return PathBuf::from(d);
    }
    paths::home().join("learnings")
}

/// 出厂经验目录：优先可执行文件旁（cargo install 场景），退回编译期源码目录（开发场景）
pub fn seed_dir() -> PathBuf {
    if let Ok(exe) = std::env::current_exe() {
        let cand = exe.parent().unwrap_or(Path::new(".")).join("docs/经验");
        if cand.is_dir() {
            return cand;
        }
    }
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("docs/经验")
}

/// 'https://my.feishu.cn/base/x?y=1' → 'my.feishu.cn'
fn normalize(input: &str) -> String {
    let d = input.trim().to_lowercase();
    let d = Regex::new(r"^[a-z]+://").unwrap().replace(&d, "").to_string();
    let host = d.split(['/', '?', '#']).next().unwrap_or("").to_string();
    let host = host.split(':').next().unwrap_or("").to_string();
    Regex::new(r"^www\.").unwrap().replace(&host, "").to_string()
}

fn md_files(dir: &Path) -> Vec<String> {
    let Ok(rd) = fs::read_dir(dir) else { return vec![] };
    rd.filter_map(|e| e.ok())
        .filter_map(|e| e.file_name().to_str().map(|s| s.to_string()))
        .filter(|f| f.ends_with(".md"))
        .map(|f| f.trim_end_matches(".md").to_string())
        .collect()
}

/// 逐级去子域找归属键：my.feishu.cn → feishu.cn。别名先展开。
fn resolve_key(domain: &str) -> Option<String> {
    let mut known: HashSet<String> = md_files(&seed_dir()).into_iter().collect();
    known.extend(md_files(&learnings_dir()));
    let d = normalize(domain);
    if d.is_empty() {
        return None;
    }
    let parts: Vec<&str> = d.split('.').collect();
    for i in 0..parts.len().saturating_sub(1) {
        let cand = parts[i..].join(".");
        let key = ALIAS.get(cand.as_str()).map(|s| s.to_string()).unwrap_or(cand);
        if known.contains(&key) {
            return Some(key);
        }
    }
    if let Some(a) = ALIAS.get(d.as_str()) {
        return Some(a.to_string());
    }
    // 没见过的站归到主域名；托管平台子域保留三段
    if parts.len() > 2 {
        let base = parts[parts.len() - 2..].join(".");
        let second_last = parts[parts.len() - 2];
        let last = parts[parts.len() - 1];
        let keep3 = SHARED_HOSTS.contains(base.as_str())
            || (matches!(second_last, "com" | "net" | "org" | "gov" | "edu" | "co" | "ac") && last.len() == 2);
        let take = if keep3 { 3 } else { 2 };
        Some(parts[parts.len() - take..].join("."))
    } else {
        Some(d)
    }
}

fn read_if(dir: &Path, key: &str) -> Option<String> {
    fs::read_to_string(dir.join(format!("{}.md", key))).ok().map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
}

// ---------- 剧本草稿 ----------

fn draft_dir() -> PathBuf {
    learnings_dir().join("_drafts")
}
fn draft_file(key: &str) -> PathBuf {
    draft_dir().join(format!("{}.json", key))
}

fn read_drafts(key: &str) -> Vec<Value> {
    fs::read_to_string(draft_file(key))
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

/// 起点只留 host + path：query 里常有 token、搜索词这类不该落盘的东西
fn path_only(url: &str) -> String {
    url::Url::parse(url).map(|u| format!("{}{}", u.host_str().unwrap_or(""), u.path())).unwrap_or_default()
}

fn drafts_note(key: &str, drafts: &[Value]) -> String {
    let list: Vec<String> = drafts
        .iter()
        .map(|d| {
            format!(
                "{}（{} 步，跑通 {} 次，{}，起点 {}）",
                d["id"].as_str().unwrap_or("?"),
                d["steps"].as_array().map(|a| a.len()).unwrap_or(0),
                d["runs"].as_u64().unwrap_or(1),
                &d["at"].as_str().unwrap_or("")[..10.min(d["at"].as_str().unwrap_or("").len())],
                d["from"].as_str().unwrap_or("?")
            )
        })
        .collect();
    format!(
        "[有 {} 份待确认的剧本草稿（之前跑通过的 act，ref 已换成 find、输入已换成占位符）：{}。确认可复用就 learnings({{domain:\"{}\", adopt:\"d…\", name:\"这段流程叫什么\"}}) 收进经验；不确定先问用户。]",
        drafts.len(),
        list.join("；"),
        key
    )
}

/// act 完整跑通后落草稿；同一份步骤再跑通只记次数。返回 Some((key, id, runs))。
pub fn save_draft(url: &str, steps: &Value) -> Option<(String, String, u64)> {
    let key = resolve_key(url)?;
    let steps_arr = steps.as_array()?;
    if steps_arr.is_empty() {
        return None;
    }
    let mut list = read_drafts(&key);
    let sig = serde_json::to_string(steps).unwrap_or_default();
    if let Some(d) = list.iter_mut().find(|x| serde_json::to_string(&x["steps"]).unwrap_or_default() == sig) {
        let runs = d["runs"].as_u64().unwrap_or(1) + 1;
        d["runs"] = json!(runs);
        d["at"] = json!(paths::now_iso());
        let id = d["id"].as_str().unwrap_or("d1").to_string();
        write_drafts(&key, &list);
        return Some((key, id, runs));
    }
    let n = list
        .iter()
        .filter_map(|x| x["id"].as_str().and_then(|s| s[1..].parse::<u64>().ok()))
        .max()
        .unwrap_or(0)
        + 1;
    let d = json!({
        "id": format!("d{}", n),
        "at": paths::now_iso(),
        "from": path_only(url),
        "runs": 1,
        "steps": steps,
    });
    list.push(d);
    // 只留最近 5 份
    if list.len() > 5 {
        list = list.split_off(list.len() - 5);
    }
    write_drafts(&key, &list);
    Some((key, format!("d{}", n), 1))
}

fn write_drafts(key: &str, list: &[Value]) {
    let _ = fs::create_dir_all(draft_dir());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(draft_dir(), fs::Permissions::from_mode(0o700));
    }
    let _ = fs::write(draft_file(key), serde_json::to_string_pretty(list).unwrap_or_default());
}

pub fn adopt_draft(domain: &str, id: &str, name: Option<&str>) -> String {
    let Some(key) = resolve_key(domain) else { return "缺 domain。".into() };
    let list = read_drafts(&key);
    let Some(d) = list.iter().find(|x| x["id"].as_str() == Some(id)) else {
        let have: Vec<&str> = list.iter().filter_map(|x| x["id"].as_str()).collect();
        return format!("「{}」没有草稿 {}。现有：{}。", key, id, if have.is_empty() { "无".into() } else { have.join("、") });
    };
    let steps_sig = serde_json::to_string(&d["steps"]).unwrap_or_default();
    let title = name.map(|s| s.trim()).filter(|s| !s.is_empty()).map(|s| s.to_string()).unwrap_or_else(|| {
        format!("{} 上的 {} 步流程", d["from"].as_str().unwrap_or("?"), d["steps"].as_array().map(|a| a.len()).unwrap_or(0))
    });
    let slots: Vec<String> = {
        let mut v: Vec<String> = Regex::new(r"\{\{[^{}]+\}\}").unwrap().find_iter(&steps_sig).map(|m| m.as_str().to_string()).collect();
        v.sort();
        v.dedup();
        v
    };
    let runs = d["runs"].as_u64().unwrap_or(1);
    let from = d["from"].as_str().unwrap_or("?");
    let block = format!(
        "## 剧本：{}\n\n{} 收录，跑通 {} 次，起点 `{}`。{}\n\n```act\n{}\n```",
        title,
        today(),
        runs,
        from,
        if slots.is_empty() { String::new() } else { format!("运行前替换：{}。", slots.join("、")) },
        serde_json::to_string_pretty(&d["steps"]).unwrap_or_default()
    );
    let local = read_if(&learnings_dir(), &key);
    let merged = [local.as_deref(), Some(block.as_str())].into_iter().flatten().collect::<Vec<_>>().join("\n\n");
    write_note(&key, &clean_note(&merged));
    write_drafts(&key, &list.iter().filter(|x| x["id"].as_str() != Some(id)).cloned().collect::<Vec<_>>());
    let warns = lint_playbooks(&block);
    format!(
        "已收录剧本「{}」→ learnings/{}.md。下次 learnings 会把它列在最前面。{}",
        title,
        key,
        if warns.is_empty() { String::new() } else { format!("\n⚠️ 剧本体检没过：{}", warns.join("；")) }
    )
}

// ---------- 读取 ----------

pub fn get_learnings(domain: &str) -> String {
    if domain.is_empty() {
        let mut all: Vec<String> = md_files(&seed_dir()).into_iter().collect();
        all.extend(md_files(&learnings_dir()));
        all.sort();
        all.dedup();
        return if all.is_empty() {
            "经验库是空的。做完任务学到非显而易见的规律时，用 {domain, save} 存下来。".into()
        } else {
            format!("已有经验的站点：{}\n带 {{domain}} 再调一次取具体内容。", all.join("、"))
        };
    }
    let Some(key) = resolve_key(domain) else { return "缺 domain。".into() };
    let seed = read_if(&seed_dir(), &key);
    let local = read_if(&learnings_dir(), &key);
    if seed.is_none() && local.is_none() {
        let mut all: Vec<String> = md_files(&seed_dir()).into_iter().collect();
        all.extend(md_files(&learnings_dir()));
        all.sort();
        all.dedup();
        let drafts = read_drafts(&key);
        let mut out = String::new();
        if !drafts.is_empty() {
            out.push_str(&drafts_note(&key, &drafts));
            out.push('\n');
        }
        out.push_str(&format!("「{}」还没有经验记录——按通用流程干就行，别让查询空手而归拖慢任务。\n", key));
        if !all.is_empty() {
            out.push_str(&format!("已有记录的站点：{}\n", all.join("、")));
        }
        out.push_str(&format!("摸清这个站后用 {{domain: \"{}\", save}} 把规律存下来，下次就快了。", key));
        return out;
    }
    let mut parts: Vec<String> = vec![HINT.into()];
    // 可运行的剧本放最前面
    let joined = [seed.as_deref(), local.as_deref()].into_iter().flatten().collect::<Vec<_>>().join("\n");
    let names: Vec<String> = Regex::new(r"(?m)^## 剧本：(.+)$").unwrap().captures_iter(&joined).map(|c| c[1].trim().to_string()).collect();
    if !names.is_empty() {
        parts.push(format!("[可直接运行的剧本：{}——见下文 ```act 块]", names.iter().map(|x| format!("「{}」", x)).collect::<Vec<_>>().join("、")));
    }
    let drafts = read_drafts(&key);
    if !drafts.is_empty() {
        parts.push(drafts_note(&key, &drafts));
    }
    if let Some(s) = &seed {
        parts.push(format!("## 出厂经验（huashu-chrome v{}）\n\n{}", paths::VERSION, s));
    }
    if let Some(l) = &local {
        parts.push(format!("## 本机经验\n\n{}", l));
    }
    let n = count_playbooks(&parts.join("\n"));
    if n > 0 {
        parts.push(format!("[上面有 {} 份可执行剧本（```act 块）：内容就是 act 的 steps，把 {{{{占位符}}}} 填成实值后可直接运行——一条 act 顶过去几十轮试错。剧本可能过时：assert/until 会在页面不符时停下，停了就按现场重走，收工时把新流程写回来。]", n));
    }
    parts.join("\n\n")
}

// ---------- 保存前清洗 ----------

/// agent 的惯用法是「get → 改 → 整段 save 回去」，而 get 返回的是拼装全文。
/// 原样存回会让出厂段被复制进本机文件、越叠越厚。存的时候剥干净。
/// （JS 版用多行环视正则，rust regex 不支持，这里改成显式的行扫描，语义一致。）
pub fn clean_note(body: &str) -> String {
    let mut b = body.replace(HINT, "");
    // ① 剥掉我们自己的提示段：以 [上面有 N 份可执行剧本 / 可直接运行的剧本 / 有 N 份待确认的剧本草稿 开头、以 ] 结束的整段
    let hint_prefixes = ["[上面有 ", "[可直接运行的剧本", "[有 "];
    let mut out_lines: Vec<&str> = Vec::new();
    let mut skipping = false;
    for line in b.lines() {
        if !skipping && hint_prefixes.iter().any(|p| line.trim_start().starts_with(p)) {
            skipping = true;
        }
        if skipping {
            if line.trim_end().ends_with(']') {
                skipping = false;
            }
            continue;
        }
        out_lines.push(line);
    }
    b = out_lines.join("\n");
    // ② 剥掉回贴的出厂段：从「## 出厂经验（…」到下一个「## 本机经验」或文末
    let mut out_lines: Vec<&str> = Vec::new();
    let mut in_factory = false;
    for line in b.lines() {
        if line.starts_with("## 出厂经验（") {
            in_factory = true;
            continue;
        }
        if in_factory && line.trim() == "## 本机经验" {
            in_factory = false;
            continue;
        }
        if in_factory {
            continue;
        }
        if line.trim() == "## 本机经验" {
            continue;
        }
        out_lines.push(line);
    }
    b = out_lines.join("\n");
    b = Regex::new(r"(?m)^> 最后更新：\d{4}-\d{2}-\d{2}[^\n]*\n?").unwrap().replace(&b, "").to_string();
    // ③ 一字不差的小节只留第一份（按「## 」切段）
    let mut seen: HashSet<String> = HashSet::new();
    let mut out = String::new();
    let mut sec = String::new();
    let mut flush = |sec: &mut String, out: &mut String, seen: &mut HashSet<String>| {
        let k = sec.trim().to_string();
        if !k.is_empty() && !(k.starts_with("## ") && seen.contains(&k)) {
            if !k.is_empty() {
                seen.insert(k.clone());
            }
            out.push_str(sec);
        }
        sec.clear();
    };
    for line in b.lines() {
        if line.starts_with("## ") && !sec.is_empty() {
            flush(&mut sec, &mut out, &mut seen);
        }
        sec.push_str(line);
        sec.push('\n');
    }
    flush(&mut sec, &mut out, &mut seen);
    Regex::new(r"\n{3,}").unwrap().replace_all(&out, "\n\n").trim().to_string()
}

fn today() -> String {
    chrono::Local::now().format("%Y-%m-%d").to_string()
}

/// 覆盖前留底：整文件覆盖是这个接口的语义，但 agent 合并失手时要有地方找回来。每站留最近 10 份。
fn backup(key: &str) {
    let cur = learnings_dir().join(format!("{}.md", key));
    if !cur.exists() {
        return;
    }
    let dir = learnings_dir().join(".history").join(key);
    let _ = fs::create_dir_all(&dir);
    let stamp = paths::now_iso().replace([':', '.'], "-");
    let mut n = 0;
    let mut f = dir.join(format!("{}.md", stamp));
    while f.exists() {
        n += 1;
        f = dir.join(format!("{}-{}.md", stamp, n));
    }
    let _ = fs::copy(&cur, &f);
    if let Ok(mut old) = fs::read_dir(&dir).map(|rd| rd.filter_map(|e| e.ok()).map(|e| e.path()).collect::<Vec<_>>()) {
        old.sort();
        let excess = old.len().saturating_sub(10);
        for f in old.iter().take(excess) {
            let _ = fs::remove_file(f);
        }
    }
}

fn write_note(key: &str, body: &str) {
    let _ = fs::create_dir_all(learnings_dir());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(learnings_dir(), fs::Permissions::from_mode(0o700));
    }
    backup(key);
    let _ = fs::write(learnings_dir().join(format!("{}.md", key)), format!("> 最后更新：{}\n\n{}\n", today(), body));
}

pub fn save_learnings(domain: &str, content: &str) -> String {
    let Some(key) = resolve_key(domain) else { return "缺 domain，没存。".into() };
    let raw = content.trim();
    let body = clean_note(raw);
    if body.is_empty() {
        return "内容为空，没存。要清掉本机经验请直接说明再人工删。".into();
    }
    write_note(&key, &body);
    let warns = lint_playbooks(&body);
    let cleaned = if raw.chars().count().saturating_sub(body.chars().count()) > 40 {
        format!("（已自动剥掉回贴的出厂段落、提示语和重复小节，少存了 {} 字）", raw.chars().count() - body.chars().count())
    } else {
        String::new()
    };
    format!(
        "已存 → learnings/{}.md（{} 字）{}。本机经验整文件覆盖：下次保存前先 get 合并旧内容；旧版本留底在 learnings/.history/。{}",
        key,
        body.chars().count(),
        cleaned,
        if warns.is_empty() { String::new() } else { format!("\n⚠️ 剧本体检没过：{}。照存了（经验不阻塞），但这样的剧本下个会话直接跑会失败——修好再 save 一次。", warns.join("；")) }
    )
}

// ---------- 剧本形状体检（移植自 extension/script.js 的 validateScript） ----------

static PLAYBOOK_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"```act\s*\n([\s\S]*?)```").unwrap());

const BLOCKS: &[&str] = &["repeat", "if", "assert"];

pub fn validate_script(steps: &[Value]) -> Option<String> {
    for (i, st) in steps.iter().enumerate() {
        let do_ = st["do"].as_str().unwrap_or("");
        if do_ == "repeat" {
            let Some(inner) = st["steps"].as_array() else {
                return Some(format!("第 {} 步 repeat 没有 steps 子步", i + 1));
            };
            if inner.is_empty() {
                return Some(format!("第 {} 步 repeat 没有 steps 子步", i + 1));
            }
            for s in inner {
                let sd = s["do"].as_str().unwrap_or("");
                if BLOCKS.contains(&sd) {
                    return Some(format!("第 {} 步 repeat 里嵌了 {}——控制块只允许一层，拆成多次 act", i + 1, sd));
                }
                if s.get("ref").is_some() && s.get("find").is_none() {
                    return Some(format!(
                        "第 {} 步 repeat 的子步用了 ref=\"{}\"——循环里页面每轮都会变，改用 find（role+名字）或 selector",
                        i + 1,
                        s["ref"].as_str().unwrap_or("?")
                    ));
                }
            }
        }
        if do_ == "read" && st.get("ref").is_none() && st.get("find").is_none() && st.get("selector").is_none() && st.get("contains").is_none() {
            return Some(format!("第 {} 步 read 没说读什么：给 ref / find / selector 读某个元素，或给 contains 按文本找", i + 1));
        }
        if do_ == "if" {
            if st.get("cond").is_none() {
                return Some(format!("第 {} 步 if 没写 cond", i + 1));
            }
            let mut branches: Vec<&Value> = st["then"].as_array().map(|a| a.iter().collect()).unwrap_or_default();
            if let Some(arr) = st["else"].as_array() {
                branches.extend(arr.iter());
            }
            for s in branches {
                let sd = s["do"].as_str().unwrap_or("");
                if BLOCKS.contains(&sd) {
                    return Some(format!("第 {} 步 if 分支里嵌了 {}——控制块只允许一层，拆成多次 act", i + 1, sd));
                }
            }
            if st["then"].as_array().map(|a| a.is_empty()).unwrap_or(true) {
                return Some(format!("第 {} 步 if 没有 then 分支", i + 1));
            }
        }
        if do_ == "assert" && st.get("cond").is_none() {
            return Some(format!("第 {} 步 assert 没写 cond", i + 1));
        }
    }
    None
}

pub fn lint_playbooks(md: &str) -> Vec<String> {
    let mut warns = vec![];
    for (i, caps) in PLAYBOOK_RE.captures_iter(md).enumerate() {
        let body = Regex::new(r"\{\{[^{}]*\}\}").unwrap().replace_all(&caps[1], "X").to_string();
        let steps: Value = match serde_json::from_str(&body) {
            Ok(v) => v,
            Err(e) => {
                warns.push(format!("剧本块 {}：不是合法 JSON（{}）", i + 1, e.to_string().chars().take(60).collect::<String>()));
                continue;
            }
        };
        let Some(arr) = steps.as_array() else {
            warns.push(format!("剧本块 {}：应是 act 的 steps 数组", i + 1));
            continue;
        };
        if arr.is_empty() {
            warns.push(format!("剧本块 {}：应是 act 的 steps 数组", i + 1));
            continue;
        }
        if let Some(bad) = validate_script(arr) {
            warns.push(format!("剧本块 {}：{}", i + 1, bad));
        }
    }
    warns
}

fn count_playbooks(md: &str) -> usize {
    PLAYBOOK_RE.find_iter(md).count()
}

/// act 超时预算用的原子步数预估（与扩展侧 EXEC_BUDGET 同源规则）
pub fn flat_count(steps: &Value) -> u64 {
    let Some(arr) = steps.as_array() else { return 0 };
    arr.iter()
        .map(|st| match st["do"].as_str().unwrap_or("") {
            "repeat" => {
                let max = st["max"].as_u64().unwrap_or(10).clamp(1, 25);
                max * st["steps"].as_array().map(|a| a.len() as u64).unwrap_or(1).max(1)
            }
            "if" => {
                let t = st["then"].as_array().map(|a| a.len() as u64).unwrap_or(0);
                let e = st["else"].as_array().map(|a| a.len() as u64).unwrap_or(0);
                t.max(e).max(1)
            }
            _ => 1,
        })
        .sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_strips_scheme_www_port() {
        assert_eq!(normalize("https://www.Example.com/x?a=1"), "example.com");
        assert_eq!(normalize("my.feishu.cn:443/base"), "my.feishu.cn");
    }

    #[test]
    fn clean_note_strips_factory_section() {
        let body = format!("{}\n\n## 出厂经验（huashu-chrome v1.3.0）\n\n出厂内容\n\n## 本机经验\n\n真正的新知识", HINT);
        let c = clean_note(&body);
        assert!(!c.contains("出厂内容"));
        assert!(!c.contains(HINT));
        assert!(c.contains("真正的新知识"));
    }

    #[test]
    fn validate_script_rejects_nested_block() {
        let steps = serde_json::from_str::<Value>(r#"[{"do":"repeat","steps":[{"do":"if","cond":{}}]}]"#).unwrap();
        assert!(validate_script(steps.as_array().unwrap()).is_some());
    }

    #[test]
    fn flat_count_expands_repeat() {
        let steps = serde_json::from_str::<Value>(r#"[{"do":"click"},{"do":"repeat","max":3,"steps":[{"do":"click"},{"do":"scroll"}]}]"#).unwrap();
        assert_eq!(flat_count(&steps), 1 + 3 * 2);
    }

    #[test]
    fn draft_roundtrip() {
        let tmp = std::env::temp_dir().join(format!("hc-learn-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&tmp);
        std::env::set_var("HUASHU_CHROME_LEARNINGS_DIR", &tmp);
        let steps = serde_json::from_str::<Value>(r#"[{"do":"click","find":{"role":"button","name":"登录"}},{"do":"type","text":"{{用户名}}"},{"do":"click","find":{"role":"button","name":"提交"}}]"#).unwrap();
        let (key, id, runs) = save_draft("https://example.com/login?token=x", &steps).unwrap();
        assert_eq!((key.as_str(), runs), ("example.com", 1));
        // 同一份步骤再跑通只记次数
        let (_, _, runs2) = save_draft("https://example.com/login", &steps).unwrap();
        assert_eq!(runs2, 2);
        // query 不落盘
        let raw = fs::read_to_string(draft_file(&key)).unwrap();
        assert!(!raw.contains("token=x"));
        let msg = adopt_draft("example.com", &id, Some("登录流程"));
        assert!(msg.contains("已收录剧本「登录流程」"), "got: {}", msg);
        let note = fs::read_to_string(tmp.join("example.com.md")).unwrap();
        assert!(note.contains("## 剧本：登录流程"));
        assert!(note.contains("```act"));
        let _ = fs::remove_dir_all(&tmp);
        std::env::remove_var("HUASHU_CHROME_LEARNINGS_DIR");
    }
}
