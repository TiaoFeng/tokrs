use super::*;
use std::fs;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

fn temp_dir() -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "tokrs-gemini-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    fs::create_dir_all(&dir).unwrap();
    dir
}

fn write_session(base: &Path, project: &str, name: &str, content: &str) {
    let dir = base.join(project).join("chats");
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join(name), content).unwrap();
}

fn session_json(session_id: &str, messages: &[String]) -> String {
    format!(
        r#"{{"sessionId":"{session_id}","startTime":"2026-09-01T09:00:00Z","messages":[{}]}}"#,
        messages.join(",")
    )
}

fn gemini_msg(
    id: &str,
    model: &str,
    input: u64,
    output: u64,
    cached: u64,
    thoughts: u64,
) -> String {
    format!(
        r#"{{"id":"{id}","type":"gemini","timestamp":"2026-09-01T10:00:00Z","model":"{model}","tokens":{{"input":{input},"output":{output},"cached":{cached},"thoughts":{thoughts},"tool":5,"total":999999}}}}"#
    )
}

#[test]
fn test_parse_tokens_and_merge_thoughts() {
    let base = temp_dir();
    let doc = session_json(
        "sess-1",
        &[
            r#"{"id":"u1","type":"user","timestamp":"2026-09-01T09:59:00Z"}"#.to_string(),
            gemini_msg("m1", "gemini-2.5-pro", 100, 20, 0, 30),
            gemini_msg("m2", "gemini-2.5-pro", 0, 0, 500, 0),
            gemini_msg("m3", "gemini-2.5-pro", 0, 0, 0, 0),
            gemini_msg("m4", "gemini-2.5-pro", 200, 20, 50, 0),
        ],
    );
    write_session(&base, "proj-a", "session-x.json", &doc);
    let mut entries = collect_from(&base).unwrap();
    entries.sort_by_key(|e| e.input_tokens);
    // user 消息与全零 token 消息被过滤, 纯缓存命中保留
    assert_eq!(entries.len(), 3);
    assert_eq!(entries[0].cache_read_tokens, 500);
    assert_eq!(entries[0].input_tokens, 0);
    // thoughts 并入 output, tool/total 字段忽略
    assert_eq!(entries[1].output_tokens, 50);
    assert_eq!(entries[1].cache_creation_tokens, 0);
    assert_eq!(entries[1].total_tokens(), 150);
    assert_eq!(entries[1].session_id.as_deref(), Some("sess-1"));
    assert_eq!(entries[1].model, "gemini-2.5-pro");
    assert_eq!(entries[1].created_at, 1_788_256_800);
    // input 含 cached 已扣除: 200-50=150
    assert_eq!(entries[2].input_tokens, 150);
    assert_eq!(entries[2].cache_read_tokens, 50);
    assert_eq!(entries[2].total_tokens(), 220);
    fs::remove_dir_all(&base).ok();
}

#[test]
fn test_same_id_last_wins_within_session() {
    let base = temp_dir();
    let doc = session_json(
        "s",
        &[
            gemini_msg("m1", "first", 1, 1, 0, 0),
            gemini_msg("m1", "second", 2, 2, 0, 0),
        ],
    );
    write_session(&base, "p", "session-1.json", &doc);
    let entries = collect_from(&base).unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].model, "second");
    assert_eq!(entries[0].input_tokens, 2);
    fs::remove_dir_all(&base).ok();
}

#[test]
fn test_same_id_across_sessions_not_merged() {
    let base = temp_dir();
    write_session(
        &base,
        "p1",
        "session-1.json",
        &session_json("sa", &[gemini_msg("m1", "x", 1, 1, 0, 0)]),
    );
    write_session(
        &base,
        "p2",
        "session-2.json",
        &session_json("sb", &[gemini_msg("m1", "x", 5, 5, 0, 0)]),
    );
    assert_eq!(collect_from(&base).unwrap().len(), 2);
    fs::remove_dir_all(&base).ok();
}

#[test]
fn test_non_session_files_and_corrupted_ignored() {
    let base = temp_dir();
    write_session(
        &base,
        "p",
        "other.json",
        &session_json("s", &[gemini_msg("m1", "x", 1, 1, 0, 0)]),
    );
    write_session(
        &base,
        "p",
        "session-ok.json",
        &session_json("s", &[gemini_msg("m2", "x", 1, 1, 0, 0)]),
    );
    write_session(&base, "p", "session-bad.json", "{not json");
    let entries = collect_from(&base).unwrap();
    assert_eq!(entries.len(), 1);
    fs::remove_dir_all(&base).ok();
}

#[test]
fn test_missing_id_and_model_fallback() {
    let base = temp_dir();
    let msg = r#"{"type":"gemini","tokens":{"input":3,"output":4,"cached":0,"thoughts":0}}"#;
    write_session(
        &base,
        "p",
        "session-1.json",
        &session_json("s", &[msg.to_string()]),
    );
    let entries = collect_from(&base).unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].model, "unknown");
    fs::remove_dir_all(&base).ok();
}

#[test]
fn test_missing_base_returns_empty() {
    let base = temp_dir();
    fs::remove_dir_all(&base).unwrap();
    assert!(collect_from(&base).unwrap().is_empty());
}
