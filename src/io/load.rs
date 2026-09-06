//! 从用户文件夹中读取每个agent的数据文件
//!
use serde_json::Value;
use std::{
    fs,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use crate::error::{AppError, io_err, json_err};

/// 返回用户home地址
pub fn home_dir() -> Result<PathBuf, AppError> {
    std::env::home_dir().ok_or_else(|| AppError::Io {
        operation: "resolve home directory",
        path: "$HOME".to_string(),
        source: std::io::Error::new(std::io::ErrorKind::NotFound, "home directory not found"),
    })
}

pub fn now_epoch() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// 递归收集 base 下指定扩展名的文件（深度不超过 max_depth，跳过符号链接，结果确定性排序）
pub fn discover_files(base: &Path, extension: &str, max_depth: usize) -> Vec<PathBuf> {
    let mut files = Vec::new();
    collect_dir(base, extension, 0, max_depth, &mut files);
    files.sort();
    files
}

fn collect_dir(
    dir: &Path,
    extension: &str,
    depth: usize,
    max_depth: usize,
    files: &mut Vec<PathBuf>,
) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        let path = entry.path();
        if file_type.is_symlink() {
            continue;
        }
        if file_type.is_dir() {
            if depth < max_depth {
                collect_dir(&path, extension, depth + 1, max_depth, files);
            }
        } else if path
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| e.eq_ignore_ascii_case(extension))
        {
            files.push(path);
        }
    }
}

/// 逐行读取 JSONL，返回解析成功的行；畸形行直接跳过
pub fn read_jsonl(path: &Path) -> Result<Vec<Value>, AppError> {
    let content = fs::read(path).map_err(|e| io_err("read", path, e))?;
    let mut out = Vec::new();
    for line in content.split(|b| *b == b'\n') {
        if line.is_empty() {
            continue;
        }
        if let Ok(value) = serde_json::from_slice::<Value>(line) {
            out.push(value);
        }
    }
    Ok(out)
}

/// 读取单个 JSON 对象文件（非 JSONL，如 gemini 的 session 文件）
pub fn read_json(path: &Path) -> Result<Value, AppError> {
    let content = fs::read(path).map_err(|e| io_err("read", path, e))?;
    serde_json::from_slice(&content).map_err(|e| json_err(path, e))
}

/// 从 JSON 值按路径取 u64，缺失或类型不符返回 0
pub fn u64_get(value: &Value, keys: &[&str]) -> u64 {
    match get_nested(value, keys) {
        Some(v) => v
            .as_u64()
            .unwrap_or_else(|| v.as_f64().map(|f| f.max(0.0) as u64).unwrap_or(0)),
        None => 0,
    }
}

/// 从 JSON 值按路径取字符串
pub fn str_get<'a>(value: &'a Value, keys: &[&str]) -> Option<&'a str> {
    get_nested(value, keys).and_then(Value::as_str)
}

/// 从 JSON 值按路径取正成本(USD), 缺失/非正/非数返回 None
pub fn cost_get(value: &Value, keys: &[&str]) -> Option<f64> {
    let raw = get_nested(value, keys)?;
    let cost = raw.as_f64()?;
    (cost > 0.0 && cost.is_finite()).then_some(cost)
}

/// 从 JSON 值按路径取 bool，缺失或类型不符返回 false
pub fn bool_get(value: &Value, keys: &[&str]) -> bool {
    get_nested(value, keys)
        .and_then(Value::as_bool)
        .unwrap_or(false)
}

/// 时间戳自适应解析：数字（秒/毫秒）、数字字符串或 RFC3339，统一转 epoch 秒
pub fn timestamp_to_epoch(value: &Value) -> Option<i64> {
    if let Some(n) = value.as_i64() {
        return Some(if n > 100_000_000_000 { n / 1000 } else { n });
    }
    let s = value.as_str()?;
    if let Ok(n) = s.parse::<i64>() {
        return Some(if n > 100_000_000_000 { n / 1000 } else { n });
    }
    chrono::DateTime::parse_from_rfc3339(s)
        .ok()
        .map(|dt| dt.timestamp())
}

fn get_nested<'a>(value: &'a Value, keys: &[&str]) -> Option<&'a Value> {
    let mut current = value;
    for key in keys {
        current = current.get(key)?;
    }
    Some(current)
}

#[cfg(test)]
#[path = "../tests/io_load_test.rs"]
mod tests;
