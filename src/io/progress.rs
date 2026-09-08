//! 扫描进度条(stderr, 仅 TTY 生效)
//!
//! 字节驱动的单行动态进度条(与下载进度条同款 \r 重绘), 文件切换时以后缀实时显示;
//! stdout 零污染(--json/管道不受影响), stderr 非 TTY 时所有渲染自动静默
//!
use std::io::{IsTerminal, stderr};
use std::time::{Duration, Instant};

/// 重绘节流间隔
const REDRAW_INTERVAL: Duration = Duration::from_millis(100);
/// 进度条字符宽
const BAR_WIDTH: usize = 40;
/// 文件名后缀最大显示宽度(保留尾部: rollout 文件名的 uuid 在尾部)
const FILE_NAME_WIDTH: usize = 48;
/// 整行最大渲染宽度(超出截断, 右侧补空格擦除残留)
const LINE_WIDTH: usize = 128;

/// 扫描进度(字节驱动)
pub struct Progress {
    label: &'static str,
    file: String,
    total: u64,
    done: u64,
    last_render: Option<Instant>,
    tty: bool,
    finished: bool,
}

impl Progress {
    /// 创建进度(total_bytes 为本 app 待扫描总字节)
    pub fn start(label: &'static str, total_bytes: u64) -> Self {
        Self {
            label,
            file: String::new(),
            total: total_bytes,
            done: 0,
            last_render: None,
            tty: stderr().is_terminal(),
            finished: false,
        }
    }

    /// 切换当前处理文件并立即重绘
    pub fn set_file(&mut self, name: &str) {
        self.file = tail(name, FILE_NAME_WIDTH);
        self.draw();
    }

    /// 累计已消费字节数(内部节流重绘)
    pub fn add(&mut self, bytes: u64) {
        self.done += bytes;
        let throttled = self
            .last_render
            .is_some_and(|t| t.elapsed() < REDRAW_INTERVAL);
        if !throttled {
            self.draw();
        }
    }

    /// 完成收尾(补换行); 幂等
    pub fn finish(&mut self) {
        if !self.finished {
            self.finished = true;
            self.draw();
            if self.tty {
                eprintln!();
            }
        }
    }

    fn draw(&mut self) {
        if !self.tty || self.finished {
            return;
        }
        self.last_render = Some(Instant::now());
        let pct = if self.total > 0 {
            (self.done.min(self.total)) as f64 / self.total as f64 * 100.0
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
        let msg = format!(
            "{} [{head}{rest}] {:>5.1}% {:>9}/{:<9} {}",
            self.label,
            pct,
            fmt_bytes(self.done),
            fmt_bytes(self.total),
            self.file,
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
    unreachable!("units 末项已兜底返回")
}

/// 保留尾部的截断(文件名 uuid 在尾部, 头部时间戳可舍)
fn tail(s: &str, max_chars: usize) -> String {
    if s.chars().count() <= max_chars {
        s.to_string()
    } else {
        let skip = s.chars().count() - max_chars;
        s.chars().skip(skip).collect()
    }
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
        // 测试环境 stderr 非 TTY: 全流程静默, 只验证不 panic 与幂等
        let mut p = Progress::start("test", 1000);
        p.set_file("a.jsonl");
        p.add(400);
        p.add(600);
        p.finish();
        p.finish();
    }
}
