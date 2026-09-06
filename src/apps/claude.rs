//! claude日志解析
//!
use serde_json::Value;
use std::{
    collections::{HashMap, hash_map::Entry},
    path::Path,
};

use crate::{
    error::AppError,
    io::load,
    model::{AppKind, UsageEntry},
};

const MAX_DEPTH: usize = 5;

pub fn collect() -> Result<Vec<UsageEntry>, AppError> {
    let base = load::home_dir()?.join(".claude").join("projects");
    if !base.is_dir() {
        return Ok(Vec::new());
    }
    collect_from(&base)
}

pub fn collect_from(base: &Path) -> Result<Vec<UsageEntry>, AppError> {
    let mut candidates: HashMap<String, Candidate> = HashMap::new();
    for file in load::discover_files(base, "jsonl", MAX_DEPTH) {
        let mut session_fallback: Option<String> = None;
        for value in load::read_jsonl(&file)? {
            if session_fallback.is_none() {
                session_fallback = load::str_get(&value, &["sessionId"]).map(str::to_string);
            }
            parse_assistant_line(&value, session_fallback.as_deref(), &mut candidates);
        }
    }
    Ok(candidates.into_values().map(|c| c.entry).collect())
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
    let model = load::str_get(message, &["model"])
        .unwrap_or("unknown")
        .to_string();
    let session_id = load::str_get(value, &["sessionId"])
        .or(session_fallback)
        .map(str::to_string);
    let created_at = value
        .get("timestamp")
        .and_then(load::timestamp_to_epoch)
        .unwrap_or_else(load::now_epoch);
    let entry = UsageEntry {
        app: AppKind::Claude,
        model,
        session_id,
        created_at,
        input_tokens: input,
        output_tokens: output,
        cache_read_tokens: cache_read,
        cache_creation_tokens: cache_creation,
    };
    let candidate = Candidate {
        has_stop_reason: load::str_get(message, &["stop_reason"]).is_some(),
        entry,
    };
    match candidates.entry(msg_id.to_string()) {
        Entry::Vacant(vacant) => {
            vacant.insert(candidate);
        }
        Entry::Occupied(mut occupied) => {
            let existing = occupied.get();
            let should_replace = (candidate.has_stop_reason && !existing.has_stop_reason)
                || (candidate.has_stop_reason == existing.has_stop_reason
                    && candidate.entry.output_tokens > existing.entry.output_tokens);
            if should_replace {
                occupied.insert(candidate);
            }
        }
    }
}

#[cfg(test)]
#[path = "tests/claude_test.rs"]
mod tests;
