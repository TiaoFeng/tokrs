use std::collections::HashMap;
use std::collections::hash_map::Entry;
use std::path::Path;

use serde_json::Value;

use crate::error::AppError;
use crate::io::load;
use crate::model::{AppKind, UsageEntry};

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
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_dir() -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "tokrs-claude-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(dir.join("proj-hash")).unwrap();
        dir
    }

    fn write_session(base: &Path, name: &str, lines: &[String]) {
        let mut content = lines.join("\n");
        content.push('\n');
        fs::write(base.join("proj-hash").join(name), content).unwrap();
    }

    fn assistant_line(id: &str, output: u64, stop_reason: Option<&str>) -> String {
        let stop = match stop_reason {
            Some(s) => format!("\"stop_reason\":\"{s}\","),
            None => String::new(),
        };
        format!(
            r#"{{"type":"assistant","sessionId":"sess-1","timestamp":"2026-09-01T10:00:00Z","message":{{"id":"{id}","model":"claude-sonnet-4",{stop}"usage":{{"input_tokens":10,"output_tokens":{output},"cache_read_input_tokens":100,"cache_creation_input_tokens":20}}}}}}"#
        )
    }

    #[test]
    fn test_dedup_by_message_id_prefers_stop_reason() {
        let base = temp_dir();
        write_session(
            &base,
            "session.jsonl",
            &[
                assistant_line("m1", 26, None),
                assistant_line("m1", 1349, Some("end_turn")),
                assistant_line("m2", 7, None),
                r#"{"type":"user","sessionId":"sess-1"}"#.to_string(),
                r#"{"type":"assistant","sessionId":"sess-1","message":{"id":"m3","usage":{"input_tokens":0,"output_tokens":0,"cache_read_input_tokens":0,"cache_creation_input_tokens":0}}}"#.to_string(),
            ],
        );
        let entries = collect_from(&base).unwrap();
        assert_eq!(entries.len(), 2);
        let m1 = entries
            .iter()
            .find(|e| e.model == "claude-sonnet-4" && e.output_tokens == 1349);
        assert!(m1.is_some(), "expected m1 winner with stop_reason");
        assert_eq!(m1.unwrap().total_tokens(), 10 + 1349 + 100 + 20);
        assert_eq!(m1.unwrap().session_id.as_deref(), Some("sess-1"));
        assert_eq!(m1.unwrap().created_at, 1_788_256_800);
        fs::remove_dir_all(&base).ok();
    }

    #[test]
    fn test_same_stop_status_takes_larger_output() {
        let base = temp_dir();
        write_session(
            &base,
            "session.jsonl",
            &[
                assistant_line("m1", 1349, None),
                assistant_line("m1", 2000, None),
            ],
        );
        let entries = collect_from(&base).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].output_tokens, 2000);
        fs::remove_dir_all(&base).ok();
    }

    #[test]
    fn test_session_id_fallback_from_user_line() {
        let base = temp_dir();
        write_session(
            &base,
            "session.jsonl",
            &[
                r#"{"type":"user","sessionId":"sess-9"}"#.to_string(),
                r#"{"type":"assistant","timestamp":"2026-09-01T10:00:00Z","message":{"id":"m1","model":"m","usage":{"input_tokens":1,"output_tokens":1}}}"#.to_string(),
            ],
        );
        let entries = collect_from(&base).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].session_id.as_deref(), Some("sess-9"));
        assert_eq!(entries[0].model, "m");
        fs::remove_dir_all(&base).ok();
    }

    #[test]
    fn test_missing_base_returns_empty() {
        let base = temp_dir();
        fs::remove_dir_all(&base).unwrap();
        assert!(collect_from(&base).unwrap().is_empty());
    }
}
