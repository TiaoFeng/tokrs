//! 扫描进度条(stderr, 仅 TTY 生效)
//!
//! 句柄式进度(Arc<Mutex<>> 内核, 可 Clone 跨线程共享): 字节驱动 + 文件计数;
//! 并行扫描下显示"N/M files"与 err 计数(单文件名展示在并行下语义不成立,
//! 出错文件的路径由各解析器的警告行逐行输出, 不会被 \r 重绘覆盖);
//! stdout 零污染(--json/管道不受影响), stderr 非 TTY 时所有渲染自动静默
//!
use std::io::{IsTerminal, stderr};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// 重绘节流间隔
const REDRAW_INTERVAL: Duration = Duration::from_millis(100);
/// 进度条字符宽
const BAR_WIDTH: usize = 40;
/// 整行最大渲染宽度(超出截断, 右侧补空格擦除残留)
const LINE_WIDTH: usize = 128;

/// 进度内核状态(锁内持有)
struct Inner {
    total: u64,
    done: u64,
    files_total: usize,
    files_done: usize,
    errors: usize,
    last_render: Option<Instant>,
    finished: bool,
}

/// 扫描进度(字节 + 文件计数驱动, 可跨线程共享)
#[derive(Clone)]
pub struct Progress {
    label: &'static str,
    tty: bool,
    inner: Arc<Mutex<Inner>>,
}

impl Progress {
    /// 创建进度(total_bytes 为本 app 待扫描总字节, files_total 为文件总数)
    pub fn start(label: &'static str, total_bytes: u64, files_total: usize) -> Self {
        Self {
            label,
            tty: stderr().is_terminal(),
            inner: Arc::new(Mutex::new(Inner {
                total: total_bytes,
                done: 0,
                files_total,
                files_done: 0,
                errors: 0,
                last_render: None,
                finished: false,
            })),
        }
    }

    /// 累计已消费字节数(内部节流重绘)
    pub fn add(&self, bytes: u64) {
        let throttled = {
            let mut inner = self.inner.lock().expect("progress mutex poisoned");
            inner.done += bytes;
            inner
                .last_render
                .is_some_and(|t| t.elapsed() < REDRAW_INTERVAL)
        };
        if !throttled {
            self.draw();
        }
    }

    /// 标记一个文件处理完成(立即重绘, 文件计数可见)
    pub fn file_done(&self) {
        self.inner
            .lock()
            .expect("progress mutex poisoned")
            .files_done += 1;
        self.draw();
    }

    /// 记一次文件级错误(进度条显示 err 计数; 文件名由警告行输出)
    pub fn note_error(&self) {
        self.inner.lock().expect("progress mutex poisoned").errors += 1;
        self.draw();
    }

    /// 完成收尾: 渲染最终帧后补换行; 幂等
    pub fn finish(&self) {
        if !self.tty {
            self.inner.lock().expect("progress mutex poisoned").finished = true;
            return;
        }
        let mut inner = self.inner.lock().expect("progress mutex poisoned");
        if inner.finished {
            return;
        }
        self.draw_frame(&mut inner);
        inner.finished = true;
        drop(inner);
        eprintln!();
    }

    fn draw(&self) {
        if !self.tty {
            return;
        }
        let mut inner = self.inner.lock().expect("progress mutex poisoned");
        if inner.finished {
            return;
        }
        self.draw_frame(&mut inner);
    }

    /// 渲染一帧(调用方持锁并负责 finished 检查)
    fn draw_frame(&self, inner: &mut Inner) {
        inner.last_render = Some(Instant::now());
        let pct = if inner.total > 0 {
            (inner.done.min(inner.total)) as f64 / inner.total as f64 * 100.0
        } else {
            100.0
        };
        let filled = ((pct / 100.0) * BAR_WIDTH as f64).round() as usize;
        let filled = filled.min(BAR_WIDTH);
        // 标准下载条: 箭头仅在 0 < filled < BAR_WIDTH 时出现(0% 空条/100% 满条不带箭头)
        let (head, rest) = match filled {
            0 => (String::new(), ".".repeat(BAR_WIDTH)),
            BAR_WIDTH => ("=".repeat(BAR_WIDTH), String::new()),
            _ => (
                format!("{}>", "=".repeat(filled - 1)),
                ".".repeat(BAR_WIDTH - filled),
            ),
        };
        let files = format!(" {:>4}/{} files", inner.files_done, inner.files_total);
        let err = if inner.errors > 0 {
            format!(", {} err", inner.errors)
        } else {
            String::new()
        };
        let msg = format!(
            "{} [{head}{rest}] {:>5.1}% {:>9}/{:<9}{files}{err}",
            self.label,
            pct,
            fmt_bytes(inner.done),
            fmt_bytes(inner.total),
        );
        // 右侧补空格擦除上一帧残留, 整行截断防终端折行
        let msg = truncate(&msg, LINE_WIDTH);
        eprint!("\r{msg:<LINE_WIDTH$}");
    }
}

/// TTY-only 单行提示(非 TTY 静默; 如 opencode 无文件流的起止提示)
pub fn stderr_note(msg: &str) {
    if stderr().is_terminal() {
        eprintln!("{msg}");
    }
}

/// 人类可读字节数: 0B / 512B / 1.5KB / 18.0GB
pub fn fmt_bytes(n: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut v = n as f64;
    for (i, unit) in UNITS.iter().enumerate() {
        if v < 1024.0 || i == UNITS.len() - 1 {
            return if i == 0 {
                format!("{n}B")
            } else {
                format!("{v:.1}{unit}")
            };
        }
        v /= 1024.0;
    }
    unreachable!(
        "The last item in `units` will always return a result; this branch is unreachable."
    )
}

/// 字符级截断(渲染行宽控制)
fn truncate(s: &str, max_chars: usize) -> String {
    s.chars().take(max_chars).collect()
}

#[cfg(test)]
mod tests {
    use super::{Progress, fmt_bytes};

    #[test]
    fn test_fmt_bytes() {
        assert_eq!(fmt_bytes(0), "0B");
        assert_eq!(fmt_bytes(512), "512B");
        assert_eq!(fmt_bytes(1536), "1.5KB");
        assert_eq!(fmt_bytes(18 * 1024 * 1024 * 1024), "18.0GB");
    }

    #[test]
    fn test_progress_non_tty_silent() {
        // 测试环境 stderr 非 TTY: 全流程静默, 只验证不 panic 与 finish 幂等
        let progress = Progress::start("test", 1000, 4);
        progress.add(400);
        progress.file_done();
        progress.note_error();
        progress.add(600);
        progress.file_done();
        progress.finish();
        progress.finish();
    }

    #[test]
    fn test_progress_clone_shares_state() {
        // 句柄 Clone 共享同一内核: 跨"线程"句柄的计数汇入同一进度
        let progress = Progress::start("test", 100, 2);
        let cloned = progress.clone();
        cloned.file_done();
        progress.file_done();
        cloned.note_error();
        progress.finish();
        cloned.finish(); // 幂等: 不再补换行
    }
}
