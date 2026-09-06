use super::*;
use std::fs;
use std::time::{SystemTime, UNIX_EPOCH};

const TS: i64 = 1_788_256_800;

fn temp_dir() -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "tokrs-pi-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    fs::create_dir_all(&dir).unwrap();
    dir
}

fn write_file(base: &Path, name: &str, lines: &[String]) {
    let mut content = lines.join("\n");
    content.push('\n');
    fs::write(base.join(name), content).unwrap();
}

fn header(id: &str, ts: i64) -> String {
    format!(r#"{{"type":"session","id":"{id}","timestamp":{ts}}}"#)
}

fn usage_json(input: u64, output: u64, cache_read: u64, cache_write: u64) -> String {
    format!(
        r#"{{"input":{input},"output":{output},"cacheRead":{cache_read},"cacheWrite":{cache_write}}}"#
    )
}

fn model_field(m: &str) -> String {
    format!(r#""model":"{m}","#)
}

fn message_entry(id: Option<&str>, ts: i64, role: &str, extra: &str, usage: &str) -> String {
    let idf = id.map(|i| format!(r#""id":"{i}","#)).unwrap_or_default();
    format!(
        r#"{{"type":"message",{idf}"timestamp":{ts},"message":{{"role":"{role}",{extra}"usage":{usage}}}}}"#
    )
}

fn usage_entry(kind_type: &str, id: Option<&str>, ts: i64, usage: &str) -> String {
    let idf = id.map(|i| format!(r#""id":"{i}","#)).unwrap_or_default();
    format!(r#"{{"type":"{kind_type}",{idf}"timestamp":{ts},"usage":{usage}}}"#)
}

#[test]
fn test_assistant_usage_and_model_precedence() {
    let base = temp_dir();
    write_file(
        &base,
        "s1.jsonl",
        &[
            header("s-1", TS),
            message_entry(
                Some("a1"),
                TS,
                "assistant",
                r#""provider":"anthropic","model":"req-model","responseModel":"actual-model","#,
                &usage_json(100, 10, 5, 2),
            ),
            message_entry(
                Some("a2"),
                TS + 1,
                "assistant",
                &model_field("req-model"),
                &usage_json(1, 1, 0, 0),
            ),
            message_entry(Some("a3"), TS + 2, "assistant", "", &usage_json(2, 2, 0, 0)),
        ],
    );
    let mut entries = collect_from(std::slice::from_ref(&base)).unwrap();
    entries.sort_by_key(|e| e.input_tokens);
    assert_eq!(entries.len(), 3);
    let big = &entries[2];
    // responseModel 优先于 model; cacheWrite 映射到 cache_creation
    assert_eq!(big.model, "actual-model");
    assert_eq!(big.input_tokens, 100);
    assert_eq!(big.output_tokens, 10);
    assert_eq!(big.cache_read_tokens, 5);
    assert_eq!(big.cache_creation_tokens, 2);
    assert_eq!(big.session_id.as_deref(), Some("s-1"));
    assert_eq!(big.created_at, TS);
    assert_eq!(entries[0].model, "req-model");
    assert_eq!(entries[1].model, "unknown");
    fs::remove_dir_all(&base).ok();
}

#[test]
fn test_kind_filtering() {
    let base = temp_dir();
    write_file(
        &base,
        "s2.jsonl",
        &[
            header("s-2", TS),
            message_entry(Some("u1"), TS, "user", "", &usage_json(9, 9, 9, 9)),
            message_entry(Some("t1"), TS, "toolResult", "", &usage_json(3, 1, 0, 0)),
            usage_entry("compaction", Some("c1"), TS, &usage_json(7, 0, 0, 0)),
            usage_entry("branch_summary", Some("b1"), TS, &usage_json(11, 0, 0, 0)),
            r#"{"type":"model_change","id":"m1","timestamp":1788256800}"#.to_string(),
        ],
    );
    let mut entries = collect_from(std::slice::from_ref(&base)).unwrap();
    entries.sort_by_key(|e| e.input_tokens);
    // user 消息与 model_change 条目跳过, toolResult/compaction/branch_summary 入账
    assert_eq!(entries.len(), 3);
    assert_eq!(entries[0].input_tokens, 3);
    assert_eq!(entries[0].model, "unknown");
    assert_eq!(entries[1].input_tokens, 7);
    assert_eq!(entries[2].input_tokens, 11);
    fs::remove_dir_all(&base).ok();
}

#[test]
fn test_header_required_and_nested_layout() {
    let base = temp_dir();
    // 首条有效 JSON 不是 session header: 整个文件跳过
    write_file(
        &base,
        "bad.jsonl",
        &[message_entry(
            Some("a1"),
            TS,
            "assistant",
            "",
            &usage_json(1, 1, 0, 0),
        )],
    );
    // <project>/*.jsonl 嵌套布局应被发现
    let proj = base.join("proj-a");
    fs::create_dir_all(&proj).unwrap();
    let lines = [
        header("s-3", TS),
        message_entry(
            Some("a1"),
            TS,
            "assistant",
            &model_field("m"),
            &usage_json(4, 4, 0, 0),
        ),
    ];
    fs::write(proj.join("s3.jsonl"), lines.join("\n") + "\n").unwrap();
    let entries = collect_from(std::slice::from_ref(&base)).unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].session_id.as_deref(), Some("s-3"));
    fs::remove_dir_all(&base).ok();
}

#[test]
fn test_id_dedup_last_wins_and_kind_scoped() {
    let base = temp_dir();
    write_file(
        &base,
        "s4.jsonl",
        &[
            header("s-4", TS),
            message_entry(
                Some("x1"),
                TS,
                "assistant",
                &model_field("first"),
                &usage_json(1, 1, 0, 0),
            ),
            message_entry(
                Some("x1"),
                TS + 1,
                "assistant",
                &model_field("second"),
                &usage_json(2, 2, 0, 0),
            ),
            usage_entry("compaction", Some("x1"), TS, &usage_json(5, 0, 0, 0)),
        ],
    );
    let entries = collect_from(std::slice::from_ref(&base)).unwrap();
    // 同 id 跨 kind 不互并; 同 kind 同 id 后到者覆盖
    assert_eq!(entries.len(), 2);
    let winner = entries.iter().find(|e| e.model == "second").unwrap();
    assert_eq!(winner.input_tokens, 2);
    assert!(!entries.iter().any(|e| e.model == "first"));
    fs::remove_dir_all(&base).ok();
}

#[test]
fn test_content_hash_dedup_without_id() {
    let base = temp_dir();
    let dup = message_entry(
        None,
        TS,
        "assistant",
        &model_field("m"),
        &usage_json(6, 6, 0, 0),
    );
    let other_ts = message_entry(
        None,
        TS + 9,
        "assistant",
        &model_field("m"),
        &usage_json(6, 6, 0, 0),
    );
    write_file(
        &base,
        "s5.jsonl",
        &[header("s-5", TS), dup.clone(), dup, other_ts],
    );
    let entries = collect_from(std::slice::from_ref(&base)).unwrap();
    // 逐字节相同的两行收敛为一笔; 时间戳不同视为独立用量
    assert_eq!(entries.len(), 2);
    fs::remove_dir_all(&base).ok();
}

#[test]
fn test_zero_and_malformed_skipped_with_header_ts_fallback() {
    let base = temp_dir();
    // 缺 entry 时间戳: 回退 header 时间戳
    let no_ts = format!(
        r#"{{"type":"message","id":"a2","message":{{"role":"assistant","model":"m","usage":{}}}}}"#,
        usage_json(8, 8, 0, 0)
    );
    write_file(
        &base,
        "s6.jsonl",
        &[
            "not-json".to_string(),
            header("s-6", TS),
            message_entry(
                Some("a1"),
                TS,
                "assistant",
                &model_field("m"),
                &usage_json(0, 0, 0, 0),
            ),
            no_ts,
        ],
    );
    let entries = collect_from(std::slice::from_ref(&base)).unwrap();
    // 前导畸形行不影响 header 判定; 全零 usage 跳过
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].created_at, TS);
    fs::remove_dir_all(&base).ok();
}

#[test]
fn test_missing_roots_return_empty() {
    let base = temp_dir();
    fs::remove_dir_all(&base).unwrap();
    assert!(collect_from(&[base]).unwrap().is_empty());
}
