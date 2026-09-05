use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::Value;

use crate::error::{AppError, io_err};

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
mod tests {
    use super::*;
    use std::fs;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "tokrs-load-{name}-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn test_discover_files_respects_extension_and_depth() {
        let base = temp_dir("discover");
        fs::create_dir_all(base.join("a/b")).unwrap();
        fs::write(base.join("root.jsonl"), "{}").unwrap();
        fs::write(base.join("root.txt"), "{}").unwrap();
        fs::write(base.join("a/mid.jsonl"), "{}").unwrap();
        fs::write(base.join("a/b/deep.jsonl"), "{}").unwrap();

        let found = discover_files(&base, "jsonl", 1);
        let names: Vec<String> = found
            .iter()
            .map(|p| p.strip_prefix(&base).unwrap().to_string_lossy().to_string())
            .collect();
        assert_eq!(names, vec!["a/mid.jsonl", "root.jsonl"]);
        fs::remove_dir_all(&base).ok();
    }

    #[test]
    fn test_discover_files_missing_base_is_empty() {
        let base = temp_dir("missing");
        fs::remove_dir_all(&base).ok();
        assert!(discover_files(&base, "jsonl", 3).is_empty());
    }

    #[test]
    fn test_read_jsonl_skips_malformed_lines() {
        let path = temp_dir("jsonl").join("f.jsonl");
        fs::write(&path, "{\"a\":1}\nnot-json\n{\"a\":2}\n\n{\"a\":3}").unwrap();
        let rows = read_jsonl(&path).unwrap();
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[2]["a"], 3);
    }

    #[test]
    fn test_u64_and_str_get() {
        let v: Value = serde_json::from_str(r#"{"a":{"b":42},"s":{"t":"x"},"f":1.7}"#).unwrap();
        assert_eq!(u64_get(&v, &["a", "b"]), 42);
        assert_eq!(u64_get(&v, &["f"]), 1);
        assert_eq!(u64_get(&v, &["nope"]), 0);
        assert_eq!(str_get(&v, &["s", "t"]), Some("x"));
        assert_eq!(str_get(&v, &["a", "t"]), None);
    }

    #[test]
    fn test_timestamp_to_epoch() {
        let v: Value = serde_json::from_str(
            "[1767000000, 1767000000000, \"1767000000\", \"2026-09-01T00:00:00Z\"]",
        )
        .unwrap();
        let arr = v.as_array().unwrap();
        assert_eq!(timestamp_to_epoch(&arr[0]), Some(1_767_000_000));
        assert_eq!(timestamp_to_epoch(&arr[1]), Some(1_767_000_000));
        assert_eq!(timestamp_to_epoch(&arr[2]), Some(1_767_000_000));
        assert_eq!(timestamp_to_epoch(&arr[3]), Some(1_788_220_800));
    }
}
