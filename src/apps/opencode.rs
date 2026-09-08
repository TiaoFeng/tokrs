use rusqlite::{Connection, OpenFlags};
use serde_json::Value;
use std::{collections::HashSet, path::Path};

use crate::{
    apps::normalize_model,
    error::{AppError, sqlite_err},
    io::{load, progress},
    model::{AppKind, UsageEntry},
};

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
    // 无文件字节流可推进(SQLite 单查询), 仅 TTY 起止提示
    progress::stderr_note(&format!("opencode: scanning {}", db_path.display()));
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
    progress::stderr_note("opencode: done");
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
    // opencode 自报聚合成本(USD), >0 时无条件优先于定价表
    let self_cost = load::cost_get(&value, &["cost"]);
    // token 全零但有真实扣费(如失败仍计价的请求)时保留
    if input == 0
        && output == 0
        && reasoning == 0
        && cache_read == 0
        && cache_write == 0
        && self_cost.is_none()
    {
        return;
    }

    let model =
        load::str_get(&value, &["modelID"]).map_or_else(|| "unknown".to_string(), normalize_model);
    let created_at = value
        .pointer("/time/created")
        .and_then(load::timestamp_to_epoch)
        .unwrap_or_else(load::now_epoch);

    entries.push(UsageEntry::new(
        AppKind::OpenCode,
        model,
        Some(session_id.to_string()),
        created_at,
        input,
        output + reasoning,
        cache_read,
        cache_write,
        self_cost,
    ));
}

#[cfg(test)]
#[path = "tests/opencode_test.rs"]
mod tests;
