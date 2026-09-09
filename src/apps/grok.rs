//! grok日志解析
//!
//! 数据源: ~/.grok/{sessions,archived_sessions}/<enc-cwd>/<session-id>/updates.jsonl
//! 只统计 turn_completed 事件; usage 是逐轮独立总量, 按面值入账(禁差分, 差分致巨量漏记)
//! reasoningTokens 已含于 outputTokens 不另计; inputTokens 含 cachedRead 已扣除归一
//! costUsdTicks 待定价模块处理
//! 参考: cc-switch session_usage_grokbuild.rs
//!
use serde_json::Value;
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
};

use crate::{
    apps::{fresh_input, normalize_model},
    error::AppError,
    io::{load, progress::Progress},
    model::{AppKind, UsageEntry},
};

const MAX_DEPTH: usize = 4;

pub fn collect() -> Result<Vec<UsageEntry>, AppError> {
    let base = load::home_dir()?.join(".grok");
    if !base.is_dir() {
        return Ok(Vec::new());
    }
    collect_from(&base)
}

pub fn collect_from(base: &Path) -> Result<Vec<UsageEntry>, AppError> {
    // 预筛真正解析的文件(只认 updates.jsonl), 总字节数供进度条按字节推进
    let mut files: Vec<PathBuf> = Vec::new();
    for root in ["sessions", "archived_sessions"] {
        files.extend(load::discover_files(&base.join(root), "jsonl", MAX_DEPTH));
    }
    files.retain(|f| load::file_name_str(f) == "updates.jsonl");
    // 去重键: session_id:prompt_id(缺失时为事件序号):model, 后出现者覆盖(UPSERT 语义)
    let mut candidates: HashMap<String, UsageEntry> = HashMap::new();
    let mut progress = Progress::start("grok", load::total_bytes(&files));
    for file in &files {
        progress.set_file(load::file_name_str(file));
        parse_updates(file, &mut progress, &mut candidates);
    }
    progress.finish();
    Ok(candidates.into_values().collect())
}

fn parse_updates(
    file: &Path,
    progress: &mut Progress,
    candidates: &mut HashMap<String, UsageEntry>,
) {
    // 会话 ID = updates.jsonl 的父目录名(UUIDv7, 全局唯一)
    let session_id = file
        .parent()
        .and_then(|d| d.file_name())
        .and_then(|n| n.to_str())
        .unwrap_or("unknown");
    // 序号只对有效的用量事件递增(对齐 cc-switch 的 events 下标)
    let mut event_index = 0usize;
    // 单文件读取失败警告后跳过(不中止全局); 逐行流式防 GB 级文件整读驻留
    if let Err(e) = load::for_each_jsonl_progress(file, &[], progress, |record| {
        if load::str_get(&record, &["method"]) != Some("_x.ai/session/update") {
            return true;
        }
        let Some(update) = record.get("params").and_then(|p| p.get("update")) else {
            return true;
        };
        // 显式标为其它类型(如 usage_snapshot)即使带 usage 也跳过, 防中途快照双算;
        // 字段缺失则向后兼容放行
        let kind = load::str_get(update, &["sessionUpdate"]);
        if kind.is_some() && kind != Some("turn_completed") {
            return true;
        }
        let Some(usage) = update.get("usage").filter(|u| u.is_object()) else {
            return true;
        };
        // 没有时间戳的事件无法归入任何日期, 直接跳过
        let Some(created_at) = record.get("timestamp").and_then(load::timestamp_to_epoch) else {
            return true;
        };
        let prompt_id = load::str_get(update, &["prompt_id"]).unwrap_or("");
        let turn_key = if prompt_id.is_empty() {
            format!("idx{event_index}")
        } else {
            prompt_id.to_string()
        };
        event_index += 1;
        // 事件级 costIsPartial 对该事件全部模型生效
        let event_partial = load::bool_get(usage, &["costIsPartial"]);
        for (model, counters) in per_model(usage) {
            let input = load::u64_get(counters, &["inputTokens"]);
            let output = load::u64_get(counters, &["outputTokens"]);
            let cached = load::u64_get(counters, &["cachedReadTokens"]);
            // CLI 自报本轮成本, 1 tick = 1e-10 USD;
            // costIsPartial=true 表示自报仅为下界, 不可信, 不采自报(交由定价表估价)
            let ticks = load::u64_get(counters, &["costUsdTicks"]);
            let partial = event_partial || load::bool_get(counters, &["costIsPartial"]);
            let self_cost = (ticks > 0 && !partial).then(|| ticks as f64 / 1e10);
            // token 全零但有可信自报成本时保留(不丢真实扣费)
            if input == 0 && output == 0 && cached == 0 && self_cost.is_none() {
                continue;
            }
            // grok 的 inputTokens 含 cachedRead, 归一为 fresh input
            let input = fresh_input(input, cached, 0);
            // 去重键保留原始 bucket 名(不同 bucket 是独立用量), 落表值统一归一化
            candidates.insert(
                format!("{session_id}:{turn_key}:{model}"),
                UsageEntry::new(
                    AppKind::Grok,
                    normalize_model(model),
                    Some(session_id.to_string()),
                    created_at,
                    input,
                    output,
                    cached,
                    0,
                    self_cost,
                ),
            );
        }
        true
    }) {
        load::warn_file(&e);
    }
}

/// 逐模型面值用量; 缺 modelUsage 时回退顶层 usage 且模型名未知
///
/// 返回按模型名排序, 保证多次扫描间插入顺序确定
fn per_model(usage: &Value) -> Vec<(&str, &Value)> {
    match usage.get("modelUsage").and_then(Value::as_object) {
        Some(map) if !map.is_empty() => {
            let mut pairs: Vec<(&str, &Value)> = map.iter().map(|(k, v)| (k.as_str(), v)).collect();
            pairs.sort_unstable_by_key(|(k, _)| *k);
            pairs
        }
        _ => vec![("unknown", usage)],
    }
}

#[cfg(test)]
#[path = "tests/grok_test.rs"]
mod tests;
