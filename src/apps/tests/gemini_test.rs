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
fn test_missing_id_messages_counted_individually() {
    let base = temp_dir();
    let doc = session_json(
        "s",
        &[
            r#"{"type":"gemini","timestamp":"2026-09-01T10:00:00Z","tokens":{"input":10,"output":1,"cached":0,"thoughts":0}}"#.to_string(),
            r#"{"type":"gemini","timestamp":"2026-09-01T10:01:00Z","tokens":{"input":20,"output":2,"cached":0,"thoughts":0}}"#.to_string(),
        ],
    );
    write_session(&base, "p", "session-1.json", &doc);
    let entries = collect_from(&base).unwrap();
    // 缺 id 消息用内容哈希兜底: 各自计数, 不折叠进固定 unknown 键互相覆盖
    assert_eq!(entries.len(), 2);
    fs::remove_dir_all(&base).ok();
}

#[test]
fn test_missing_id_identical_content_deduped() {
    let base = temp_dir();
    let doc = session_json(
        "s",
        &[
            r#"{"type":"gemini","timestamp":"2026-09-01T10:00:00Z","tokens":{"input":10,"output":1,"cached":0,"thoughts":0}}"#.to_string(),
            r#"{"type":"gemini","timestamp":"2026-09-01T10:00:00Z","tokens":{"input":10,"output":1,"cached":0,"thoughts":0}}"#.to_string(),
        ],
    );
    write_session(&base, "p", "session-2.json", &doc);
    let entries = collect_from(&base).unwrap();
    // 内容完全相同的无 id 消息仍去重(last-wins 语义保留)
    assert_eq!(entries.len(), 1);
    fs::remove_dir_all(&base).ok();
}

#[test]
fn test_missing_id_same_usage_different_content_counted() {
    let base = temp_dir();
    let doc = session_json(
        "s",
        &[
            // timestamp/tokens/model 相同但消息内容不同(text 字段): 各自计数
            r#"{"type":"gemini","timestamp":"2026-09-01T10:00:00Z","model":"m","text":"a","tokens":{"input":10,"output":1,"cached":0,"thoughts":0}}"#.to_string(),
            r#"{"type":"gemini","timestamp":"2026-09-01T10:00:00Z","model":"m","text":"b","tokens":{"input":10,"output":1,"cached":0,"thoughts":0}}"#.to_string(),
        ],
    );
    write_session(&base, "p", "session-3.json", &doc);
    let entries = collect_from(&base).unwrap();
    // 完整消息哈希: 任何内容差异都各自计数
    assert_eq!(entries.len(), 2);
    fs::remove_dir_all(&base).ok();
}

#[test]
fn test_missing_base_returns_empty() {
    let base = temp_dir();
    fs::remove_dir_all(&base).unwrap();
    assert!(collect_from(&base).unwrap().is_empty());
}

#[test]
fn test_messages_before_session_id() {
    let base = temp_dir();
    // 键序不假设: messages 在 sessionId 之前出现仍生效
    let doc = format!(
        r#"{{"startTime":"2026-09-01T09:00:00Z","messages":[{}],"sessionId":"s-late"}}"#,
        gemini_msg("m1", "x", 1, 1, 0, 0)
    );
    write_session(&base, "p", "session-1.json", &doc);
    let entries = collect_from(&base).unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].session_id.as_deref(), Some("s-late"));
    fs::remove_dir_all(&base).ok();
}

#[test]
fn test_truncated_file_contributes_nothing() {
    let base = temp_dir();
    let doc = session_json(
        "s",
        &[
            gemini_msg("m1", "x", 1, 1, 0, 0),
            gemini_msg("m2", "x", 2, 2, 0, 0),
        ],
    );
    write_session(&base, "p", "session-trunc.json", &doc[..doc.len() / 2]);
    write_session(
        &base,
        "p",
        "session-ok.json",
        &session_json("s", &[gemini_msg("m3", "x", 3, 3, 0, 0)]),
    );
    // 截断文件警告后整体不计(staging 丢弃), 其余文件仍解析
    let entries = collect_from(&base).unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].input_tokens, 3);
    fs::remove_dir_all(&base).ok();
}

#[test]
fn test_many_messages_streamed() {
    let base = temp_dir();
    let mut messages = Vec::new();
    for i in 0..300 {
        if i % 2 == 0 {
            messages.push(gemini_msg(
                &format!("m{i}"),
                "gemini-2.5-pro",
                i as u64,
                1,
                0,
                0,
            ));
        } else {
            messages.push(r#"{"id":"u","type":"user"}"#.to_string());
        }
    }
    write_session(
        &base,
        "p",
        "session-big.json",
        &session_json("s", &messages),
    );
    let entries = collect_from(&base).unwrap();
    // 150 条 gemini 消息逐条瞬态入账, user 消息过滤
    assert_eq!(entries.len(), 150);
    fs::remove_dir_all(&base).ok();
}

#[test]
fn test_non_object_and_null_messages_skipped() {
    let base = temp_dir();
    // 合法 JSON 但顶层非对象; messages 为 null(非数组)
    write_session(&base, "p", "session-arr.json", "[]");
    write_session(
        &base,
        "p",
        "session-null.json",
        r#"{"sessionId":"s","messages":null}"#,
    );
    let entries = collect_from(&base).unwrap();
    assert!(entries.is_empty());
    fs::remove_dir_all(&base).ok();
}

/// 确定性排序(HashMap 输出序不定, 比较前排序)
fn sorted(mut entries: Vec<UsageEntry>) -> Vec<UsageEntry> {
    entries.sort_by(|x, y| {
        x.created_at
            .cmp(&y.created_at)
            .then(x.model.cmp(&y.model))
            .then(x.total_tokens().cmp(&y.total_tokens()))
            .then(x.input_tokens.cmp(&y.input_tokens))
            .then(x.output_tokens.cmp(&y.output_tokens))
    });
    entries
}

#[test]
fn test_parallel_scan_deterministic() {
    let base = temp_dir();
    write_session(
        &base,
        "p1",
        "session-1.json",
        &session_json(
            "s1",
            &[
                gemini_msg("m1", "x", 1, 1, 0, 0),
                gemini_msg("m2", "x", 2, 2, 0, 0),
            ],
        ),
    );
    write_session(
        &base,
        "p2",
        "session-2.json",
        &session_json("s2", &[gemini_msg("m1", "y", 5, 5, 0, 0)]),
    );
    let one = sorted(collect_from_with(&base, Some(1)).unwrap());
    let four = sorted(collect_from_with(&base, Some(4)).unwrap());
    assert_eq!(one, four);
    // 跨文件同 id 不同 session 不合并 → 3 条
    assert_eq!(four.len(), 3);
    fs::remove_dir_all(&base).ok();
}

#[test]
fn test_model_normalization() {
    let base = temp_dir();
    write_session(
        &base,
        "p",
        "session-1.json",
        &session_json(
            "s",
            &[gemini_msg("m1", "vertex-ai/Gemini-2.5-Pro", 1, 1, 0, 0)],
        ),
    );
    let entries = collect_from(&base).unwrap();
    assert_eq!(entries.len(), 1);
    // 全 app 统一归一化: 前缀剥除 + 小写
    assert_eq!(entries[0].model, "gemini-2.5-pro");
    fs::remove_dir_all(&base).ok();
}
