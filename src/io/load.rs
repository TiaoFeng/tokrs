//! 从用户文件夹中读取每个agent的数据文件
//!
//! 日志文件可达 GB 级: 所有读取均为流式(逐行/逐块), 峰值内存 O(单行),
//! 修改原本的读取逻辑(整读 + DOM 驻留会耗尽内存)
//!
use serde_json::Value;
use std::{
    fs,
    io::{BufRead, BufReader},
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use super::progress::Progress;
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

/// 递归收集 base 下指定扩展名的普通文件（深度不超过 max_depth，结果确定性排序）
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
        // 只跟随目录、只收集普通文件: symlink/FIFO/socket/设备一律跳过
        // (FIFO 等特殊文件会让读取永久阻塞; symlink 的 file_type.is_file() 为 false)
        if file_type.is_dir() {
            if depth < max_depth {
                collect_dir(&path, extension, depth + 1, max_depth, files);
            }
        } else if file_type.is_file()
            && path
                .extension()
                .and_then(|e| e.to_str())
                .is_some_and(|e| e.eq_ignore_ascii_case(extension))
        {
            files.push(path);
        }
    }
}

/// 单行字节上限: 正常日志单行为 KB~MB 级, 上限防损坏/异常巨型行把内存撑爆
const MAX_LINE_BYTES: usize = 16 * 1024 * 1024;

/// 进度感知变体: 每消费一段即向 progress 上报字节数(节流重绘在 Progress 内部);
/// 其余语义见 for_each_jsonl_impl 文档
pub fn for_each_jsonl_progress(
    path: &Path,
    needles: &[&str],
    progress: &mut Progress,
    on_line: impl FnMut(Value) -> bool,
) -> Result<(), AppError> {
    for_each_jsonl_impl(path, needles, MAX_LINE_BYTES, Some(progress), on_line)
}

/// 流式逐行读取 JSONL(私有内核)
///
/// 文件可达 GB 级, 禁止整读驻留: 逐行解析、处理完即释放, 峰值内存 O(单行)。
/// - 行含任一 needle 字节串才解析回调, 否则零分配跳过(空 needles 全放行);
///   needle 须为不含转义的 ASCII 字面量(如 "\"token_count\"")
/// - 行超过 max_line 整行跳过并每文件警告一次, 继续消费到换行为止
/// - 畸形行/无效 UTF-8 行跳过(from_slice 语义与整读版逐字一致, 含 CRLF/末行无换行)
/// - 文件打开/读失败返回 Err; 回调返回 false 提前终止
fn for_each_jsonl_impl(
    path: &Path,
    needles: &[&str],
    max_line: usize,
    mut progress: Option<&mut Progress>,
    mut on_line: impl FnMut(Value) -> bool,
) -> Result<(), AppError> {
    let file = fs::File::open(path).map_err(|e| io_err("open", path, e))?;
    let mut reader = BufReader::with_capacity(64 * 1024, file);
    let mut line: Vec<u8> = Vec::new();
    let mut warned = false;
    loop {
        line.clear();
        let mut oversized = false;
        let mut eof = true; // 本次行是否以 EOF 收尾(而非换行符)
        let mut any = false;
        loop {
            let buf = match reader.fill_buf() {
                Ok(buf) => buf,
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(io_err("read", path, e)),
            };
            if buf.is_empty() {
                break;
            }
            eof = false;
            any = true;
            match buf.iter().position(|&b| b == b'\n') {
                Some(nl) => {
                    // 超限后不再拷贝, 仅消费到换行为止
                    if !oversized && line.len() + nl <= max_line {
                        line.extend_from_slice(&buf[..nl]);
                    } else {
                        oversized = true;
                    }
                    reader.consume(nl + 1);
                    if let Some(p) = &mut progress {
                        p.add((nl + 1) as u64);
                    }
                }
                None => {
                    if !oversized && line.len() + buf.len() <= max_line {
                        line.extend_from_slice(buf);
                    } else {
                        oversized = true;
                    }
                    let len = buf.len();
                    reader.consume(len);
                    if let Some(p) = &mut progress {
                        p.add(len as u64);
                    }
                    continue; // 行未结束, 继续读
                }
            }
            break; // 换行处行结束
        }
        if eof && !any {
            return Ok(()); // 数据读完
        }
        if oversized {
            if !warned {
                warned = true;
                eprintln!(
                    "> {}: line(s) over {max_line} bytes skipped",
                    path.display()
                );
            }
            continue;
        }
        if !needles.is_empty() && !line_contains(&line, needles) {
            continue;
        }
        if let Ok(value) = serde_json::from_slice::<Value>(&line)
            && !on_line(value)
        {
            return Ok(());
        }
    }
}

/// 行是否包含任一 needle(字节级字面量比较)
fn line_contains(line: &[u8], needles: &[&str]) -> bool {
    needles.iter().filter(|n| !n.is_empty()).any(|n| {
        let n = n.as_bytes();
        line.windows(n.len()).any(|w| w == n)
    })
}

/// 文件列表总字节(进度条总量; metadata 失败按 0 计)
pub fn total_bytes(files: &[PathBuf]) -> u64 {
    files
        .iter()
        .map(|f| fs::metadata(f).map(|m| m.len()).unwrap_or(0))
        .sum()
}

/// 文件名字符串(无文件名时为空串)
pub fn file_name_str(path: &Path) -> &str {
    path.file_name().and_then(|n| n.to_str()).unwrap_or("")
}

/// 读取单个 JSON 对象文件（非 JSONL，如 gemini 的 session 文件）; 流式解析避免整读双缓冲
/// (打开/读错误经 serde 统一包装为 Corrupted, 调用方按整体失败处理)
pub fn read_json(path: &Path) -> Result<Value, AppError> {
    let file = fs::File::open(path).map_err(|e| io_err("open", path, e))?;
    serde_json::from_reader(file).map_err(|e| json_err(path, e))
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
