//! codex日志文件解析
//!
use serde_json::Value;
use std::{collections::HashSet, path::Path};

use crate::{
    apps::fresh_input,
    error::AppError,
    io::load,
    model::{AppKind, UsageEntry},
};

const SESSIONS_MAX_DEPTH: usize = 3;

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
    files.sort();
    files.dedup();

    let mut seen_signatures: HashSet<Signature> = HashSet::new();
    let mut entries = Vec::new();
    for file in files {
        parse_file(&file, &mut entries, &mut seen_signatures)?;
    }
    Ok(entries)
}

fn is_rollout_filename(path: &Path) -> bool {
    path.file_name()
        .and_then(|n| n.to_str())
        .is_some_and(|name| name.starts_with("rollout-") && name.ends_with(".jsonl"))
}

#[derive(Clone, Copy, Default, PartialEq, Eq, Hash)]
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

struct FileState {
    thread_id: Option<String>,
    model: Option<String>,
    high_water: Option<[u64; 4]>,
}

fn parse_file(
    file: &Path,
    entries: &mut Vec<UsageEntry>,
    seen: &mut HashSet<Signature>,
) -> Result<(), AppError> {
    let mut state = FileState {
        thread_id: None,
        model: None,
        high_water: None,
    };
    for line in load::read_jsonl(file)? {
        match load::str_get(&line, &["type"]) {
            Some("session_meta") => {
                if state.thread_id.is_none() {
                    state.thread_id = ["id", "thread_id", "threadId", "session_id"]
                        .iter()
                        .find_map(|k| load::str_get(&line, &["payload", k]))
                        .map(str::to_string);
                }
            }
            Some("turn_context") => {
                if let Some(m) = load::str_get(&line, &["payload", "model"])
                    .or_else(|| load::str_get(&line, &["payload", "info", "model"]))
                {
                    state.model = Some(normalize_model(m));
                }
            }
            Some("event_msg") => parse_token_count(&line, &mut state, entries, seen),
            _ => {}
        }
    }
    Ok(())
}

fn parse_token_count(
    line: &Value,
    state: &mut FileState,
    entries: &mut Vec<UsageEntry>,
    seen: &mut HashSet<Signature>,
) {
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

    let [d_input, d_cached, d_write, d_output] = match last {
        Some(l) => l.billable(),
        None => delta_from_total(state, &total.unwrap_or_default()),
    };

    if !seen.insert(signature(total, last)) {
        return;
    }
    let d_cached = d_cached.min(d_input);
    if d_input == 0 && d_cached == 0 && d_write == 0 && d_output == 0 {
        return;
    }
    // codex 的 input_tokens 含 cache read 与 cache write, 归一为 fresh input
    let d_input = fresh_input(d_input, d_cached, d_write);

    let model = load::str_get(info, &["model"])
        .or_else(|| load::str_get(info, &["model_name"]))
        .map(normalize_model)
        .or_else(|| state.model.clone())
        .unwrap_or_else(|| "unknown".to_string());
    let created_at = line
        .get("timestamp")
        .and_then(load::timestamp_to_epoch)
        .unwrap_or_else(load::now_epoch);

    // codex 无自报成本, 待定价表估价
    entries.push(UsageEntry::new(
        AppKind::Codex,
        model,
        state.thread_id.clone(),
        created_at,
        d_input,
        d_output,
        d_cached,
        d_write,
        None,
    ));
}

fn delta_from_total(state: &mut FileState, counters: &Counters) -> [u64; 4] {
    let current = counters.billable();
    let base = *state.high_water.get_or_insert([0; 4]);
    state.high_water = Some(std::array::from_fn(|i| base[i].max(current[i])));
    std::array::from_fn(|i| current[i].saturating_sub(base[i]))
}

fn normalize_model(raw: &str) -> String {
    let mut s = raw.trim().to_ascii_lowercase();
    if let Some(pos) = s.rfind('/') {
        s = s[pos + 1..].to_string();
    }
    strip_date_suffix(&s).to_string()
}

fn strip_date_suffix(s: &str) -> &str {
    let bytes = s.as_bytes();
    let all_digits = |b: &[u8]| b.iter().all(u8::is_ascii_digit);
    if bytes.len() >= 11
        && bytes[bytes.len() - 11] == b'-'
        && bytes[bytes.len() - 6] == b'-'
        && bytes[bytes.len() - 3] == b'-'
        && all_digits(&bytes[bytes.len() - 10..bytes.len() - 6])
        && all_digits(&bytes[bytes.len() - 5..bytes.len() - 3])
        && all_digits(&bytes[bytes.len() - 2..])
    {
        return &s[..s.len() - 11];
    }
    if bytes.len() >= 9 && bytes[bytes.len() - 9] == b'-' && all_digits(&bytes[bytes.len() - 8..]) {
        return &s[..s.len() - 9];
    }
    s
}

#[cfg(test)]
#[path = "tests/codex_test.rs"]
mod tests;
