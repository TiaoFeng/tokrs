//! gemini日志解析
//!
//! 数据源: ~/.gemini/tmp/<project>/chats/session-*.json
//! 每个文件是单个 JSON 对象(非 JSONL), 含 messages 数组
//! 只统计 type=="gemini" 的消息; thoughts 并入 output; input 含 cached 已扣除归一
//! 按消息 id last-wins 去重(缺 id 用内容哈希兜底, pi 同款)
//! 参考: cc-switch session_usage_gemini.rs
//!
use serde_json::Value;
use std::{
    collections::{HashMap, hash_map::DefaultHasher},
    hash::{Hash, Hasher},
    path::Path,
};

use crate::{
    apps::{fresh_input, normalize_model},
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
        // 去重键: sessionId:msg.id(同 id last-wins, 对应 cc-switch 的 UPSERT 语义);
        // 缺 id 用完整消息内容哈希兜底(pi 同款): 任何内容差异都各自计数,
        // 不折叠进固定 "unknown" 键互相覆盖(漏计), 字节级相同的消息仍去重
        let msg_key = match load::str_get(msg, &["id"]).filter(|s| !s.is_empty()) {
            Some(id) => id.to_string(),
            None => format!("hash:{}", content_hash(msg)),
        };
        let dedup_key = format!("{}:{msg_key}", session_id.as_deref().unwrap_or("unknown"));
        let model =
            load::str_get(msg, &["model"]).map_or_else(|| "unknown".to_string(), normalize_model);
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

/// msg.id 缺失时的内容哈希兜底(pi 同款)
///
/// 哈希完整消息 JSON: 任何内容差异(响应文本/额外字段)都各自计数, 修复固定
/// "unknown" 键折叠漏计; 字节级相同的消息(含跨文件同 sessionId 副本)仍去重.
/// serde_json 默认用 BTreeMap 存对象, 序列化与哈希顺序确定;
/// DefaultHasher::new() 固定 key(0,0), 跨进程结果稳定;
/// 本工具全量重扫, 单次运行内去重即足够
fn content_hash(msg: &Value) -> u64 {
    let mut hasher = DefaultHasher::new();
    msg.hash(&mut hasher);
    hasher.finish()
}

#[cfg(test)]
#[path = "tests/gemini_test.rs"]
mod tests;
