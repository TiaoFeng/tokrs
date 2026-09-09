//! gemini日志解析
//!
//! 数据源: ~/.gemini/tmp/<project>/chats/session-*.json
//! 每个文件是单个 JSON 对象(非 JSONL), 含 messages 数组
//!
//! 流式解析: serde_json Deserializer::from_reader(IoRead 逐块) + 自定义 Visitor,
//! messages 数组逐条取出瞬态处理(峰值内存 O(单条消息), 不整读 DOM, 文件可达任意大),
//! 其余字段 IgnoredAny 跳过; ProgressReader 按阈值批报字节(Drop 冲账)保持
//! 进度条字节驱动
//! sessionId 与 messages 的键序不假设: 先文件内 staging(HashMap<msg_key, entry>),
//! 流读完统一补 session_id 并拼前缀入全局表, 去重语义与整读版逐字一致
//! 只统计 type=="gemini" 的消息; thoughts 并入 output; input 含 cached 已扣除归一
//! 按消息 id last-wins 去重(缺 id 用内容哈希兜底, pi 同款)
//! 损坏/不可读文件: 警告并整体不计(staging 丢弃), 不中断其它文件
//! 参考: cc-switch session_usage_gemini.rs
//!
use serde::de::{
    DeserializeSeed, Deserializer, IgnoredAny, MapAccess, SeqAccess, Visitor as DeVisitor,
};
use serde_json::Value;
use std::{collections::HashMap, fs, path::Path};

use crate::{
    apps::{fresh_input, normalize_model, value_hash},
    error::{AppError, io_err, json_err},
    io::{load, progress::Progress},
    model::{AppKind, UsageEntry},
};

const MAX_DEPTH: usize = 3;

pub fn collect(threads: Option<usize>) -> Result<Vec<UsageEntry>, AppError> {
    let base = load::home_dir()?.join(".gemini").join("tmp");
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
    let mut files = Vec::new();
    for file in load::discover_files(base, "json", MAX_DEPTH) {
        let name = load::file_name_str(&file);
        if name.starts_with("session-") && name.ends_with(".json") {
            files.push(file);
        }
    }
    let threads = threads.unwrap_or_else(|| load::auto_threads(files.len()));
    let progress = Progress::start("gemini", load::total_bytes(&files), files.len());
    // 并行逐文件流式解析(文件内 staging, 峰值 O(单条消息));
    // 损坏/不可读文件警告+err 计数后整体不计(staging 丢弃), 不中断其它文件
    let per_file: Vec<(String, HashMap<String, UsageEntry>)> = load::map_files(
        &files,
        threads,
        &progress,
        |file, progress| match stream_session(file, progress) {
            Ok(staged) => staged,
            Err(e) => {
                load::warn_file(&e);
                progress.note_error();
                ("unknown".to_string(), HashMap::new())
            }
        },
    );
    progress.finish();
    // 合并: 按文件序 last-wins(与串行单全局 map 逐字一致, files 已确定性排序)
    let mut candidates: HashMap<String, UsageEntry> = HashMap::new();
    for (sid, staged) in per_file {
        for (msg_key, mut entry) in staged {
            entry.session_id = Some(sid.clone());
            candidates.insert(format!("{sid}:{msg_key}"), entry);
        }
    }
    Ok(candidates.into_values().collect())
}

/// 流式解析单个 session 文件: 顶层对象逐 key 消费, messages 数组逐条瞬态处理
///
/// 返回 (session_id, 文件内 staging): messages 先于 sessionId 出现时逐条消息
/// 尚不知道 session_id, 故先按 msg_key 收集, 流读完由调用方统一拼键合并
fn stream_session(
    file: &Path,
    progress: &Progress,
) -> Result<(String, HashMap<String, UsageEntry>), AppError> {
    let f = fs::File::open(file).map_err(|e| io_err("open", file, e))?;
    // 64KB 缓冲对齐 JSONL 读取端(load::for_each_jsonl_impl): GB 级文件摊薄 syscall
    let reader = load::ProgressReader::new(
        std::io::BufReader::with_capacity(64 * 1024, f),
        progress.clone(),
    );
    let mut de = serde_json::Deserializer::from_reader(reader);
    let mut session_id: Option<String> = None;
    let mut staged: HashMap<String, UsageEntry> = HashMap::new();
    de.deserialize_any(SessionVisitor {
        session_id: &mut session_id,
        staged: &mut staged,
    })
    .map_err(|e| json_err(file, e))?;
    // 至此完整解析成功; 非 gemini 消息/全零消息不会进入 staging
    let sid = session_id.unwrap_or_else(|| "unknown".to_string());
    Ok((sid, staged))
}

/// 顶层对象 visitor: 只保留 sessionId, messages 逐条瞬态处理, 其余字段忽略
struct SessionVisitor<'a> {
    session_id: &'a mut Option<String>,
    staged: &'a mut HashMap<String, UsageEntry>,
}

impl<'de> DeVisitor<'de> for SessionVisitor<'_> {
    type Value = ();

    fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("a gemini session object")
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<(), A::Error> {
        // 键序不假设: sessionId 在 messages 之后也生效; 同名键后值覆盖前值(对齐
        // serde_json Value 整读语义); 重复 messages 数组均处理(实际文件不出现)
        while let Some(key) = map.next_key::<String>()? {
            match key.as_str() {
                "sessionId" => {
                    *self.session_id = map.next_value::<Value>()?.as_str().map(str::to_string);
                }
                "messages" => map.next_value_seed(MessagesSeed {
                    staged: self.staged,
                })?,
                _ => {
                    map.next_value::<IgnoredAny>()?;
                }
            }
        }
        Ok(())
    }
}

/// messages 数组值的 seed: deserialize_seq 逐条取出元素, 单条瞬态解析后即释放
struct MessagesSeed<'a> {
    staged: &'a mut HashMap<String, UsageEntry>,
}

impl<'de> DeserializeSeed<'de> for MessagesSeed<'_> {
    type Value = ();

    fn deserialize<D: serde::Deserializer<'de>>(self, deserializer: D) -> Result<(), D::Error> {
        deserializer.deserialize_seq(self)
    }
}

impl<'de> DeVisitor<'de> for MessagesSeed<'_> {
    type Value = ();

    fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("an array of messages")
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<(), A::Error> {
        // 单条消息瞬态 Value: 处理完即释放, 峰值内存 O(单条); 非数组 messages
        // (null/对象/字符串)触发 invalid_type 错误, 文件整体跳过并警告
        while let Some(msg) = seq.next_element::<Value>()? {
            if let Some((msg_key, entry)) = parse_message(&msg) {
                self.staged.insert(msg_key, entry); // 同 key 后者覆盖(last-wins)
            }
        }
        Ok(())
    }
}

/// 解析单条消息: 返回 (msg_key, entry); session_id 后置(staging 阶段未知),
/// entry.session_id 暂为 None, 由 stream_session 统一回填
fn parse_message(msg: &Value) -> Option<(String, UsageEntry)> {
    if load::str_get(msg, &["type"]) != Some("gemini") {
        return None;
    }
    let input = load::u64_get(msg, &["tokens", "input"]);
    let output = load::u64_get(msg, &["tokens", "output"]);
    let cached = load::u64_get(msg, &["tokens", "cached"]);
    let thoughts = load::u64_get(msg, &["tokens", "thoughts"]);
    // 任一 token>0 才导入(纯缓存命中也保留)
    if input == 0 && output == 0 && cached == 0 && thoughts == 0 {
        return None;
    }
    // gemini 的 input 含 cached, 归一为 fresh input
    let input = fresh_input(input, cached, 0);
    // 去重键: msg.id(同 id last-wins); 缺 id 用完整消息内容哈希兜底(pi 同款):
    // 任何内容差异都各自计数, 不折叠进固定 "unknown" 键互相覆盖(漏计),
    // 字节级相同的消息仍去重
    let msg_key = match load::str_get(msg, &["id"]).filter(|s| !s.is_empty()) {
        Some(id) => id.to_string(),
        None => format!("hash:{}", value_hash(msg)),
    };
    let model =
        load::str_get(msg, &["model"]).map_or_else(|| "unknown".to_string(), normalize_model);
    let created_at = msg
        .get("timestamp")
        .and_then(load::timestamp_to_epoch)
        .unwrap_or_else(load::now_epoch);
    // gemini 无自报成本, 待定价表估价; 思考 token 按输出计费, 并入 output
    Some((
        msg_key,
        UsageEntry::new(
            AppKind::Gemini,
            model,
            None,
            created_at,
            input,
            output + thoughts,
            cached,
            0,
            None,
        ),
    ))
}

#[cfg(test)]
#[path = "tests/gemini_test.rs"]
mod tests;
