//! codex日志文件解析
//!
//! 数据源: ~/.codex/{sessions/**,archived_sessions} 下的 rollout-*.jsonl
//! 去重与 delta 口径对齐 cc-switch session_usage_codex.rs:
//! - 含计费 token 的文件必须有 session_meta(无 meta 无法建立线程身份, 回放无法
//!   判定); 本工具每次运行全量重扫, meta/父文件之后出现时下次运行自然恢复
//! - 身份一致性: session_meta.id 与文件名 uuid(尾部/双段前置)均存在时必须匹配,
//!   比较大小写不敏感(uuid 形状 id 归一小写, 对齐 cc-switch Uuid::hyphenated);
//!   冲突文件整体不计(时间线仍按文件名 uuid 登记), 防止错拷/异常文件污染线程
//! - 文件内去重: rate-limit 刷新会在其它 limit_id 下原样重发快照; 同源(limit_id)
//!   最新快照或紧邻前一事件签名一致且 total 存在 → delta 判零; 不与更旧快照
//!   比较——计数器重置后旧签名可能合法复现
//! - delta: last_token_usage 是单次请求精确面值, 优先; 缺失时对 total 高水位差分;
//!   高水位随每条含 total 事件推进, last/total 混合流不超计
//! - fork 回放: meta 声明 forked_from_id / thread_spawn.parent_thread_id 时, 父候选
//!   以文件名尾部 uuid 索引(同键多候选须 cutoff 前签名序列全等), 父时间线取
//!   ts ≤ 子 meta 时刻的签名, 子事件按序做子序列匹配, 最长前导命中段为回放
//!   拷贝, 跳过; 父 ID 冲突/父缺失/子 meta 无时间戳/父含无时间戳事件/父最大
//!   时间戳早于子 meta 时刻 → 子文件不计(对齐 cc-switch 的 defer 语义)
//! - archived_sessions 存在同名副本(cc-switch 靠游标继承防双算), 本工具按文件名
//!   去重保留字节最长者
//! - 无跨文件签名去重: 不同会话雷同用量不误吞(cc-switch 亦无此逻辑)
//!
//! 与 cc-switch 的已知差异(无状态全量扫描的取舍):
//! - 签名缺失字段按 0 计, 不做 Option 区分
//! - 不硬性要求文件名尾部 uuid 存在(无 DB request_id 幂等需求); 缺 uuid 的文件
//!   照常入账但不参与父索引
//!
use serde_json::Value;
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
};

use crate::{
    apps::{fresh_input, normalize_model},
    error::AppError,
    io::load,
    model::{AppKind, UsageEntry},
};

const SESSIONS_MAX_DEPTH: usize = 3;

/// 有效快照须至少含一个本工具解析的 token 字段(对齐 cc-switch
/// parse_cumulative_tokens); cache_write_input_tokens 为本工具扩展解析字段
/// (cc-switch 不计 cache write), 同样参与有效性判定
const TOKEN_FIELDS: [&str; 7] = [
    "input_tokens",
    "cached_input_tokens",
    "cache_read_input_tokens",
    "cache_write_input_tokens",
    "output_tokens",
    "reasoning_output_tokens",
    "total_tokens",
];

pub fn collect() -> Result<Vec<UsageEntry>, AppError> {
    let base = load::home_dir()?.join(".codex");
    if !base.is_dir() {
        return Ok(Vec::new());
    }
    collect_from(&base)
}

pub fn collect_from(base: &Path) -> Result<Vec<UsageEntry>, AppError> {
    let mut files = load::discover_files(&base.join("sessions"), "jsonl", SESSIONS_MAX_DEPTH);
    files.extend(load::discover_files(
        &base.join("archived_sessions"),
        "jsonl",
        0,
    ));
    files.retain(|f| is_rollout_filename(f));
    let files = dedupe_by_filename(files);

    // Pass 1: 逐文件解析 meta 与 token 事件(文件内去重与 delta 计算在此完成)
    let mut parsed: Vec<ParsedFile> = Vec::with_capacity(files.len());
    for file in &files {
        parsed.push(parse_file(file)?);
    }

    // Pass 2: 以文件名 uuid 汇总父时间线, fork 回放段跳过后入账
    let timelines = build_timelines(&parsed);
    let mut entries = Vec::new();
    for file in &parsed {
        emit_entries(file, &timelines, &mut entries);
    }
    Ok(entries)
}

/// 按文件名去重: codex 归档会把同名 rollout 复制进 archived_sessions, 保留字节
/// 最长者(并列取非 archived 路径), 避免同一 rollout 双算
fn dedupe_by_filename(files: Vec<PathBuf>) -> Vec<PathBuf> {
    let mut best: HashMap<String, (u64, bool, PathBuf)> = HashMap::new();
    for path in files {
        let Some(name) = path
            .file_name()
            .and_then(|n| n.to_str())
            .map(str::to_string)
        else {
            continue;
        };
        let Ok(size) = std::fs::metadata(&path).map(|m| m.len()) else {
            continue;
        };
        let is_live = path
            .parent()
            .and_then(|p| p.file_name())
            .and_then(|n| n.to_str())
            != Some("archived_sessions");
        let should_replace = match best.get(&name) {
            Some((kept_size, kept_live, _)) => (size, is_live) > (*kept_size, *kept_live),
            None => true,
        };
        if should_replace {
            best.insert(name, (size, is_live, path));
        }
    }
    let mut paths: Vec<PathBuf> = best.into_values().map(|(_, _, path)| path).collect();
    paths.sort();
    paths
}

fn is_rollout_filename(path: &Path) -> bool {
    path.file_name()
        .and_then(|n| n.to_str())
        .is_some_and(|name| name.starts_with("rollout-") && name.ends_with(".jsonl"))
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct Signature([u64; 12]);

#[derive(Clone, Copy, Default)]
struct Counters {
    input: u64,
    cached: u64,
    cache_write: u64,
    output: u64,
    reasoning: u64,
    total: u64,
}

impl Counters {
    fn parse(info: &Value, key: &str) -> Option<Self> {
        let obj = info.get(key)?.as_object()?;
        // 空对象/无 token 字段的对象不是有效快照, 视为缺失
        if !TOKEN_FIELDS.iter().any(|f| obj.contains_key(*f)) {
            return None;
        }
        Some(Self {
            input: obj.get("input_tokens").and_then(Value::as_u64).unwrap_or(0),
            cached: obj
                .get("cached_input_tokens")
                .and_then(Value::as_u64)
                .or_else(|| obj.get("cache_read_input_tokens").and_then(Value::as_u64))
                .unwrap_or(0),
            cache_write: obj
                .get("cache_write_input_tokens")
                .and_then(Value::as_u64)
                .unwrap_or(0),
            output: obj
                .get("output_tokens")
                .and_then(Value::as_u64)
                .unwrap_or(0),
            reasoning: obj
                .get("reasoning_output_tokens")
                .and_then(Value::as_u64)
                .unwrap_or(0),
            total: obj.get("total_tokens").and_then(Value::as_u64).unwrap_or(0),
        })
    }

    /// [input(含缓存), cached, cache_write, output]; fresh 归一在入账处统一处理
    fn billable(&self) -> [u64; 4] {
        [self.input, self.cached, self.cache_write, self.output]
    }

    fn fields(self) -> [u64; 6] {
        [
            self.input,
            self.cached,
            self.cache_write,
            self.output,
            self.reasoning,
            self.total,
        ]
    }
}

fn signature(total: Option<Counters>, last: Option<Counters>) -> Signature {
    let mut fields = [0u64; 12];
    if let Some(t) = total {
        fields[..6].copy_from_slice(&t.fields());
    }
    if let Some(l) = last {
        fields[6..].copy_from_slice(&l.fields());
    }
    Signature(fields)
}

/// fork/子代理声明的父线程: forked_from_id 与 thread_spawn.parent_thread_id
enum ParentLink {
    /// 未声明
    None,
    /// 两处声明一致
    Parent(String),
    /// 两处声明不一致, 无法确定父线程(cc-switch 对应 defer, 子文件不计)
    Conflicted,
}

/// session_meta 关键信息(仅首条生效)
struct MetaInfo {
    thread_id: Option<String>,
    parent: ParentLink,
    /// meta 时间戳(epoch 秒), 作为父时间线的 cutoff
    ts: Option<i64>,
}

/// 单条 token_count 事件的解析产物
struct TokenEvent {
    signature: Signature,
    /// delta 面值 [input(含缓存), cached, cache_write, output]; 重复快照判零为 None
    delta: Option<[u64; 4]>,
    /// 事件时间戳(epoch 秒), 缺失 None
    ts: Option<i64>,
    model: String,
    created_at: i64,
}

struct ParsedFile {
    file: PathBuf,
    meta: Option<MetaInfo>,
    /// session_meta.id 与文件名 uuid 冲突: 文件不计, 时间线仍登记
    identity_conflict: bool,
    events: Vec<TokenEvent>,
}

#[derive(Default)]
struct ParseState {
    model: Option<String>,
    high_water: Option<[u64; 4]>,
    /// rate-limit 分桶(source)各自最新的完整快照签名
    last_by_source: HashMap<Option<String>, Signature>,
    previous: Option<Signature>,
}

fn parse_file(file: &Path) -> Result<ParsedFile, AppError> {
    let mut meta: Option<MetaInfo> = None;
    let mut state = ParseState::default();
    let mut events = Vec::new();
    for line in load::read_jsonl(file)? {
        match load::str_get(&line, &["type"]) {
            // 仅首条 session_meta 生效(对齐 cc-switch root_meta_seen)
            Some("session_meta") if meta.is_none() => {
                meta = Some(MetaInfo {
                    thread_id: ["id", "thread_id", "threadId", "session_id"]
                        .iter()
                        .find_map(|k| load::str_get(&line, &["payload", k]))
                        .map(normalize_thread_id),
                    parent: line
                        .get("payload")
                        .map_or(ParentLink::None, explicit_parent_from_meta),
                    ts: line.get("timestamp").and_then(load::timestamp_to_epoch),
                });
            }
            Some("turn_context") => {
                if let Some(m) = load::str_get(&line, &["payload", "model"])
                    .or_else(|| load::str_get(&line, &["payload", "info", "model"]))
                {
                    state.model = Some(normalize_model(m));
                }
            }
            Some("event_msg") => parse_token_count(&line, &mut state, &mut events),
            _ => {}
        }
    }
    // 身份一致性: session_meta.id 与文件名 uuid(尾部/双段前置)均存在时必须匹配;
    // 文件名无 uuid 时无从校验, 放行
    let tail = thread_id_from_filename(file);
    let leading = leading_thread_id_from_filename(file);
    let identity_conflict = meta
        .as_ref()
        .and_then(|m| m.thread_id.as_deref())
        .is_some_and(|id| {
            (tail.is_some() || leading.is_some())
                && tail.as_deref() != Some(id)
                && leading.as_deref() != Some(id)
        });
    Ok(ParsedFile {
        file: file.to_path_buf(),
        meta,
        identity_conflict,
        events,
    })
}

fn parse_token_count(line: &Value, state: &mut ParseState, events: &mut Vec<TokenEvent>) {
    if load::str_get(line, &["payload", "type"]) != Some("token_count") {
        return;
    }
    let Some(info) = line.pointer("/payload/info") else {
        return;
    };
    let total = Counters::parse(info, "total_token_usage");
    let last = Counters::parse(info, "last_token_usage");
    if total.is_none() && last.is_none() {
        return;
    }
    let signature = signature(total, last);

    // info 携带 model 时持久化到当前模型(对齐 cc-switch), 后续无 model 事件沿用
    if let Some(m) = load::str_get(info, &["model"])
        .or_else(|| load::str_get(info, &["model_name"]))
        .or_else(|| load::str_get(line, &["payload", "model"]))
    {
        state.model = Some(normalize_model(m));
    }

    // 同源最新快照或紧邻前一事件签名一致且 total 存在 → rate-limit 刷新重复,
    // delta 判零; 不与更旧快照比较——计数器重置后旧签名可能合法复现
    let source = load::str_get(line, &["payload", "rate_limits", "limit_id"])
        .or_else(|| load::str_get(info, &["rate_limits", "limit_id"]))
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    let duplicate = total.is_some()
        && (state.last_by_source.get(&source) == Some(&signature)
            || state.previous.as_ref() == Some(&signature));
    if total.is_some() {
        state.last_by_source.insert(source, signature);
    }
    state.previous = Some(signature);

    let delta = if duplicate {
        None
    } else {
        match last {
            // last 是单次请求精确面值, 优先于累计快照差分
            Some(l) => Some(l.billable()),
            None => Some(delta_from_total(
                state.high_water,
                total.as_ref().unwrap_or(&Counters::default()),
            )),
        }
    };
    // 高水位随每条含 total 事件推进(重复事件值相同为 no-op), 混合流不超计
    if let Some(t) = &total {
        advance_high_water(&mut state.high_water, t);
    }
    // 上游异常时 cached 面值可能超过 input, clamp 到 input
    let delta = delta.map(|mut d| {
        d[1] = d[1].min(d[0]);
        d
    });

    let ts = line.get("timestamp").and_then(load::timestamp_to_epoch);
    events.push(TokenEvent {
        signature,
        delta,
        ts,
        model: state.model.clone().unwrap_or_else(|| "unknown".to_string()),
        created_at: ts.unwrap_or_else(load::now_epoch),
    });
}

fn delta_from_total(high_water: Option<[u64; 4]>, counters: &Counters) -> [u64; 4] {
    let current = counters.billable();
    let base = high_water.unwrap_or([0; 4]);
    std::array::from_fn(|i| current[i].saturating_sub(base[i]))
}

fn advance_high_water(high_water: &mut Option<[u64; 4]>, counters: &Counters) {
    let current = counters.billable();
    let base = high_water.get_or_insert([0; 4]);
    *base = std::array::from_fn(|i| base[i].max(current[i]));
}

/// fork/子代理声明的父线程; 两处并存且一致才启用, 不一致则无法确定父线程
fn explicit_parent_from_meta(payload: &Value) -> ParentLink {
    // 先归一再比较, 大小写不同的同一 uuid 不会误判为冲突
    let forked = payload
        .get("forked_from_id")
        .and_then(nonempty_str)
        .map(normalize_thread_id);
    let spawned = payload
        .get("source")
        .and_then(|s| s.get("subagent"))
        .and_then(|s| s.get("thread_spawn"))
        .and_then(|s| s.get("parent_thread_id"))
        .and_then(nonempty_str)
        .map(normalize_thread_id);
    match (forked, spawned) {
        (None, None) => ParentLink::None,
        (Some(parent), None) | (None, Some(parent)) => ParentLink::Parent(parent),
        (Some(forked), Some(spawned)) if forked == spawned => ParentLink::Parent(forked),
        _ => ParentLink::Conflicted,
    }
}

fn nonempty_str(value: &Value) -> Option<&str> {
    value.as_str().filter(|s| !s.is_empty())
}

/// 文件名尾部 uuid -> 各文件的时间线候选(同键多文件时不盲并)
#[derive(Default)]
struct ParentTimeline {
    /// 带时间戳的签名(文件序); 回放匹配按序子序列进行
    events: Vec<TimelineEvent>,
    /// 存在无时间戳的 token 事件(cutoff 过滤不可靠)
    has_untimed: bool,
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct TimelineEvent {
    ts: i64,
    signature: Signature,
}

fn build_timelines(parsed: &[ParsedFile]) -> HashMap<String, Vec<ParentTimeline>> {
    let mut timelines: HashMap<String, Vec<ParentTimeline>> = HashMap::new();
    for file in parsed {
        // 父索引键 = 文件名尾部 uuid(对齐 cc-switch RolloutIndex, 不用 meta id);
        // 双段文件名的尾部是物理 rollout id, 与 cc-switch 一致不作为线程键
        let Some(uuid) = thread_id_from_filename(&file.file) else {
            continue;
        };
        let mut timeline = ParentTimeline::default();
        for event in &file.events {
            match event.ts {
                Some(ts) => timeline.events.push(TimelineEvent {
                    ts,
                    signature: event.signature,
                }),
                None => timeline.has_untimed = true,
            }
        }
        timelines.entry(uuid).or_default().push(timeline);
    }
    timelines
}

#[derive(Clone, Copy)]
enum Replay {
    /// 子文件不计(对齐 cc-switch 的 defer 语义)
    Skip,
    /// 跳过前 n 个回放事件
    Prefix(usize),
}

fn replay_prefix(
    meta: &MetaInfo,
    events: &[TokenEvent],
    timelines: &HashMap<String, Vec<ParentTimeline>>,
) -> Replay {
    let parent_id = match &meta.parent {
        ParentLink::None => return Replay::Prefix(0),
        // 自引用的 parent 视为异常元数据, 不计
        ParentLink::Parent(id) if meta.thread_id.as_deref() == Some(id.as_str()) => {
            return Replay::Skip;
        }
        ParentLink::Parent(id) => id,
        ParentLink::Conflicted => return Replay::Skip,
    };
    // cutoff = 子 meta 时刻; 缺失则回放窗口无法界定
    let Some(cutoff) = meta.ts else {
        return Replay::Skip;
    };
    let Some(candidates) = timelines.get(parent_id) else {
        return Replay::Skip;
    };
    // 父视图不完整(无时间戳事件/最大时间戳早于 cutoff)时回放无法可靠判定
    for timeline in candidates {
        if timeline.has_untimed {
            return Replay::Skip;
        }
        let max_ts = timeline.events.iter().map(|e| e.ts).max();
        if max_ts.is_none_or(|max| max < cutoff) {
            return Replay::Skip;
        }
    }
    // 同键多候选: cutoff 前的签名序列必须全等(对齐 cc-switch 对内容不一致的
    // 父文件的 defer)
    let filtered = |timeline: &ParentTimeline| {
        timeline
            .events
            .iter()
            .filter(|e| e.ts <= cutoff)
            .map(|e| e.signature)
            .collect::<Vec<_>>()
    };
    let Some(first) = candidates.first() else {
        return Replay::Skip;
    };
    let signatures = filtered(first);
    if candidates
        .iter()
        .any(|timeline| filtered(timeline) != signatures)
    {
        return Replay::Skip;
    }
    Replay::Prefix(matching_replay_prefix(events, &signatures))
}

/// 子事件按序在父签名序列中做子序列匹配(父游标单调推进), 返回最长前导命中段;
/// 回放拷贝与父事件顺序一致, 乱序雷同签名不会误命中
fn matching_replay_prefix(child: &[TokenEvent], parent: &[Signature]) -> usize {
    let mut offset = 0usize;
    let mut matched = 0usize;
    for event in child {
        let Some(relative) = parent[offset..]
            .iter()
            .position(|signature| *signature == event.signature)
        else {
            break;
        };
        offset += relative + 1;
        matched += 1;
    }
    matched
}

fn emit_entries(
    parsed: &ParsedFile,
    timelines: &HashMap<String, Vec<ParentTimeline>>,
    entries: &mut Vec<UsageEntry>,
) {
    // 含计费 token 但无 session_meta: 无法建立线程身份, 不计(cc-switch 同)
    let Some(meta) = &parsed.meta else {
        return;
    };
    // meta.id 与文件名 uuid 冲突: 异常/错拷文件整体不计
    if parsed.identity_conflict {
        return;
    }
    let Replay::Prefix(prefix) = replay_prefix(meta, &parsed.events, timelines) else {
        return;
    };
    for event in &parsed.events[prefix..] {
        let Some([input, cached, write, output]) = event.delta else {
            continue; // 重复快照判零
        };
        if input == 0 && cached == 0 && write == 0 && output == 0 {
            continue;
        }
        // codex 的 input_tokens 含 cache read 与 cache write, 归一为 fresh input
        let input = fresh_input(input, cached, write);
        // codex 无自报成本, 待定价表估价
        entries.push(UsageEntry::new(
            AppKind::Codex,
            event.model.clone(),
            meta.thread_id.clone(),
            event.created_at,
            input,
            output,
            cached,
            write,
            None,
        ));
    }
}

/// 文件名尾部 36 字符的线程 uuid(rollout-<ts>-<uuid>.jsonl), 作父链索引键;
/// 双段文件名(thread/revert 替换 rollout)的尾段是物理 rollout id, 与 cc-switch
/// 一致仅用尾部
fn thread_id_from_filename(path: &Path) -> Option<String> {
    let stem = path.file_stem()?.to_str()?;
    let candidate = stem.get(stem.len().checked_sub(36)?..)?;
    is_uuid_like(candidate).then(|| normalize_thread_id(candidate))
}

/// 双段文件名(`rollout-…-<threadId>_<rolloutId>.jsonl`)中下划线前的线程本体
/// uuid, 仅用于身份一致性校验; 单段文件名返回 None
fn leading_thread_id_from_filename(path: &Path) -> Option<String> {
    let stem = path.file_stem()?.to_str()?;
    let len = stem.len();
    // 布局尾部: …<uuidA>_<uuidB>, uuidB 占 36 字符, 其前是 '_'
    if !stem.get(len.checked_sub(37)?..)?.starts_with('_') {
        return None;
    }
    let candidate = stem.get(len.checked_sub(73)?..len.checked_sub(37)?)?;
    is_uuid_like(candidate).then(|| normalize_thread_id(candidate))
}

/// uuid 形状的线程 id 归一为小写(对齐 cc-switch 的 Uuid::hyphenated 输出),
/// 否则原样; meta.id / forked_from_id / 文件名 uuid 统一经此归一后比较
fn normalize_thread_id(s: &str) -> String {
    if is_uuid_like(s) {
        s.to_ascii_lowercase()
    } else {
        s.to_string()
    }
}

/// 8-4-4-4-12 十六进制 uuid 形状校验(避免引入 uuid 依赖)
fn is_uuid_like(s: &str) -> bool {
    let b = s.as_bytes();
    b.len() == 36
        && b.iter().enumerate().all(|(i, &c)| match i {
            8 | 13 | 18 | 23 => c == b'-',
            _ => c.is_ascii_hexdigit(),
        })
}

#[cfg(test)]
#[path = "tests/codex_test.rs"]
mod tests;
