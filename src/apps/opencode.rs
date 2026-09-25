//! opencode日志解析（更新并支持 v2）
//!
//! v2 起会话消息迁至 session_message 表
//! v1 message 表冻结为历史行且可能含有迁移行. 需要双表读取并共享去重
//!
use rusqlite::{Connection, OpenFlags};
use serde_json::Value;
use std::{
    collections::HashSet,
    ffi::OsStr,
    path::{Path, PathBuf},
};

use crate::{
    apps::normalize_model,
    error::{AppError, sqlite_err},
    io::{load, progress},
    model::{AppKind, UsageEntry},
};

/// opencode 数据库路径(对齐 cc-switch opencode_config.rs:64-90):
/// OPENCODE_DB(空串忽略; 绝对直用; 相对路径基于数据目录拼接, 不展开 ~ 同
/// cc-switch 字面语义) > XDG_DATA_HOME > ~/.local/share/opencode/opencode.db
fn db_path(home: &Path, xdg: Option<&OsStr>, custom: Option<&OsStr>) -> PathBuf {
    let data_dir = load::xdg_data_dir(home, xdg).join("opencode");
    match custom {
        Some(raw) if !raw.to_string_lossy().is_empty() => {
            let path = PathBuf::from(raw);
            if path.is_absolute() {
                path
            } else {
                data_dir.join(path)
            }
        }
        _ => data_dir.join("opencode.db"),
    }
}

/// 日志(v1 message / v2 session_message)
#[derive(Clone, Copy, PartialEq, Eq)]
enum MessageTable {
    /// v1: data.role 判断 assistant, 模型为 modelID 字符串
    Message,
    /// v2: SQL 按 type='assistant' 过滤, 不含 role, 模型为 model.id 对象
    SessionMessage,
}

impl MessageTable {
    /// 库内表名(常量字面值)
    fn name(self) -> &'static str {
        match self {
            MessageTable::Message => "message",
            MessageTable::SessionMessage => "session_message",
        }
    }

    /// 两代查询语句(v2 在 SQL 层过滤 assistant, v1 交给 parse 按照 role 过滤)
    fn sql(self) -> &'static str {
        match self {
            MessageTable::Message => "SELECT session_id, id, data FROM message",
            MessageTable::SessionMessage => {
                "SELECT session_id, id, data FROM session_message WHERE type = 'assistant'"
            }
        }
    }

    /// v1 data 含有 role 需要校验; v2 已由 type 列过滤
    fn requires_role(self) -> bool {
        self == MessageTable::Message
    }
}

pub fn collect() -> Result<Vec<UsageEntry>, AppError> {
    let home = load::home_dir()?;
    let db = db_path(
        &home,
        std::env::var_os("XDG_DATA_HOME").as_deref(),
        std::env::var_os("OPENCODE_DB").as_deref(),
    );
    if !db.is_file() {
        return Ok(Vec::new());
    }
    collect_from(&db)
}

pub fn collect_from(db_path: &Path) -> Result<Vec<UsageEntry>, AppError> {
    if !db_path.is_file() {
        return Ok(Vec::new());
    }
    // 无文件字节流可推进(SQLite 单查询), 仅 TTY 起止提示
    progress::stderr_note(&format!(">_: opencode: scanning {}", db_path.display()));
    // 单 DB 源不可用: 警告后返回空(与其余 app 的单文件失败策略一致), 不中止全局
    let Some(conn) = warn_sqlite(
        Connection::open_with_flags(db_path, OpenFlags::SQLITE_OPEN_READ_ONLY)
            .map_err(|e| sqlite_err(db_path, e)),
    ) else {
        return Ok(Vec::new());
    };
    // v2 session_message 优先, 之后读取 v1 message 中的记录;
    // 两表共享 seen((session_id, message_id)) 跨表去重, 不会重复计算
    let mut seen: HashSet<(String, String)> = HashSet::new();
    let mut entries = Vec::new();
    'tables: for table in [MessageTable::SessionMessage, MessageTable::Message] {
        match table_exists(&conn, table.name()) {
            Ok(true) => collect_table(&conn, db_path, table, &mut seen, &mut entries),
            Ok(false) => {}
            Err(e) => {
                // 库级失败(损坏/不可读): 警告后保留已收集条目, 不中止全局
                load::warn_file(&sqlite_err(db_path, e));
                break 'tables;
            }
        }
    }
    progress::stderr_note(">_: opencode: done");
    Ok(entries)
}

/// 单表收集: prepare/查询失败警告后跳过该表(不中止全局),
/// 行级失败警告后保留已收集条目
fn collect_table(
    conn: &Connection,
    db_path: &Path,
    table: MessageTable,
    seen: &mut HashSet<(String, String)>,
    entries: &mut Vec<UsageEntry>,
) {
    let Some(mut stmt) = warn_sqlite(
        conn.prepare(table.sql())
            .map_err(|e| sqlite_err(db_path, e)),
    ) else {
        return;
    };
    let Some(rows) = warn_sqlite(
        stmt.query_map([], |row| {
            Ok((
                row.get::<_, Option<String>>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
            ))
        })
        .map_err(|e| sqlite_err(db_path, e)),
    ) else {
        return;
    };

    for row in rows {
        let (session_id, message_id, data) = match row {
            Ok(row) => row,
            Err(e) => {
                // 行级失败: 警告后保留已收集条目
                load::warn_file(&sqlite_err(db_path, e));
                break;
            }
        };
        let Some(session_id) = session_id else {
            continue;
        };
        let key = (session_id, message_id);
        if seen.contains(&key) {
            continue;
        }
        // 仅成功入账才占用去重键: v2 行无效(未完成/损坏/全零)时,
        // 同 id 的 v1 兜底行仍可入账, 先成功者胜不双算
        if let Some(entry) = parse_message(&data, key.0.as_str(), table) {
            seen.insert(key);
            entries.push(entry);
        }
    }
}

/// 表是否存在(v1 库无 session_message, v2 库两表共存; 未来删表也不误报)
///
/// 仅"无此行"(QueryReturnedNoRows)视为表缺失返回 Ok(false); 其余错误(如损坏库
/// not a database)原样 Err, 由调用方警告——区分"无表"与"读不了库"
fn table_exists(conn: &Connection, name: &str) -> Result<bool, rusqlite::Error> {
    match conn.query_row(
        "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1",
        [name],
        |_| Ok(()),
    ) {
        Ok(()) => Ok(true),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(false),
        Err(e) => Err(e),
    }
}

/// sqlite 步骤失败统一警告; 调用方按"该源空结果"继续, 不中止全局
fn warn_sqlite<T>(result: Result<T, AppError>) -> Option<T> {
    match result {
        Ok(value) => Some(value),
        Err(e) => {
            load::warn_file(&e);
            None
        }
    }
}

/// 解析单条消息为用量条目
///
/// 无效行返回 None(角色不符 / JSON 损坏 / 无 tokens / 未完成 / 全零且无自报成本);
/// 调用方仅在 Some 后占用去重键, 使 v2 无效时同 id 的 v1 兜底行仍可入账
fn parse_message(data: &str, session_id: &str, table: MessageTable) -> Option<UsageEntry> {
    let value = serde_json::from_str::<Value>(data).ok()?;
    // v1 按 data.role 判断 assistant; v2 由 type 列筛选
    if table.requires_role() && load::str_get(&value, &["role"]) != Some("assistant") {
        return None;
    }
    // 无 tokens 对象 / completed 非整数时间戳(ms; 缺失/null/字符串/浮点)均无效
    value.get("tokens").and_then(Value::as_object)?;
    value
        .get("time")
        .and_then(|t| t.get("completed"))
        .and_then(Value::as_i64)?;

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
        return None;
    }

    // 模型: v1 为 modelID 字符串, v2 为 model.id 对象, 空值统一记作 unknown
    let model = load::str_get(&value, &["modelID"])
        .or_else(|| load::str_get(&value, &["model", "id"]))
        .map_or_else(|| "unknown".to_string(), normalize_model);
    let created_at = value
        .get("time")
        .and_then(|t| t.get("created"))
        .and_then(load::timestamp_to_epoch)
        .unwrap_or_else(load::now_epoch);

    Some(UsageEntry::new(
        AppKind::OpenCode,
        model,
        Some(session_id.to_string()),
        created_at,
        input,
        output + reasoning,
        cache_read,
        cache_write,
        self_cost,
    ))
}

#[cfg(test)]
#[path = "tests/opencode_test.rs"]
mod tests;
