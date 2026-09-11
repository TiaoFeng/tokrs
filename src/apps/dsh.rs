//! dsh 日志解析
//!
//! 数据源: `$DSH_HOME(默认 ~/.dsh)/sessions/<编码工作区>/session-<UUID>/session.jsonl.zstd`
//! zstd 压缩 JSONL, 经 io/load 的 zstd 变体流式解压逐行解析(峰值 O(单行), 进度按压缩字节推进)
//! 事件信封: {type, seq, time(epoch ms), data}; 只统计 type=="assistant/message" 事件:
//! - usage 双发(assistant/chunk 的 usage chunk 与 assistant/message 各一份, 值相同):
//!   只计 assistant/message 即官方口径——chunk 预发布无 id/model, 且行内不含
//!   "assistant/message" 字面量, needle 预过滤直接挡掉, 防双算零成本
//! - inputTokens/cacheReadTokens/cacheWriteTokens 互不包含: input 为 fresh
//!   (不经 fresh_input, 对齐 claude/kimi/pi); reasoningTokens 是 outputTokens
//!   的子集(output 已含, 不另加, kimi "thinking 已并入 output" 同款)
//! - model: data.message.source.model(消息级) > 最近一次 model/selection
//!   (会话级持久化, codex turn_context 式) > unknown; 经 apps::normalize_model 归一
//! - 去重键: message.id(缺失回退整行内容哈希, pi 同款), first-wins(kimi 同款);
//!   无自报成本, 交由定价表估价
//! - 首条有效 JSON 须为 type=="session" header(load 首行不过滤规则保障, pi 同款),
//!   id 作 session_id, createdAt 作时间戳回退; 截断/损坏文件警告后保留已解析条目
//!   (claude/kimi 式: 会话在写截断属常态, 全量重扫每次独立无跨次双算)
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

const MAX_DEPTH: usize = 2;

/// 行级预过滤 needle: 入账行(type=="assistant/message")与影响状态的
/// model/selection 行各自的定值 type 字面量(不含转义), 其余零分配跳过;
/// session header 不含两者, 由 load 的首行不过滤规则保障 header 校验
const DSH_LINE_NEEDLES: [&str; 2] = ["\"assistant/message\"", "\"model/selection\""];

/// dsh 数据根(官方 DSH_HOME > ~/.dsh); 环境变量经 load::env_abs_path
/// 归一(~/ 展开, 非绝对警告后回退默认)
fn dsh_base(home: &Path, env: Option<&OsStr>) -> PathBuf {
    load::env_abs_path("DSH_HOME", env, home).unwrap_or_else(|| home.join(".dsh"))
}

pub fn collect(threads: Option<usize>) -> Result<Vec<UsageEntry>, AppError> {
    let base =
        dsh_base(&load::home_dir()?, std::env::var_os("DSH_HOME").as_deref()).join("sessions");
    if !base.is_dir() {
        return Ok(Vec::new());
    }
    collect_from_with(&base, threads)
}

fn collect_from_with(base: &Path, threads: Option<usize>) -> Result<Vec<UsageEntry>, AppError> {
    // 预筛真正解析的文件(只认 session.jsonl.zstd), 总字节数(压缩)供进度条按字节推进
    let files: Vec<PathBuf> = load::discover_files(base, "zstd", MAX_DEPTH)
        .into_iter()
        .filter(|f| load::file_name_str(f) == "session.jsonl.zstd")
        .collect();
    let threads = threads.unwrap_or_else(|| load::auto_threads(files.len()));
    let progress = Progress::start("dsh", load::total_bytes(&files), files.len());
    // 并行逐文件解析(header 校验/current_model/内容签名均为文件内状态);
    // 单文件解压/读取失败警告+err 计数后保留已解析条目, 不中止全局
    let per_file: Vec<HashMap<String, UsageEntry>> =
        load::map_files(&files, threads, &progress, parse_session);
    progress.finish();
    // 合并: 按文件序 first-wins(message.id 全局唯一, 副本先到者入账, kimi 同款;
    // files 确定性排序, 与串行单全局 map 逐字一致)
    let mut candidates: HashMap<String, UsageEntry> = HashMap::new();
    for map in per_file {
        for (key, entry) in map {
            candidates.entry(key).or_insert(entry);
        }
    }
    Ok(candidates.into_values().collect())
}

/// 解析单个 session.jsonl.zstd: 返回该文件的去重表(message.id|内容哈希 → entry)
fn parse_session(file: &Path, progress: &Progress) -> HashMap<String, UsageEntry> {
    let mut candidates: HashMap<String, UsageEntry> = HashMap::new();
    let mut first_seen = false;
    let mut session_id = "unknown".to_string();
    let mut header_ts: Option<i64> = None;
    // 会话级当前模型: 最近一次 model/selection 持久化, 后续无 model 消息沿用
    // (codex turn_context 式); 消息级 source.model 优先
    let mut current_model: Option<String> = None;
    // 单文件解压/读取失败警告+err 计数后保留已解析条目(不中止全局);
    // 逐行流式解压防巨文件整读驻留
    if let Err(e) = load::for_each_jsonl_zstd_progress(file, &DSH_LINE_NEEDLES, progress, |entry| {
        if !first_seen {
            first_seen = true;
            if load::str_get(&entry, &["type"]) != Some("session") {
                return false;
            }
            session_id = load::str_get(&entry, &["id"])
                .unwrap_or("unknown")
                .to_string();
            header_ts = entry.get("createdAt").and_then(load::timestamp_to_epoch);
            return true;
        }
        match load::str_get(&entry, &["type"]) {
            // 会话级当前模型持久化(仅更新状态, 不入账)
            Some("model/selection") => {
                if let Some(m) = load::str_get(&entry, &["data", "model"]) {
                    current_model = Some(normalize_model(m));
                }
            }
            Some("assistant/message") => {
                if let Some((key, record)) =
                    parse_entry(&entry, &session_id, header_ts, current_model.as_deref())
                {
                    // first-wins: 同 message.id 视为副本(恢复/fork 复制), 先到者入账
                    candidates.entry(key).or_insert(record);
                }
            }
            _ => {}
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
    current_model: Option<&str>,
) -> Option<(String, UsageEntry)> {
    let data = entry.get("data").filter(|d| d.is_object())?;
    let usage = data.get("usage").filter(|u| u.is_object())?;
    let input = load::u64_get(usage, &["inputTokens"]);
    let output = load::u64_get(usage, &["outputTokens"]);
    let cache_read = load::u64_get(usage, &["cacheReadTokens"]);
    let cache_write = load::u64_get(usage, &["cacheWriteTokens"]);
    // reasoningTokens 已含于 outputTokens, 不另加(kimi 同款); 四项全零跳过
    if input == 0 && output == 0 && cache_read == 0 && cache_write == 0 {
        return None;
    }
    // model 回退链: 消息级 source.model > 会话级 model/selection > unknown;
    // current_model 已归一, normalize_model 幂等可重复施加
    let model = load::str_get(data, &["message", "source", "model"])
        .or(current_model)
        .map_or_else(|| "unknown".to_string(), normalize_model);
    let created_at = entry
        .get("time")
        .and_then(load::timestamp_to_epoch)
        .or(header_ts)
        .unwrap_or_else(load::now_epoch);
    let key = match load::str_get(data, &["message", "id"]).filter(|s| !s.is_empty()) {
        Some(id) => format!("id:{id}"),
        None => format!("hash:{}", value_hash(entry)),
    };
    Some((
        key,
        UsageEntry::new(
            AppKind::Dsh,
            model,
            Some(session_id.to_string()),
            created_at,
            input,
            output,
            cache_read,
            cache_write,
            None,
        ),
    ))
}

/// 测试便捷入口(auto 线程); 生产路径经 collect(threads)
#[cfg(test)]
pub fn collect_from(base: &Path) -> Result<Vec<UsageEntry>, AppError> {
    collect_from_with(base, None)
}

#[cfg(test)]
#[path = "tests/dsh_test.rs"]
mod tests;
