//! claude日志解析
//!
use serde_json::Value;
use std::{
    collections::{HashMap, hash_map::Entry},
    ffi::OsStr,
    path::{Path, PathBuf},
};

use crate::{
    apps::normalize_model,
    error::AppError,
    io::{load, progress::Progress},
    model::{AppKind, UsageEntry},
};

const MAX_DEPTH: usize = 5;

/// claude 数据根(官方 CLAUDE_CONFIG_DIR > ~/.claude); 环境变量经 load::env_abs_path
/// 归一(~/ 展开, 非绝对警告后回退默认)
fn claude_base(home: &Path, env: Option<&OsStr>) -> PathBuf {
    load::env_abs_path("CLAUDE_CONFIG_DIR", env, home).unwrap_or_else(|| home.join(".claude"))
}

pub fn collect(threads: Option<usize>) -> Result<Vec<UsageEntry>, AppError> {
    let home = load::home_dir()?;
    let base =
        claude_base(&home, std::env::var_os("CLAUDE_CONFIG_DIR").as_deref()).join("projects");
    if !base.is_dir() {
        return Ok(Vec::new());
    }
    collect_from_with(&base, threads)
}

/// 测试便捷入口(auto 线程); 生产路径经 collect(threads)
#[cfg(test)]
pub fn collect_from(base: &Path) -> Result<Vec<UsageEntry>, AppError> {
    collect_from_with(base, None)
}

fn collect_from_with(base: &Path, threads: Option<usize>) -> Result<Vec<UsageEntry>, AppError> {
    let files = load::discover_files(base, "jsonl", MAX_DEPTH);
    let threads = threads.unwrap_or_else(|| load::auto_threads(files.len()));
    let progress = Progress::start("claude", load::total_bytes(&files), files.len());
    // 并行逐文件流式解析(session_fallback/候选规则均为文件内状态);
    // 单文件读取失败警告+err 计数后跳过, 不中止全局统计
    let per_file: Vec<HashMap<String, Candidate>> =
        load::map_files(&files, threads, &progress, |file, progress| {
            let mut candidates: HashMap<String, Candidate> = HashMap::new();
            let mut session_fallback: Option<String> = None;
            if let Err(e) = load::for_each_jsonl_progress(file, &[], progress, |value| {
                if session_fallback.is_none() {
                    session_fallback = load::str_get(&value, &["sessionId"]).map(str::to_string);
                }
                parse_assistant_line(&value, session_fallback.as_deref(), &mut candidates);
                true
            }) {
                load::warn_file(&e);
                progress.note_error();
            }
            candidates
        });
    progress.finish();
    // 跨文件合并: 取代规则为 max 语义(stop_reason 优先/同级 output 取大),
    // 交换律——任意合并序与串行全局 HashMap 逐字一致
    let mut merged: HashMap<String, Candidate> = HashMap::new();
    for candidates in per_file {
        for (msg_id, candidate) in candidates {
            match merged.entry(msg_id) {
                Entry::Vacant(vacant) => {
                    vacant.insert(candidate);
                }
                Entry::Occupied(mut occupied) => {
                    if candidate_should_replace(&candidate, occupied.get()) {
                        occupied.insert(candidate);
                    }
                }
            }
        }
    }
    Ok(merged.into_values().map(|c| c.entry).collect())
}

/// 取代规则: 有 stop_reason 优先; 同级取 output 大者
fn candidate_should_replace(new: &Candidate, old: &Candidate) -> bool {
    (new.has_stop_reason && !old.has_stop_reason)
        || (new.has_stop_reason == old.has_stop_reason
            && new.entry.output_tokens > old.entry.output_tokens)
}

struct Candidate {
    entry: UsageEntry,
    has_stop_reason: bool,
}

fn parse_assistant_line(
    value: &Value,
    session_fallback: Option<&str>,
    candidates: &mut HashMap<String, Candidate>,
) {
    if load::str_get(value, &["type"]) != Some("assistant") {
        return;
    }
    let Some(message) = value.get("message") else {
        return;
    };
    let Some(msg_id) = load::str_get(message, &["id"]) else {
        return;
    };
    let input = load::u64_get(message, &["usage", "input_tokens"]);
    let output = load::u64_get(message, &["usage", "output_tokens"]);
    let cache_read = load::u64_get(message, &["usage", "cache_read_input_tokens"]);
    let cache_creation = load::u64_get(message, &["usage", "cache_creation_input_tokens"]);
    if input == 0 && output == 0 && cache_read == 0 && cache_creation == 0 {
        return;
    }
    let model =
        load::str_get(message, &["model"]).map_or_else(|| "unknown".to_string(), normalize_model);
    let session_id = load::str_get(value, &["sessionId"])
        .or(session_fallback)
        .map(str::to_string);
    let created_at = value
        .get("timestamp")
        .and_then(load::timestamp_to_epoch)
        .unwrap_or_else(load::now_epoch);
    // claude 无自报成本, 待定价表估价
    let entry = UsageEntry::new(
        AppKind::Claude,
        model,
        session_id,
        created_at,
        input,
        output,
        cache_read,
        cache_creation,
        None,
    );
    let candidate = Candidate {
        has_stop_reason: load::str_get(message, &["stop_reason"]).is_some(),
        entry,
    };
    match candidates.entry(msg_id.to_string()) {
        Entry::Vacant(vacant) => {
            vacant.insert(candidate);
        }
        Entry::Occupied(mut occupied) => {
            if candidate_should_replace(&candidate, occupied.get()) {
                occupied.insert(candidate);
            }
        }
    }
}

#[cfg(test)]
#[path = "tests/claude_test.rs"]
mod tests;
