use super::*;
use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

/// 临时库: v2 形态(两表共存)
fn temp_db() -> Connection {
    temp_db_with(true)
}

/// 临时库: v1 老库(仅 message 表)
fn temp_v1_db() -> Connection {
    temp_db_with(false)
}

fn temp_db_with(with_session_message: bool) -> Connection {
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
    if with_session_message {
        conn.execute_batch(
            "CREATE TABLE session_message (id TEXT, session_id TEXT, type TEXT, time_created INTEGER, time_updated INTEGER, data TEXT);",
        )
        .unwrap();
    }
    conn
}

fn insert_message(conn: &Connection, id: &str, session_id: &str, data: &str) {
    conn.execute(
            "INSERT INTO message (id, session_id, time_created, time_updated, data) VALUES (?1, ?2, 1, 1, ?3)",
            [id, session_id, data],
        )
        .unwrap();
}

/// 插入 v2 session_message 行(kind 为 type 列: assistant/user/...)
fn insert_session_message(conn: &Connection, id: &str, session_id: &str, kind: &str, data: &str) {
    conn.execute(
            "INSERT INTO session_message (id, session_id, type, time_created, time_updated, data) VALUES (?1, ?2, ?3, 1, 1, ?4)",
            [id, session_id, kind, data],
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
fn test_parse_v2_session_messages() {
    let conn = temp_db();
    // v2 形态: data 无 role, 模型为 model.id 对象, 角色由 type 列判定
    insert_session_message(
        &conn,
        "m1",
        "s1",
        "assistant",
        r#"{"time":{"created":1788256800000,"completed":1788256801000},"model":{"id":"openrouter/anthropic/Claude-Sonnet-4-5","providerID":"openrouter","variant":"max"},"cost":0.42,"tokens":{"input":10,"output":5,"reasoning":3,"cache":{"read":100,"write":20}}}"#,
    );
    // 未完成消息跳过(无 time.completed)
    insert_session_message(
        &conn,
        "m2",
        "s1",
        "assistant",
        r#"{"time":{"created":1788256800000},"model":{"id":"deepseek-v4"},"tokens":{"input":7,"output":0}}"#,
    );
    // completed 为 null: 非整数时间戳, 同样视为未完成跳过
    insert_session_message(
        &conn,
        "m3",
        "s1",
        "assistant",
        r#"{"time":{"created":1788256800000,"completed":null},"model":{"id":"deepseek-v4"},"tokens":{"input":9,"output":9}}"#,
    );

    let db_path = std::path::Path::new(conn.path().unwrap()).to_path_buf();
    drop(conn);
    let entries = collect_from(&db_path).unwrap();
    std::fs::remove_file(&db_path).ok();
    assert_eq!(entries.len(), 1);
    let e = &entries[0];
    // model.id 归一化: 多级前缀剥除 + 小写
    assert_eq!(e.model, "claude-sonnet-4-5");
    assert_eq!(e.input_tokens, 10);
    assert_eq!(e.output_tokens, 8);
    assert_eq!(e.cache_read_tokens, 100);
    assert_eq!(e.cache_creation_tokens, 20);
    assert_eq!(e.created_at, 1_788_256_800);
    assert_eq!(e.session_id.as_deref(), Some("s1"));
    assert_eq!(e.self_cost_usd, Some(0.42));
}

#[test]
fn test_cross_table_dedup_and_legacy_only() {
    let conn = temp_db();
    // 迁移重复行: 同 id 在 v1/v2 两表都有(用量一致), 跨表去重只计一次
    insert_message(
        &conn,
        "dup",
        "s1",
        r#"{"role":"assistant","modelID":"deepseek-v4","tokens":{"input":10,"output":5,"reasoning":3,"cache":{"read":100,"write":20}},"time":{"created":1788256800000,"completed":1788256801000}}"#,
    );
    insert_session_message(
        &conn,
        "dup",
        "s1",
        "assistant",
        r#"{"model":{"id":"deepseek-v4"},"tokens":{"input":10,"output":5,"reasoning":3,"cache":{"read":100,"write":20}},"time":{"created":1788256800000,"completed":1788256801000}}"#,
    );
    // 迁移漏行: 仅存在 message 表也必须入账
    insert_message(
        &conn,
        "legacy",
        "s1",
        r#"{"role":"assistant","modelID":"m2","tokens":{"input":1,"output":2},"time":{"created":1788256800000,"completed":1788256801000}}"#,
    );

    let db_path = std::path::Path::new(conn.path().unwrap()).to_path_buf();
    drop(conn);
    let entries = collect_from(&db_path).unwrap();
    std::fs::remove_file(&db_path).ok();
    assert_eq!(entries.len(), 2);
    assert!(
        entries
            .iter()
            .any(|e| e.input_tokens == 10 && e.cache_read_tokens == 100)
    );
    assert!(
        entries
            .iter()
            .any(|e| e.model == "m2" && e.input_tokens == 1 && e.output_tokens == 2)
    );
}

#[test]
fn test_v2_invalid_rows_fall_back_to_legacy() {
    let conn = temp_db();
    // v2 未完成(缺 completed): 不能占用去重键, 同 id 的 v1 有效行兜底
    insert_session_message(
        &conn,
        "m1",
        "s1",
        "assistant",
        r#"{"time":{"created":1788256800000},"model":{"id":"v2-model"},"tokens":{"input":7,"output":0}}"#,
    );
    insert_message(
        &conn,
        "m1",
        "s1",
        r#"{"role":"assistant","modelID":"v1-model","tokens":{"input":10,"output":5},"time":{"created":1788256800000,"completed":1788256801000}}"#,
    );
    // v2 JSON 损坏: 同样回退同 id 的 v1 行
    insert_session_message(&conn, "m2", "s1", "assistant", "{not json");
    insert_message(
        &conn,
        "m2",
        "s1",
        r#"{"role":"assistant","modelID":"v1-model-2","tokens":{"input":1,"output":2},"time":{"created":1788256800000,"completed":1788256801000}}"#,
    );

    let db_path = std::path::Path::new(conn.path().unwrap()).to_path_buf();
    drop(conn);
    let entries = collect_from(&db_path).unwrap();
    std::fs::remove_file(&db_path).ok();
    assert_eq!(entries.len(), 2);
    // 实际入账的是 v1 数据(v2 的模型/数值被丢弃且不重复计数)
    assert!(
        entries
            .iter()
            .any(|e| e.model == "v1-model" && e.input_tokens == 10 && e.output_tokens == 5)
    );
    assert!(
        entries
            .iter()
            .any(|e| e.model == "v1-model-2" && e.input_tokens == 1 && e.output_tokens == 2)
    );
    assert!(!entries.iter().any(|e| e.model == "v2-model"));
}

#[test]
fn test_v2_ignores_non_assistant_types() {
    let conn = temp_db();
    insert_session_message(
        &conn,
        "m1",
        "s1",
        "user",
        r#"{"time":{"created":1788256800000,"completed":1788256801000},"tokens":{"input":5,"output":5}}"#,
    );
    insert_session_message(
        &conn,
        "m2",
        "s1",
        "synthetic",
        r#"{"time":{"created":1788256800000,"completed":1788256801000},"tokens":{"input":5,"output":5}}"#,
    );

    let db_path = std::path::Path::new(conn.path().unwrap()).to_path_buf();
    drop(conn);
    let entries = collect_from(&db_path).unwrap();
    std::fs::remove_file(&db_path).ok();
    assert!(entries.is_empty());
}

#[test]
fn test_v1_db_without_session_message() {
    let conn = temp_v1_db();
    insert_message(
        &conn,
        "m1",
        "s1",
        r#"{"role":"assistant","modelID":"deepseek-v4","tokens":{"input":10,"output":5},"time":{"created":1788256800000,"completed":1788256801000}}"#,
    );

    let db_path = std::path::Path::new(conn.path().unwrap()).to_path_buf();
    drop(conn);
    let entries = collect_from(&db_path).unwrap();
    std::fs::remove_file(&db_path).ok();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].model, "deepseek-v4");
    assert_eq!(entries[0].input_tokens, 10);
}

#[test]
fn test_v2_db_without_message_table() {
    let conn = temp_db();
    insert_session_message(
        &conn,
        "m1",
        "s1",
        "assistant",
        r#"{"time":{"created":1788256800000,"completed":1788256801000},"model":{"id":"deepseek-v4"},"tokens":{"input":10,"output":5}}"#,
    );
    // 未来版本可能删除 message 表: 仅 session_message 也要正常
    conn.execute_batch("DROP TABLE message").unwrap();

    let db_path = std::path::Path::new(conn.path().unwrap()).to_path_buf();
    drop(conn);
    let entries = collect_from(&db_path).unwrap();
    std::fs::remove_file(&db_path).ok();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].model, "deepseek-v4");
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
fn test_table_exists_distinguishes_missing_and_error() {
    // 正常库: 存在的表 true, 缺失表 false(静默跳过)
    let conn = temp_db();
    assert!(table_exists(&conn, "message").unwrap());
    assert!(!table_exists(&conn, "no_such_table").unwrap());
    drop(conn);

    // 损坏库: open 成功(header 延迟读取), 首次查询报 not a database;
    // 必须区分"表不存在"与"库读不了", 返回 Err 交给调用方警告
    let path = std::env::temp_dir().join(format!(
        "tokrs-opencode-corrupt-te-{}-{}.db",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::write(&path, "not a sqlite db").unwrap();
    let conn = Connection::open(&path).unwrap();
    assert!(table_exists(&conn, "message").is_err());
    drop(conn);
    std::fs::remove_file(&path).ok();
}

#[test]
fn test_db_path_resolution() {
    let home = Path::new("/home/u");
    let default = PathBuf::from("/home/u/.local/share/opencode/opencode.db");
    // 未设 → 默认
    assert_eq!(db_path(home, None, None), default);
    // XDG_DATA_HOME 覆盖数据目录
    assert_eq!(
        db_path(home, Some(OsStr::new("/xdg/data")), None),
        PathBuf::from("/xdg/data/opencode/opencode.db")
    );
    // OPENCODE_DB 绝对直用
    assert_eq!(
        db_path(home, None, Some(OsStr::new("/abs/db.sqlite"))),
        PathBuf::from("/abs/db.sqlite")
    );
    // OPENCODE_DB 相对路径基于数据目录拼接(不展开 ~, 对齐 cc-switch 字面语义)
    assert_eq!(
        db_path(
            home,
            Some(OsStr::new("/xdg/data")),
            Some(OsStr::new("rel.db"))
        ),
        PathBuf::from("/xdg/data/opencode/rel.db")
    );
    assert_eq!(
        db_path(home, None, Some(OsStr::new("~/weird"))),
        PathBuf::from("/home/u/.local/share/opencode/~/weird")
    );
    // OPENCODE_DB 空串 → 回退 XDG 链
    assert_eq!(
        db_path(home, Some(OsStr::new("/xdg/data")), Some(OsStr::new(""))),
        PathBuf::from("/xdg/data/opencode/opencode.db")
    );
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
