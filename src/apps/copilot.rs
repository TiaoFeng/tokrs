//! VS Code Copilot Chat 解析
//!
//! 数据源: VS Code 聊天会话存储(ChatSessionStore)的操作日志 JSONL:
//! `~/.config/Code/User/workspaceStorage/<工作区哈希>/chatSessions/<会话ID>.jsonl`
//! (面板/agent 会话)与 `globalStorage/emptyWindowChatSessions/<会话ID>.jsonl`(空窗口会话);
//!
//! 格式为逐行操作(op)日志: `kind:0` 首行初始状态(sessionId/creationDate/requests 基底),
//! `kind:1` 设置路径值, `kind:2` 数组插入(`i` 缺省/null 追加, 否则插入到索引处);
//! 每个请求完成时由 `["requests", i, "promptTokens"/"completionTokens"]` 的 set 操作
//! 流式写入用量(末值为准), 未完成/取消的请求永不写入 -> 天然跳过.
//! 解析只追踪 requests 子路径(轻量 replay), 其余路径(inputState/customTitle/response等)一概忽略;
//! response 巨型推送行除 Auto 解析片段外不含 needle 字面量, 零解析跳过
//!
//! 语义:
//! - input 直用 promptTokens(上游未暴露缓存拆分, 无法归一, cache 恒 0,
//!   不经 fresh_input);
//! - output 为 completionTokens; 无自报成本, 交由定价表估价;
//! - copilot/auto(Auto 模式)的实际模型经响应流中 autoModeResolution 片段还原
//!   (resolved.id, 末次为准), 缺失时按 "auto" 兜底;
//! - modelId 先经 decode_model_id 解码 VS Code 标识
//!   (取模型段并剥扩展命名空间/variant 装饰)再经 apps::normalize_model 统一归一
//!
//! 去重键:
//! - requestId(缺失回退 `文件stem:索引`), first-wins(kimi 同款)——
//!   会话跨工作区复制/移动产生的副本不重复计算;
//! - 时间戳毫秒自适应转秒, 链 timestamp > responseTimestamp > 会话 creationDate(缺失视为损坏跳过);
//! - 截断/损坏行跳过并保留已解析条目
//!
use serde_json::Value;
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
};

use crate::{
    apps::{normalize_model, strip_provider},
    error::AppError,
    io::{load, progress::Progress},
    model::{AppKind, UsageEntry},
};

/// 工作区会话目录深度: `<User>/workspaceStorage/<哈希>/chatSessions/<文件>.jsonl`
const MAX_DEPTH: usize = 2;

/// 行级预过滤 needle
///
/// 五个均为结构字面量(JSON 转义保证不会命中字符串内容):
/// - 请求表推送 `["requests"]`, token/模型字段的 set 操作
///   (如 `["requests",0,"promptTokens"]`),
///   携带 Auto 解析的响应片段(`autoModeResolution`);
/// - 其余 response 巨型推送行不匹配任一 needle -> 零解析跳过.
/// - 首行 init 由 load 的首行不过滤规则保障(sessionId/creationDate 兜底依赖它)
const COPILOT_LINE_NEEDLES: [&str; 5] = [
    "[\"requests\"]",
    "\"promptTokens\"",
    "\"completionTokens\"",
    "\"modelId\"",
    "\"autoModeResolution\"",
];

/// 操作类型(操作日志的 kind 字段) 0 初始状态 / 1 设置路径值 / 2 数组插入
const OP_INIT: u64 = 0; // 初始状态
const OP_SET: u64 = 1; // 设置路径值
const OP_PUSH: u64 = 2; // 数组插入

/// copilot 数据根: VS Code stable 用户目录
///
/// 目前未支持环境变量, 仅默认路径(类似 gemini/grok);
/// Insiders/VSCodium/远端 server 的独立用户目录后续扩展
fn copilot_user(home: &Path) -> PathBuf {
    home.join(".config").join("Code").join("User")
}

/// VS Code 模型标识解码
///
/// `<vendor>[/显示名]/<模型id>` 取模型段, 再剥扩展装饰
///
/// 部分扩展把命名空间与路由变体编进模型 id(例如 `opencodego:deepseek-v4.1-flash::session-2026-05-21-b`),
/// 直接归一无法与其它渠道的同名模型对齐合并:
/// - `::` 起的变体/路由标记剥除(HF/ollama 风格的单冒号模型名不受影响)
/// - 仅当首个 `:` 前缀与标识首段(扩展 vendor)同名时剥命名空间,
///   保护 `llama3:70b` 一类合法含冒号模型名(前缀与 vendor 不同则不剥)
fn decode_model_id(raw: &str) -> String {
    let raw = raw.trim();
    let vendor = raw.split('/').next().unwrap_or(raw);
    let model = strip_provider(raw);
    let model = model.split_once("::").map_or(model, |(head, _)| head);
    match model.split_once(':') {
        Some((ns, rest)) if !rest.is_empty() && ns.eq_ignore_ascii_case(vendor) => rest.to_string(),
        _ => model.to_string(),
    }
}

pub fn collect(threads: Option<usize>) -> Result<Vec<UsageEntry>, AppError> {
    let user = copilot_user(&load::home_dir()?);
    collect_from_user(&user, threads)
}

/// 汇总两处会话文件(确定性排序): 工作区会话 + 空窗口会话
fn session_files(user: &Path) -> Vec<PathBuf> {
    // 工作区会话: workspaceStorage 下还有 chatEditingSessions 等目录, 按父目录名精确过滤
    let mut files = load::discover_files(&user.join("workspaceStorage"), "jsonl", MAX_DEPTH);
    files.retain(|f| f.parent().map(load::file_name_str) == Some("chatSessions"));
    // 空窗口会话(无工作区窗口时): globalStorage/emptyWindowChatSessions 顶层即文件
    files.extend(load::discover_files(
        &user.join("globalStorage").join("emptyWindowChatSessions"),
        "jsonl",
        0,
    ));
    files.sort();
    files
}

fn collect_from_user(user: &Path, threads: Option<usize>) -> Result<Vec<UsageEntry>, AppError> {
    let files = session_files(user);
    let threads = threads.unwrap_or_else(|| load::auto_threads(files.len()));
    let progress = Progress::start("copilot", load::total_bytes(&files), files.len());
    // 并行逐文件解析(replay/去重均为文件内状态);
    // 单文件读取失败警告+err 计数后该文件不计入统计, 不中止全局
    let per_file: Vec<HashMap<String, UsageEntry>> =
        load::map_files(&files, threads, &progress, parse_session);
    progress.finish();
    // 合并: 按文件序 first-wins
    // (会话副本先到者入账, files 确定性排序, 与串行单全局 map 一致)
    let mut candidates: HashMap<String, UsageEntry> = HashMap::new();
    for map in per_file {
        for (key, entry) in map {
            candidates.entry(key).or_insert(entry);
        }
    }
    Ok(candidates.into_values().collect())
}

/// 解析单个会话文件
///
/// 返回该文件的去重表(requestId|stem:索引 -> entry)
fn parse_session(file: &Path, progress: &Progress) -> HashMap<String, UsageEntry> {
    let stem = file
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or_default();
    let mut replay = Replay::default();
    // 单文件读取失败警告+err 计数后保留已解析条目(不中止全局);
    // 逐行流式防止巨大文件挤入内存
    if let Err(e) = load::for_each_jsonl_progress(file, &COPILOT_LINE_NEEDLES, progress, |op| {
        replay.apply(&op);
        true
    }) {
        load::warn_file(&e);
        progress.note_error();
    }
    replay.into_entries(stem)
}

/// 单会话文件的 replay 状态(只追踪 requests 子路径)
#[derive(Default)]
struct Replay {
    /// 按操作索引的请求骨架(与 VS Code 的数组操作语义一一对应)
    requests: Vec<Skeleton>,
    /// init 行会话 ID(session_id 兜底: 文件名 stem)
    session_id: Option<String>,
    /// init 行创建时间(时间戳链最终兜底)
    created: Option<i64>,
}

/// 单个请求的用量字段骨架(其余字段不读取; token 缺失按 0, 全零请求不入账)
#[derive(Default)]
struct Skeleton {
    request_id: Option<String>,
    model: Option<String>,
    /// Auto 模式实际模型(响应片段 autoModeResolution 的 resolved.id)
    resolved_model: Option<String>,
    timestamp: Option<i64>,
    response_timestamp: Option<i64>,
    prompt_tokens: u64,
    completion_tokens: u64,
}

impl Skeleton {
    /// 从请求对象提取用量字段(缺失保持默认)
    fn from_request(value: &Value) -> Self {
        let mut skeleton = Skeleton {
            request_id: load::str_get_nonempty(value, &["requestId"]).map(str::to_string),
            model: load::str_get_nonempty(value, &["modelId"]).map(str::to_string),
            resolved_model: None,
            timestamp: value.get("timestamp").and_then(load::timestamp_to_epoch),
            response_timestamp: value
                .get("responseTimestamp")
                .and_then(load::timestamp_to_epoch),
            prompt_tokens: load::u64_get(value, &["promptTokens"]),
            completion_tokens: load::u64_get(value, &["completionTokens"]),
        };
        if let Some(parts) = value.get("response").and_then(Value::as_array) {
            skeleton.absorb_response(parts);
        }
        skeleton
    }

    /// 从响应片段提取 Auto 模式解析结果(autoModeResolution, 流式追加末次为准)
    fn absorb_response(&mut self, parts: &[Value]) {
        for part in parts {
            if load::str_get(part, &["kind"]) == Some("autoModeResolution")
                && let Some(id) = load::str_get_nonempty(part, &["resolved", "id"])
            {
                self.resolved_model = Some(id.to_string());
            }
        }
    }
}

impl Replay {
    /// 应用一行操作: 只处理 requests 路径, 其余(kind/路径双重过滤下)忽略
    fn apply(&mut self, op: &Value) {
        match op.get("kind").and_then(Value::as_u64) {
            Some(OP_INIT) => self.apply_init(op),
            Some(OP_SET) => self.apply_set(op),
            Some(OP_PUSH) => self.apply_push(op),
            _ => {}
        }
    }

    /// kind 0 初始状态(`OP_INIT`): 记录会话 ID/创建时间, 并创建 requests 基底
    /// (正常为空数组, 快照恢复时可能非空)
    fn apply_init(&mut self, op: &Value) {
        let Some(state) = op.get("v") else { return };
        self.session_id = load::str_get_nonempty(state, &["sessionId"]).map(str::to_string);
        self.created = state.get("creationDate").and_then(load::timestamp_to_epoch);
        if let Some(list) = state.get("requests").and_then(Value::as_array) {
            self.requests = list.iter().map(Skeleton::from_request).collect();
        }
    }

    /// kind 1 设置路径值(OP_SET): ["requests"] 整表替换 / ["requests", i] 整请求替换 /
    /// ["requests", i, 字段] 字段更新(流式覆写, 末值为准); 后两者为防御分支
    fn apply_set(&mut self, op: &Value) {
        let Some(path) = op.get("k").and_then(Value::as_array) else {
            return;
        };
        if path.first().and_then(Value::as_str) != Some("requests") {
            return;
        }
        match path.as_slice() {
            [_] => {
                self.requests = op
                    .get("v")
                    .and_then(Value::as_array)
                    .map(|list| list.iter().map(Skeleton::from_request).collect())
                    .unwrap_or_default();
            }
            [_, idx] => {
                let Some(idx) = idx.as_u64() else { return };
                if let Some(value) = op.get("v") {
                    *self.slot(idx) = Skeleton::from_request(value);
                }
            }
            [_, idx, field] => {
                let (Some(idx), Some(field)) = (idx.as_u64(), field.as_str()) else {
                    return;
                };
                match field {
                    // 整表替换响应数组(防御分支): 吸收其中可能携带的 Auto 解析片段
                    "response" => {
                        if let Some(parts) = op.get("v").and_then(Value::as_array) {
                            self.slot(idx).absorb_response(parts);
                        }
                    }
                    _ => self.set_field(idx, field, op),
                }
            }
            _ => {}
        }
    }

    /// kind 2 数组插入(`OP_PUSH`): ["requests"] 请求表推送(i 缺省/null 追加, 否则插入到索引处);
    /// ["requests", i, "response"] 响应片段推送(仅携带 Auto 解析片段的行经 needle 到达)
    fn apply_push(&mut self, op: &Value) {
        let Some(path) = op.get("k").and_then(Value::as_array) else {
            return;
        };
        match path.as_slice() {
            [only] if only.as_str() == Some("requests") => {
                let items: Vec<Skeleton> = match op.get("v") {
                    Some(Value::Array(list)) => list.iter().map(Skeleton::from_request).collect(),
                    Some(value) => vec![Skeleton::from_request(value)],
                    None => Vec::new(),
                };
                let at = op
                    .get("i")
                    .and_then(Value::as_u64)
                    .map_or(self.requests.len(), |i| i as usize);
                let at = at.min(self.requests.len());
                for (offset, skeleton) in items.into_iter().enumerate() {
                    self.requests.insert(at + offset, skeleton);
                }
            }
            [_, idx, field] if field.as_str() == Some("response") => {
                let (Some(idx), Some(parts)) =
                    (idx.as_u64(), op.get("v").and_then(Value::as_array))
                else {
                    return;
                };
                self.slot(idx).absorb_response(parts);
            }
            _ => {}
        }
    }

    /// 字段级 set
    ///
    /// - token 计数为核心, modelId 为 needle 覆盖内的防御字段(空值不覆盖已有);
    /// - 其余字段的 set 操作不含 needle, 在预过滤层即被跳过
    fn set_field(&mut self, idx: u64, field: &str, op: &Value) {
        let slot = self.slot(idx);
        match field {
            "promptTokens" => slot.prompt_tokens = load::u64_get(op, &["v"]),
            "completionTokens" => slot.completion_tokens = load::u64_get(op, &["v"]),
            "modelId" => {
                if let Some(model) = load::str_get_nonempty(op, &["v"]) {
                    slot.model = Some(model.to_string());
                }
            }
            _ => {}
        }
    }

    /// 取索引处骨架, 越界时按需扩容(防止损坏/乱序数据)
    fn slot(&mut self, idx: u64) -> &mut Skeleton {
        let idx = idx as usize;
        if idx >= self.requests.len() {
            self.requests.resize_with(idx + 1, Skeleton::default);
        }
        &mut self.requests[idx]
    }

    /// 产出用量条目
    ///
    /// - 跳过未完成(无 token)与全零请求;
    /// - 模型优先级: Auto 解析结果 > 原始 modelId, 再统一解码/归一;
    /// - 键 = requestId 兜底 `stem:索引`, 文件内重复先到者入账
    fn into_entries(self, stem: &str) -> HashMap<String, UsageEntry> {
        let Replay {
            requests,
            session_id,
            created,
        } = self;
        let session_id = session_id.unwrap_or_else(|| stem.to_string());
        let mut candidates: HashMap<String, UsageEntry> = HashMap::new();
        for (idx, skeleton) in requests.into_iter().enumerate() {
            let (input, output) = (skeleton.prompt_tokens, skeleton.completion_tokens);
            // 未完成/取消(token 从未写入恒为 0)与全零请求均跳过(claude 同款: 任一 >0 才导入)
            if input == 0 && output == 0 {
                continue;
            }
            // 时间链: timestamp > responseTimestamp > 会话 creationDate; 三缺视为损坏跳过
            let Some(created_at) = skeleton
                .timestamp
                .or(skeleton.response_timestamp)
                .or(created)
            else {
                continue;
            };
            let key = skeleton
                .request_id
                .unwrap_or_else(|| format!("{stem}:{idx}"));
            // 模型优先级: Auto 解析结果 > 原始 modelId; 统一解码/归一
            let model = skeleton
                .resolved_model
                .as_deref()
                .or(skeleton.model.as_deref())
                .map_or_else(
                    || "unknown".to_string(),
                    |raw| normalize_model(&decode_model_id(raw)),
                );
            candidates.entry(key).or_insert_with(|| {
                UsageEntry::new(
                    AppKind::Copilot,
                    model,
                    Some(session_id.clone()),
                    created_at,
                    input,
                    output,
                    0,
                    0,
                    None,
                )
            });
        }
        candidates
    }
}

/// 测试便捷入口(auto 线程); 生产路径经 collect(threads)
#[cfg(test)]
pub fn collect_from(user: &Path) -> Result<Vec<UsageEntry>, AppError> {
    collect_from_user(user, None)
}

#[cfg(test)]
#[path = "tests/copilot_test.rs"]
mod tests;
