//! kimi-code 日志解析
//!
//! 数据源: ~/.kimi-code/sessions/<wd_key>/<session_id>/agents/<agentId>/wire.jsonl
//! 只统计 type=="usage.record" 事件; 经实测每条是一次真实 LLM 调用的定值面值
//! (与 llm.request 一一对应, 每 step 恰好一条), 逐条相加即官方计费口径
//! inputOther 为 fresh input(不含缓存, 实测恒小于 cacheRead), thinking token 已并入 output,
//! 故不经 fresh_input 归一(与 claude 同款); kimi 无自报成本, 交由定价表估价
//! model 为完整别名(moonshot-cn/kimi-k3), 经 apps::normalize_model 统一归一(剥前缀/小写)落表
//! (全 app 统一, 同模型跨渠道合并计价; 中转渠道无法区分, 与项目现状一致)
//! 去重键: 不含 session 的内容签名(agent:time:model:usage 四元),
//! fork/恢复会逐字节复制 wire.jsonl(time/usage 保持)→签名一致天然去重, 防双算;
//! 真实失败重试的 time 毫秒不同→各自入账, 与官方二次计费一致
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

/// kimi 数据根(官方 KIMI_CODE_HOME > ~/.kimi-code, 会话/日志等随根迁移);
/// 环境变量经 load::env_abs_path 归一(~/ 展开, 非绝对警告后回退默认)
fn kimi_base(home: &Path, env: Option<&OsStr>) -> PathBuf {
    load::env_abs_path("KIMI_CODE_HOME", env, home).unwrap_or_else(|| home.join(".kimi-code"))
}

pub fn collect() -> Result<Vec<UsageEntry>, AppError> {
    let base = kimi_base(
        &load::home_dir()?,
        std::env::var_os("KIMI_CODE_HOME").as_deref(),
    )
    .join("sessions");
    if !base.is_dir() {
        return Ok(Vec::new());
    }
    collect_from(&base)
}

pub fn collect_from(base: &Path) -> Result<Vec<UsageEntry>, AppError> {
    // 预筛真正解析的文件(只认 wire.jsonl, 排除 tasks/blobs 等目录的其它 jsonl, 对齐 grok),
    // 总字节数供进度条按字节推进
    let files: Vec<PathBuf> = load::discover_files(base, "jsonl", MAX_DEPTH)
        .into_iter()
        .filter(|f| load::file_name_str(f) == "wire.jsonl")
        .collect();
    // 全局 HashMap(跨文件), 与 claude 同款: fork 复制出的副本会话才能被去重
    let mut candidates: HashMap<String, UsageEntry> = HashMap::new();
    let mut progress = Progress::start("kimi", load::total_bytes(&files));
    for file in &files {
        progress.set_file(load::file_name_str(file));
        parse_wire(file, &mut progress, &mut candidates);
    }
    progress.finish();
    Ok(candidates.into_values().collect())
}

fn parse_wire(file: &Path, progress: &mut Progress, candidates: &mut HashMap<String, UsageEntry>) {
    // 会话 ID = 路径中 "session_" 前缀的祖先目录名(仅备查, 不进 dedup 键)
    let session_id = file.ancestors().find_map(|p| {
        p.file_name()
            .and_then(|n| n.to_str())
            .filter(|n| n.starts_with("session_"))
            .map(str::to_string)
    });
    // 单文件读取失败警告后跳过(不中止全局); 逐行流式防 GB 级文件整读驻留
    if let Err(e) = load::for_each_jsonl_progress(file, &[], progress, |record| {
        if load::str_get(&record, &["type"]) != Some("usage.record") {
            return true;
        }
        // 显式标为其它 scope(如未来版本的 session 级聚合快照)即使带 usage 也跳过, 防双算;
        // 字段缺失则向后兼容放行(对齐 grok 对 sessionUpdate 的处理)
        let scope = load::str_get(&record, &["usageScope"]);
        if scope.is_some() && scope != Some("turn") {
            return true;
        }
        let Some(usage) = record.get("usage").filter(|u| u.is_object()) else {
            return true;
        };
        // 四项 token 全零的记录跳过(claude 同款); 无任何成本来源可保留
        let input = load::u64_get(usage, &["inputOther"]);
        let output = load::u64_get(usage, &["output"]);
        let cache_read = load::u64_get(usage, &["inputCacheRead"]);
        let cache_creation = load::u64_get(usage, &["inputCacheCreation"]);
        if input == 0 && output == 0 && cache_read == 0 && cache_creation == 0 {
            return true;
        }
        // model 为完整别名(moonshot-cn/kimi-k3), 全 app 统一经 normalize_model 归一
        // (剥前缀/小写/空值兜底); 同模型跨渠道合并计价, 通道区分不支持(项目现状)
        let model = load::str_get(&record, &["model"])
            .map_or_else(|| "unknown".to_string(), normalize_model);
        let created_at = record
            .get("time")
            .and_then(load::timestamp_to_epoch)
            .unwrap_or_else(load::now_epoch);
        // 毫秒原值进 dedup 键(转秒会丢失重试区分度); time 缺失时退回秒值, 保守不合并
        let time_raw = record
            .get("time")
            .and_then(Value::as_i64)
            .unwrap_or(created_at);
        let agent_id = load::str_get(&record, &["agentId"]).unwrap_or("main");
        let key =
            format!("{agent_id}:{time_raw}:{model}:{input}:{output}:{cache_read}:{cache_creation}");
        match candidates.entry(key) {
            Entry::Vacant(vacant) => {
                // kimi 无自报成本, 待定价表估价
                vacant.insert(UsageEntry::new(
                    AppKind::Kimi,
                    model,
                    session_id.clone(),
                    created_at,
                    input,
                    output,
                    cache_read,
                    cache_creation,
                    None,
                ));
            }
            // 签名一致即同一事件的 fork 副本, 面值定值记录内容相同, 保留先到者
            Entry::Occupied(_) => {}
        }
        true
    }) {
        load::warn_file(&e);
    }
}

#[cfg(test)]
#[path = "tests/kimi_test.rs"]
mod tests;
