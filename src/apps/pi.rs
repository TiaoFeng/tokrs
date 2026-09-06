//! pi日志解析
//!
//! 数据源: $PI_CODING_AGENT_SESSION_DIR 或 ~/.pi/agent/sessions 或 ~/.pi/sessions 下的 *.jsonl
//! 每个会话文件首条有效 JSON 必须是 type=="session" header, 否则整文件跳过
//! 统计 assistant/toolResult 消息与 compaction/branch_summary 条目的 usage
//! 去重键: 有 entry.id 用 kind:entry.id(last-wins), 否则用内容哈希
//! 参考: cc-switch session_usage_pi.rs (增量游标/接管状态机等有状态逻辑不适用本工具)
//!
use serde_json::Value;
use std::collections::HashMap;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};

use crate::{
    error::AppError,
    io::load,
    model::{AppKind, UsageEntry},
};

const MAX_DEPTH: usize = 4;

pub fn collect() -> Result<Vec<UsageEntry>, AppError> {
    let home = load::home_dir()?;
    // 三候选根目录: 环境变量优先, 其余为新旧默认布局; 缺失目录自动为空
    let mut roots: Vec<PathBuf> = Vec::new();
    if let Some(raw) = std::env::var_os("PI_CODING_AGENT_SESSION_DIR") {
        let s = raw.to_string_lossy();
        let path = if let Some(suffix) = s.strip_prefix("~/") {
            home.join(suffix)
        } else if s == "~" {
            home.clone()
        } else {
            PathBuf::from(s.as_ref())
        };
        if path.is_absolute() {
            roots.push(path);
        }
    }
    roots.push(home.join(".pi").join("agent").join("sessions"));
    roots.push(home.join(".pi").join("sessions"));
    collect_from(&roots)
}

pub fn collect_from(roots: &[PathBuf]) -> Result<Vec<UsageEntry>, AppError> {
    let mut candidates: HashMap<String, UsageEntry> = HashMap::new();
    let mut seen_files = std::collections::HashSet::new();
    for root in roots {
        for file in load::discover_files(root, "jsonl", MAX_DEPTH) {
            // 多根目录可能重叠(如环境变量指向默认路径), 同文件只解析一次
            if !seen_files.insert(file.clone()) {
                continue;
            }
            parse_session(&file, &mut candidates);
        }
    }
    Ok(candidates.into_values().collect())
}

fn parse_session(file: &Path, candidates: &mut HashMap<String, UsageEntry>) {
    let Ok(records) = load::read_jsonl(file) else {
        return;
    };
    // 首条有效 JSON 必须是 session header(畸形行已被 read_jsonl 过滤, 对齐参考实现)
    let Some(first) = records.first() else {
        return;
    };
    if load::str_get(first, &["type"]) != Some("session") {
        return;
    }
    let session_id = load::str_get(first, &["id"]).unwrap_or("unknown");
    let header_ts = first.get("timestamp").and_then(load::timestamp_to_epoch);
    for entry in records.iter().skip(1) {
        if let Some((key, record)) = parse_entry(entry, session_id, header_ts) {
            candidates.insert(key, record);
        }
    }
}

fn parse_entry(
    entry: &Value,
    session_id: &str,
    header_ts: Option<i64>,
) -> Option<(String, UsageEntry)> {
    let entry_type = load::str_get(entry, &["type"])?;
    let (kind, message, usage) = match entry_type {
        "message" => {
            let message = entry.get("message").filter(|m| m.is_object())?;
            let usage = message.get("usage").filter(|u| u.is_object())?;
            match load::str_get(message, &["role"]) {
                Some("assistant") => ("assistant", Some(message), usage),
                Some("toolResult") => ("tool_result", Some(message), usage),
                _ => return None,
            }
        }
        "compaction" | "branch_summary" => (
            entry_type,
            None,
            entry.get("usage").filter(|u| u.is_object())?,
        ),
        _ => return None,
    };
    let input = load::u64_get(usage, &["input"]);
    let output = load::u64_get(usage, &["output"]);
    let cache_read = load::u64_get(usage, &["cacheRead"]);
    let cache_write = load::u64_get(usage, &["cacheWrite"]);
    if input == 0 && output == 0 && cache_read == 0 && cache_write == 0 {
        return None;
    }
    let model = if let Some(message) = message.filter(|_| kind == "assistant") {
        nonempty_str(message, &["responseModel"])
            .or_else(|| nonempty_str(message, &["model"]))
            .unwrap_or("unknown")
            .to_string()
    } else {
        "unknown".to_string()
    };
    let created_at = entry
        .get("timestamp")
        .and_then(load::timestamp_to_epoch)
        .or_else(|| {
            message
                .and_then(|m| m.get("timestamp"))
                .and_then(load::timestamp_to_epoch)
        })
        .or(header_ts)
        .unwrap_or_else(load::now_epoch);
    let key = match load::str_get(entry, &["id"]).filter(|s| !s.is_empty()) {
        Some(id) => format!("id:{kind}:{id}"),
        None => format!("hash:{kind}:{}", content_hash(entry, usage)),
    };
    // pi 自报本轮聚合成本(USD), >0 时无条件优先于定价表
    let self_cost = load::cost_get(usage, &["cost", "total"]);
    Some((
        key,
        UsageEntry::new(
            AppKind::Pi,
            model,
            Some(session_id.to_string()),
            created_at,
            input,
            output,
            cache_read,
            cache_write,
            self_cost,
        ),
    ))
}

fn nonempty_str<'a>(value: &'a Value, keys: &[&str]) -> Option<&'a str> {
    load::str_get(value, keys).filter(|s| !s.trim().is_empty())
}

/// entry.id 缺失时的内容哈希去重
///
/// serde_json 默认用 BTreeMap 存对象, 序列化与哈希顺序确定;
/// 单次运行内去重即可, 无需跨进程稳定, 故用 std 哈希不加 sha2 依赖
fn content_hash(entry: &Value, usage: &Value) -> u64 {
    let mut hasher = DefaultHasher::new();
    if let Some(ts) = entry.get("timestamp") {
        ts.hash(&mut hasher);
    }
    usage.hash(&mut hasher);
    hasher.finish()
}

#[cfg(test)]
#[path = "tests/pi_test.rs"]
mod tests;
