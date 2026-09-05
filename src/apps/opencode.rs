use std::collections::HashSet;
use std::path::Path;

use rusqlite::{Connection, OpenFlags};
use serde_json::Value;

use crate::error::{AppError, sqlite_err};
use crate::io::load;
use crate::model::{AppKind, UsageEntry};

pub fn collect() -> Result<Vec<UsageEntry>, AppError> {
    let db_path = load::home_dir()?
        .join(".local")
        .join("share")
        .join("opencode")
        .join("opencode.db");
    if !db_path.is_file() {
        return Ok(Vec::new());
    }
    collect_from(&db_path)
}

pub fn collect_from(db_path: &Path) -> Result<Vec<UsageEntry>, AppError> {
    if !db_path.is_file() {
        return Ok(Vec::new());
    }
    let conn = Connection::open_with_flags(db_path, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .map_err(|e| sqlite_err(db_path, e))?;
    let mut stmt = conn
        .prepare("SELECT session_id, id, data FROM message")
        .map_err(|e| sqlite_err(db_path, e))?;
    let rows = stmt
        .query_map([], |row| {
            Ok((
                row.get::<_, Option<String>>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
            ))
        })
        .map_err(|e| sqlite_err(db_path, e))?;

    let mut seen: HashSet<String> = HashSet::new();
    let mut entries = Vec::new();
    for row in rows {
        let (session_id, message_id, data) = row.map_err(|e| sqlite_err(db_path, e))?;
        let Some(session_id) = session_id else {
            continue;
        };
        if !seen.insert(format!("{session_id}:{message_id}")) {
            continue;
        }
        parse_message(&data, &session_id, &mut entries);
    }
    Ok(entries)
}

fn parse_message(data: &str, session_id: &str, entries: &mut Vec<UsageEntry>) {
    let Ok(value) = serde_json::from_str::<Value>(data) else {
        return;
    };
    if load::str_get(&value, &["role"]) != Some("assistant") {
        return;
    }
    if value
        .pointer("/tokens")
        .and_then(Value::as_object)
        .is_none()
    {
        return;
    }
    if value.pointer("/time/completed").is_none() {
        return;
    }

    let input = load::u64_get(&value, &["tokens", "input"]);
    let output = load::u64_get(&value, &["tokens", "output"]);
    let reasoning = load::u64_get(&value, &["tokens", "reasoning"]);
    let cache_read = load::u64_get(&value, &["tokens", "cache", "read"]);
    let cache_write = load::u64_get(&value, &["tokens", "cache", "write"]);
    if input == 0 && output == 0 && reasoning == 0 && cache_read == 0 && cache_write == 0 {
        return;
    }

    let model = load::str_get(&value, &["modelID"])
        .unwrap_or("unknown")
        .to_string();
    let created_at = value
        .pointer("/time/created")
        .and_then(load::timestamp_to_epoch)
        .unwrap_or_else(load::now_epoch);

    entries.push(UsageEntry {
        app: AppKind::OpenCode,
        model,
        session_id: Some(session_id.to_string()),
        created_at,
        input_tokens: input,
        output_tokens: output + reasoning,
        cache_read_tokens: cache_read,
        cache_creation_tokens: cache_write,
    });
}

#[cfg(test)]
mod tests {
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
            r#"{"role":"assistant","modelID":"deepseek-v4","tokens":{"input":10,"output":5,"reasoning":3,"cache":{"read":100,"write":20}},"time":{"created":1788256800000,"completed":1788256801000}}"#,
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
    }

    #[test]
    fn test_missing_db_returns_empty() {
        let path = std::env::temp_dir().join("tokrs-opencode-nonexistent.db");
        assert!(collect_from(&path).unwrap().is_empty());
    }
}
