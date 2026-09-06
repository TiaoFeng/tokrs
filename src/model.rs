//! 项目通用结构体与枚举
//!
//! 定义了AppKind(app类型)枚举,列举支持的app
//! 定义了UsageEntry结构体保存用户数据
//! 定义了TokenTotals结构体用于从用户请求数据统计Token的数量
//!
use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum AppKind {
    Claude,
    Codex,
    OpenCode,
    Gemini,
    Grok,
    Pi,
}

impl AppKind {
    pub const ALL: [AppKind; 6] = [
        AppKind::Claude,
        AppKind::Codex,
        AppKind::OpenCode,
        AppKind::Gemini,
        AppKind::Grok,
        AppKind::Pi,
    ];

    pub fn as_str(&self) -> &'static str {
        match self {
            AppKind::Claude => "claude",
            AppKind::Codex => "codex",
            AppKind::OpenCode => "opencode",
            AppKind::Gemini => "gemini",
            AppKind::Grok => "grok",
            AppKind::Pi => "pi",
        }
    }
}

impl fmt::Display for AppKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl std::str::FromStr for AppKind {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_ascii_lowercase().as_str() {
            "claude" => Ok(AppKind::Claude),
            "codex" => Ok(AppKind::Codex),
            "opencode" => Ok(AppKind::OpenCode),
            "gemini" => Ok(AppKind::Gemini),
            "grok" => Ok(AppKind::Grok),
            "pi" => Ok(AppKind::Pi),
            other => Err(format!(
                "unknown app '{other}', expected one of: claude, codex, opencode, gemini, grok, pi"
            )),
        }
    }
}

#[derive(Debug, Clone)]
pub struct UsageEntry {
    pub app: AppKind,
    pub model: String,
    #[allow(dead_code)]
    pub session_id: Option<String>,
    pub created_at: i64,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_tokens: u64,
    pub cache_creation_tokens: u64,
}

impl UsageEntry {
    #[cfg(test)]
    pub fn total_tokens(&self) -> u64 {
        self.input_tokens + self.output_tokens + self.cache_read_tokens + self.cache_creation_tokens
    }
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct TokenTotals {
    pub requests: u64,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_tokens: u64,
    pub cache_creation_tokens: u64,
}

impl TokenTotals {
    pub fn add_entry(&mut self, entry: &UsageEntry) {
        self.requests += 1;
        self.input_tokens += entry.input_tokens;
        self.output_tokens += entry.output_tokens;
        self.cache_read_tokens += entry.cache_read_tokens;
        self.cache_creation_tokens += entry.cache_creation_tokens;
    }

    pub fn total_tokens(&self) -> u64 {
        self.input_tokens + self.output_tokens + self.cache_read_tokens + self.cache_creation_tokens
    }
}
