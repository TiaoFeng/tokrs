//! gemini日志解析
//!
//! 数据源: ~/.gemini/tmp/<project>/chats/session-*.json
//! 每个文件是单个 JSON 对象(非 JSONL), 含 messages 数组
//! 只统计 type=="gemini" 的消息; thoughts 并入 output; input 含 cached 已扣除归一
//! 按消息 id last-wins 去重
//! 参考: cc-switch session_usage_gemini.rs
//!
use serde_json::Value;
use std::{collections::HashMap, path::Path};

use crate::{
    apps::fresh_input,
    error::AppError,
    io::load,
    model::{AppKind, UsageEntry},
};

const MAX_DEPTH: usize = 3;

pub fn collect() -> Result<Vec<UsageEntry>, AppError> {
    let base = load::home_dir()?.join(".gemini").join("tmp");
    if !base.is_dir() {
        return Ok(Vec::new());
    }
    collect_from(&base)
}

pub fn collect_from(base: &Path) -> Result<Vec<UsageEntry>, AppError> {
    // 去重键: 消息 id, 后出现者覆盖(对应 cc-switch 的 UPSERT 语义)
    let mut candidates: HashMap<String, UsageEntry> = HashMap::new();
    for file in load::discover_files(base, "json", MAX_DEPTH) {
        let name = file.file_name().and_then(|n| n.to_str()).unwrap_or("");
        if !name.starts_with("session-") || !name.ends_with(".json") {
            continue;
        }
        // 单个文件损坏不中断整体收集
        let Ok(value) = load::read_json(&file) else {
            continue;
        };
        parse_session(&value, &mut candidates);
    }
    Ok(candidates.into_values().collect())
}

fn parse_session(value: &Value, candidates: &mut HashMap<String, UsageEntry>) {
    let session_id = load::str_get(value, &["sessionId"]).map(str::to_string);
    let Some(messages) = value.get("messages").and_then(Value::as_array) else {
        return;
    };
    for msg in messages {
        if load::str_get(msg, &["type"]) != Some("gemini") {
            continue;
        }
        let input = load::u64_get(msg, &["tokens", "input"]);
        let output = load::u64_get(msg, &["tokens", "output"]);
        let cached = load::u64_get(msg, &["tokens", "cached"]);
        let thoughts = load::u64_get(msg, &["tokens", "thoughts"]);
        // 任一 token>0 才导入(纯缓存命中也保留)
        if input == 0 && output == 0 && cached == 0 && thoughts == 0 {
            continue;
        }
        // gemini 的 input 含 cached, 归一为 fresh input
        let input = fresh_input(input, cached, 0);
        let msg_id = load::str_get(msg, &["id"]).unwrap_or("unknown");
        let dedup_key = format!("{}:{msg_id}", session_id.as_deref().unwrap_or("unknown"));
        let model = load::str_get(msg, &["model"])
            .unwrap_or("unknown")
            .to_string();
        let created_at = msg
            .get("timestamp")
            .and_then(load::timestamp_to_epoch)
            .unwrap_or_else(load::now_epoch);
        // gemini 无自报成本, 待定价表估价; 思考 token 按输出计费, 并入 output
        candidates.insert(
            dedup_key,
            UsageEntry::new(
                AppKind::Gemini,
                model,
                session_id.clone(),
                created_at,
                input,
                output + thoughts,
                cached,
                0,
                None,
            ),
        );
    }
}

#[cfg(test)]
#[path = "tests/gemini_test.rs"]
mod tests;
