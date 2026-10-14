//! 站点经验回流 —— 纯本地读写，不需要桥和浏览器。
//! learnings.json: { "<domain>": { entries: [...], updatedAt } }
use crate::paths;
use serde_json::{json, Value};
use std::collections::BTreeMap;

type Store = BTreeMap<String, Value>;

fn load() -> Store {
    std::fs::read_to_string(paths::learnings_file())
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

fn save(store: &Store) {
    let _ = std::fs::write(paths::learnings_file(), serde_json::to_string_pretty(store).unwrap_or_default());
}

pub fn get(domain: &str) -> Value {
    let store = load();
    match store.get(domain) {
        Some(v) => v.clone(),
        None => json!({"domain": domain, "entries": []}),
    }
}

pub fn save_learning(domain: &str, entry: Value) -> Value {
    let mut store = load();
    let item = store.entry(domain.to_string()).or_insert_with(|| json!({"domain": domain, "entries": []}));
    if let Some(arr) = item.get_mut("entries").and_then(|e| e.as_array_mut()) {
        arr.push(entry);
    }
    item.as_object_mut().map(|o| o.insert("updatedAt".into(), json!(paths::now_iso())));
    let out = item.clone();
    save(&store);
    out
}
