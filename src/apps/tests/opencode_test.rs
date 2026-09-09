use super::*;
use std::time::{SystemTime, UNIX_EPOCH};

fn temp_db() -> Connection {
    let path = std::env::temp_dir().join(format!(
        "tokrs-opencode-{}-{}.db",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let conn = Connection::open(&path).unwrap();
    conn.execute_batch(
            "CREATE TABLE session (id TEXT PRIMARY KEY, time_updated INTEGER);
             CREATE TABLE message (id TEXT, session_id TEXT, time_created INTEGER, time_updated INTEGER, data TEXT);",
        )
        .unwrap();
    conn
}

fn insert_message(conn: &Connection, id: &str, session_id: &str, data: &str) {
    conn.execute(
            "INSERT INTO message (id, session_id, time_created, time_updated, data) VALUES (?1, ?2, 1, 1, ?3)",
            [id, session_id, data],
        )
        .unwrap();
}

#[test]
fn test_parse_assistant_messages() {
    let conn = temp_db();
    conn.execute(
        "INSERT INTO session (id, time_updated) VALUES ('s1', 1)",
        [],
    )
    .unwrap();
    insert_message(
        &conn,
        "m1",
        "s1",
        r#"{"role":"assistant","modelID":"deepseek-v4","cost":0.42,"tokens":{"input":10,"output":5,"reasoning":3,"cache":{"read":100,"write":20}},"time":{"created":1788256800000,"completed":1788256801000}}"#,
    );
    insert_message(
        &conn,
        "m2",
        "s1",
        r#"{"role":"assistant","modelID":"deepseek-v4","tokens":{"input":1,"output":1},"time":{"created":1788256800000}}"#,
    );
    insert_message(
        &conn,
        "m3",
        "s1",
        r#"{"role":"user","tokens":{"input":5,"output":5},"time":{"created":1788256800000,"completed":1788256801000}}"#,
    );
    insert_message(
        &conn,
        "m4",
        "s1",
        r#"{"role":"assistant","modelID":"m","tokens":{"input":0,"output":0,"reasoning":0,"cache":{"read":0,"write":0}},"time":{"created":1788256800000,"completed":1788256801000}}"#,
    );

    let db_path = std::path::Path::new(conn.path().unwrap()).to_path_buf();
    drop(conn);
    let entries = collect_from(&db_path).unwrap();
    std::fs::remove_file(&db_path).ok();
    assert_eq!(entries.len(), 1);
    let e = &entries[0];
    assert_eq!(e.model, "deepseek-v4");
    assert_eq!(e.input_tokens, 10);
    assert_eq!(e.output_tokens, 8);
    assert_eq!(e.cache_read_tokens, 100);
    assert_eq!(e.cache_creation_tokens, 20);
    assert_eq!(e.created_at, 1_788_256_800);
    assert_eq!(e.session_id.as_deref(), Some("s1"));
    // 自报聚合成本被捕获
    assert_eq!(e.self_cost_usd, Some(0.42));
}

#[test]
fn test_zero_tokens_with_cost_imported() {
    let conn = temp_db();
    conn.execute(
        "INSERT INTO session (id, time_updated) VALUES ('s2', 1)",
        [],
    )
    .unwrap();
    insert_message(
        &conn,
        "m1",
        "s2",
        r#"{"role":"assistant","modelID":"free","cost":0.9,"tokens":{"input":0,"output":0,"reasoning":0,"cache":{"read":0,"write":0}},"time":{"created":1788256800000,"completed":1788256801000}}"#,
    );
    let db_path = std::path::Path::new(conn.path().unwrap()).to_path_buf();
    drop(conn);
    let entries = collect_from(&db_path).unwrap();
    std::fs::remove_file(&db_path).ok();
    // token 全零但有真实扣费: 保留以记录成本
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].total_tokens(), 0);
    assert_eq!(entries[0].self_cost_usd, Some(0.9));
}

#[test]
fn test_missing_db_returns_empty() {
    let path = std::env::temp_dir().join("tokrs-opencode-nonexistent.db");
    assert!(collect_from(&path).unwrap().is_empty());
}

#[test]
fn test_corrupted_db_warns_and_empty() {
    let path = std::env::temp_dir().join(format!(
        "tokrs-opencode-corrupt-{}-{}.db",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::write(&path, "not a sqlite db").unwrap();
    // DB 损坏: 警告后返回空结果, 不再 Err 中止全局
    assert!(collect_from(&path).unwrap().is_empty());
    std::fs::remove_file(&path).ok();
}

#[test]
fn test_model_normalization() {
    let conn = temp_db();
    conn.execute(
        "INSERT INTO session (id, time_updated) VALUES ('s1', 1)",
        [],
    )
    .unwrap();
    insert_message(
        &conn,
        "m1",
        "s1",
        r#"{"role":"assistant","modelID":"openrouter/anthropic/Claude-Sonnet-4-5","tokens":{"input":1,"output":1},"time":{"created":1788256800000,"completed":1788256801000}}"#,
    );
    let db_path = std::path::Path::new(conn.path().unwrap()).to_path_buf();
    drop(conn);
    let entries = collect_from(&db_path).unwrap();
    std::fs::remove_file(&db_path).ok();
    assert_eq!(entries.len(), 1);
    // 全 app 统一归一化: 多级前缀剥除 + 小写
    assert_eq!(entries[0].model, "claude-sonnet-4-5");
}
