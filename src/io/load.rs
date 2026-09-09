//! 从用户文件夹中读取每个agent的数据文件
//!
//! 日志文件可达 GB 级: 所有读取均为流式(逐行/逐块), 峰值内存 O(单行),
//! 修改原本的读取逻辑(整读 + DOM 驻留会耗尽内存)
//! 单文件失败统一 warn_file 警告后跳过(不中止全局); 单对象文件(如 gemini
//! session)经 ProgressReader + serde Visitor 流式逐条解析(峰值 O(单条消息));
//! 文件级并行扫描(map_files: 原子索引抢占 + 结果按序回填, 合并语义与串行一致)
//!
use serde_json::Value;
use std::{
    ffi::OsStr,
    fs,
    io::{BufRead, BufReader, Read},
    path::{Path, PathBuf},
    sync::Mutex,
    sync::atomic::{AtomicUsize, Ordering},
    time::{SystemTime, UNIX_EPOCH},
};

use super::progress::Progress;
use crate::error::{AppError, io_err};

/// 返回用户home地址
pub fn home_dir() -> Result<PathBuf, AppError> {
    std::env::home_dir().ok_or_else(|| AppError::Io {
        operation: "resolve home directory",
        path: "$HOME".to_string(),
        source: std::io::Error::new(std::io::ErrorKind::NotFound, "home directory not found"),
    })
}

/// 环境变量路径值归一(各 app 数据根目录覆盖用)
///
/// trim + 空串视为未设置; `~`/`~/`/`~\` 前缀展开为 home 下路径;
/// 其余须为绝对路径, 非绝对 stderr 警告一行后返回 None(调用方回退默认)——
/// 防"设了非法值却静默读错库"无信号
pub fn env_abs_path(var: &str, raw: Option<&OsStr>, home: &Path) -> Option<PathBuf> {
    let s = raw?.to_string_lossy().trim().to_string();
    if s.is_empty() {
        return None;
    }
    let path = if s == "~" {
        home.to_path_buf()
    } else if let Some(suffix) = s.strip_prefix("~/").or_else(|| s.strip_prefix("~\\")) {
        home.join(suffix)
    } else {
        PathBuf::from(&s)
    };
    if path.is_absolute() {
        Some(path)
    } else {
        eprintln!("> {var}='{s}' 不是绝对路径, 已忽略(回退默认路径)");
        None
    }
}

/// XDG 数据根目录: XDG_DATA_HOME(空串视为未设置) > ~/.local/share
pub fn xdg_data_dir(home: &Path, raw: Option<&OsStr>) -> PathBuf {
    match raw {
        Some(v) if !v.to_string_lossy().trim().is_empty() => {
            PathBuf::from(v.to_string_lossy().trim())
        }
        _ => home.join(".local").join("share"),
    }
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
    progress: &Progress,
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
    progress: Option<&Progress>,
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
                    if let Some(p) = progress {
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
                    if let Some(p) = progress {
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
    needles
        .iter()
        .filter(|n| !n.is_empty())
        .any(|n| contains_needle(line, n.as_bytes()))
}

/// 两字节锚点快扫: 逐位置内联比较 needle 前两字节, 命中锚点才 memcmp 验证全串
///
/// 旧实现 `windows().any(w == n)` 对每个字节位置做一次 memcmp 调用, 绝大多数
/// 位置首字节即失败但调用开销不减; GB 级扫描中该预过滤是热路径. 两字节锚点
/// 把逐位置开销降为两次内联字节比较(随机文本锚点命中概率 ~1/65536), 预期
/// 提速 3~8x; needle 前两字节按调用方字面量分布选取(如 codex 的 `"s`/`"t`)
fn contains_needle(line: &[u8], needle: &[u8]) -> bool {
    let n = needle.len();
    if n == 0 || n > line.len() {
        return false;
    }
    if n == 1 {
        return line.iter().any(|&b| b == needle[0]);
    }
    let (a0, a1) = (needle[0], needle[1]);
    let last = line.len() - n; // 最后一个可尝试起点
    for i in 0..=last {
        if line[i] == a0 && line[i + 1] == a1 && &line[i..i + n] == needle {
            return true;
        }
    }
    false
}

/// 并行扫描线程数上限(命令层 --threads 与内核共用同一上界, 消除魔数)
///
/// 解析+页缓存读的并行收益在 ~8-16 线程后趋平; 同时限制最坏内存
/// (线程数 × 单文件峰值, 单行缓冲上限 16MiB)
pub const MAX_THREADS: usize = 16;

/// 并行默认线程数: min(CPU 逻辑核数, MAX_THREADS, 文件数)
///
/// 文件数少时避免空转线程
pub fn auto_threads(files: usize) -> usize {
    let cores = std::thread::available_parallelism().map_or(1, |n| n.get());
    cores.min(MAX_THREADS).clamp(1, files.max(1))
}

/// 文件级并行解析内核(零新依赖, std::thread::scope)
///
/// workers 经原子索引动态抢占下一个文件(单巨文件不阻塞其它文件), 结果按
/// (idx, T) 收集, join 后按 idx 重排——构造性有序, 各 app 跨文件合并语义
/// (last-wins/first-wins/交换律)与串行执行逐字一致;
/// threads 会被 min(文件数) 截断, <=1 时直接顺序执行(等价串行路径, 无线程开销);
/// 进度: 每个文件完成即 file_done(), 字节由解析闭包内部经 progress.add 上报;
/// 解析闭包须无跨文件可变状态(各 app 的文件内状态均在闭包内创建)
pub fn map_files<T, F>(files: &[PathBuf], threads: usize, progress: &Progress, parse: F) -> Vec<T>
where
    F: Fn(&Path, &Progress) -> T + Sync,
    T: Send,
{
    let workers = threads.max(1).min(files.len());
    if workers <= 1 {
        return files
            .iter()
            .map(|file| {
                let out = parse(file, progress);
                progress.file_done();
                out
            })
            .collect();
    }
    let next = AtomicUsize::new(0);
    let results: Mutex<Vec<(usize, T)>> = Mutex::new(Vec::with_capacity(files.len()));
    std::thread::scope(|scope| {
        for _ in 0..workers {
            scope.spawn(|| {
                loop {
                    let idx = next.fetch_add(1, Ordering::Relaxed);
                    if idx >= files.len() {
                        break;
                    }
                    let out = parse(&files[idx], progress);
                    progress.file_done();
                    results.lock().unwrap().push((idx, out));
                }
            });
        }
    });
    let mut slots: Vec<Option<T>> = (0..files.len()).map(|_| None).collect();
    for (idx, out) in results.into_inner().unwrap() {
        slots[idx] = Some(out);
    }
    slots.into_iter().map(Option::unwrap).collect()
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

/// 单文件失败统一警告(stderr 单行, AppError Display 自带操作与路径上下文)
///
/// 各 app 的单文件读取/解析失败一律警告后跳过继续, 不中止全局统计;
/// 全局性错误(home 解析失败/pricing.json 损坏)仍硬退出
pub fn warn_file(err: &AppError) {
    eprintln!("> {err}");
}

/// 进度感知 reader: 每读到一段字节即向 progress 上报
///
/// 用于无法逐行流式的单一对象文件(如 gemini 的 session JSON):
/// 配合 serde_json Deserializer::from_reader(IoRead 真流式)实现字节驱动进度;
/// Progress 为廉价 Clone 句柄(Arc<Mutex>), 按值持有
pub struct ProgressReader<R> {
    inner: R,
    progress: Progress,
}

impl<R: Read> ProgressReader<R> {
    pub fn new(inner: R, progress: Progress) -> Self {
        ProgressReader { inner, progress }
    }
}

impl<R: Read> Read for ProgressReader<R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let n = self.inner.read(buf)?;
        self.progress.add(n as u64);
        Ok(n)
    }
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

/// 从 JSON 值按路径取非空字符串(trim 后非空, 纯空白视为缺失)
pub fn str_get_nonempty<'a>(value: &'a Value, keys: &[&str]) -> Option<&'a str> {
    str_get(value, keys).filter(|s| !s.trim().is_empty())
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
