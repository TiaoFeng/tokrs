//! pi日志解析
//!
//! 数据源: $PI_CODING_AGENT_SESSION_DIR 或 ~/.pi/agent/sessions 或 ~/.pi/sessions 下的 *.jsonl
//! 每个会话文件首条有效 JSON 必须是 type=="session" header, 否则整文件跳过
//! 统计 assistant/toolResult 消息与 compaction/branch_summary 条目的 usage
//! 去重键: 有 entry.id 用 kind:entry.id(last-wins), 否则用整条条目内容哈希(完整 entry JSON)
//! 参考: cc-switch session_usage_pi.rs (增量游标/接管状态机等有状态逻辑不适用本工具)
//!
use serde_json::Value;
use std::{
    collections::HashMap,
    ffi::OsStr,
    path::{Path, PathBuf},
};

use crate::{
    apps::{normalize_model, value_hash},
    error::AppError,
    io::{load, progress::Progress},
    model::{AppKind, UsageEntry},
};

const MAX_DEPTH: usize = 4;

/// 行级预过滤 needle: 候选条目(assistant/toolResult 消息与 compaction/
/// branch_summary)必含 usage 对象, 其余零分配跳过(对齐 codex 同款机制);
/// session header 不含 usage, 由 load 的首行不过滤规则保障 header 校验语义
const PI_LINE_NEEDLES: [&str; 1] = ["\"usage\""];

/// pi 会话根目录链(环境变量经 load::env_abs_path 归一, ~/ 展开, 非绝对警告回退):
/// PI_CODING_AGENT_SESSION_DIR > $PI_CODING_AGENT_DIR/sessions > ~/.pi/agent/sessions;
/// ~/.pi/sessions 为旧布局兜底(tokrs 保留, cc-switch 无此层); 多根重叠由
/// collect_from 的 seen_files 去重
fn session_roots(
    home: &Path,
    session_env: Option<&OsStr>,
    agent_env: Option<&OsStr>,
) -> Vec<PathBuf> {
    let agent_root = load::env_abs_path("PI_CODING_AGENT_DIR", agent_env, home)
        .unwrap_or_else(|| home.join(".pi").join("agent"));
    let mut roots = Vec::new();
    if let Some(p) = load::env_abs_path("PI_CODING_AGENT_SESSION_DIR", session_env, home) {
        roots.push(p);
    }
    roots.push(agent_root.join("sessions"));
    roots.push(home.join(".pi").join("sessions"));
    roots
}

pub fn collect(threads: Option<usize>) -> Result<Vec<UsageEntry>, AppError> {
    let home = load::home_dir()?;
    let roots = session_roots(
        &home,
        std::env::var_os("PI_CODING_AGENT_SESSION_DIR").as_deref(),
        std::env::var_os("PI_CODING_AGENT_DIR").as_deref(),
    );
    collect_from_with(&roots, threads)
}

/// 测试便捷入口(auto 线程); 生产路径经 collect(threads)
#[cfg(test)]
pub fn collect_from(roots: &[PathBuf]) -> Result<Vec<UsageEntry>, AppError> {
    collect_from_with(roots, None)
}

fn collect_from_with(
    roots: &[PathBuf],
    threads: Option<usize>,
) -> Result<Vec<UsageEntry>, AppError> {
    let mut seen_files = std::collections::HashSet::new();
    let mut files: Vec<PathBuf> = Vec::new();
    for root in roots {
        for file in load::discover_files(root, "jsonl", MAX_DEPTH) {
            // 多根目录可能重叠(如环境变量指向默认路径), 同文件只解析一次
            if seen_files.insert(file.clone()) {
                files.push(file);
            }
        }
    }
    let threads = threads.unwrap_or_else(|| load::auto_threads(files.len()));
    let progress = Progress::start("pi", load::total_bytes(&files), files.len());
    // 并行逐文件解析(header 检查/session_id/时间戳回退均为文件内状态);
    // 单文件读取失败警告+err 计数后该文件不计, 不中止全局
    let per_file: Vec<HashMap<String, UsageEntry>> =
        load::map_files(&files, threads, &progress, parse_session);
    progress.finish();
    // 合并: 按文件序 last-wins(fork/恢复的重复条目去重, files 确定性排序,
    // 与串行单全局 map 逐字一致)
    let mut candidates: HashMap<String, UsageEntry> = HashMap::new();
    for map in per_file {
        for (key, entry) in map {
            candidates.insert(key, entry);
        }
    }
    Ok(candidates.into_values().collect())
}

/// 解析单个会话文件: 返回该文件的去重表(kind:entry.id|内容哈希 → entry)
fn parse_session(file: &Path, progress: &Progress) -> HashMap<String, UsageEntry> {
    let mut candidates: HashMap<String, UsageEntry> = HashMap::new();
    let mut first_seen = false;
    let mut session_id = "unknown".to_string();
    let mut header_ts: Option<i64> = None;
    // 首条有效 JSON 必须是 session header(畸形行已被流式过滤, 对齐参考实现);
    // 首条非 header 则整文件跳过(回调返回 false 提前终止)
    // 单文件读取失败警告+err 计数后跳过(不中止全局); 逐行流式防 GB 级文件整读驻留
    if let Err(e) = load::for_each_jsonl_progress(file, &PI_LINE_NEEDLES, progress, |entry| {
        if !first_seen {
            first_seen = true;
            if load::str_get(&entry, &["type"]) != Some("session") {
                return false;
            }
            session_id = load::str_get(&entry, &["id"])
                .unwrap_or("unknown")
                .to_string();
            header_ts = entry.get("timestamp").and_then(load::timestamp_to_epoch);
            return true;
        }
        if let Some((key, record)) = parse_entry(&entry, &session_id, header_ts) {
            candidates.insert(key, record);
        }
        true
    }) {
        load::warn_file(&e);
        progress.note_error();
    }
    candidates
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
    // pi 自报本轮聚合成本(USD), >0 时无条件优先于定价表
    let self_cost = load::cost_get(usage, &["cost", "total"]);
    // token 全零但有真实扣费(如失败仍计价的请求)时保留
    if input == 0 && output == 0 && cache_read == 0 && cache_write == 0 && self_cost.is_none() {
        return None;
    }
    let model = if let Some(message) = message.filter(|_| kind == "assistant") {
        load::str_get_nonempty(message, &["responseModel"])
            .or_else(|| load::str_get_nonempty(message, &["model"]))
            .map_or_else(|| "unknown".to_string(), normalize_model)
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
        None => format!("hash:{kind}:{}", value_hash(entry)),
    };
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

#[cfg(test)]
#[path = "tests/pi_test.rs"]
mod tests;
