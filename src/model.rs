//! 项目通用结构体与枚举
//!
//! 定义了AppKind(app类型)枚举,列举支持的app,经 clap::ValueEnum 直接驱动 --app 参数
//! 定义了UsageEntry结构体保存用户数据
//! 定义了TokenTotals结构体用于从用户请求数据统计Token的数量
//!
use clap::ValueEnum;
use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, ValueEnum)]
pub enum AppKind {
    Claude,
    Codex,
    #[value(name = "opencode")]
    OpenCode,
    Gemini,
    Grok,
    Pi,
    Kimi,
    Dsh,
}

impl AppKind {
    pub const ALL: [AppKind; 8] = [
        AppKind::Claude,
        AppKind::Codex,
        AppKind::OpenCode,
        AppKind::Gemini,
        AppKind::Grok,
        AppKind::Pi,
        AppKind::Kimi,
        AppKind::Dsh,
    ];

    pub fn as_str(&self) -> &'static str {
        match self {
            AppKind::Claude => "claude",
            AppKind::Codex => "codex",
            AppKind::OpenCode => "opencode",
            AppKind::Gemini => "gemini",
            AppKind::Grok => "grok",
            AppKind::Pi => "pi",
            AppKind::Kimi => "kimi",
            AppKind::Dsh => "dsh",
        }
    }
}

impl fmt::Display for AppKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct UsageEntry {
    pub app: AppKind,
    pub model: String,
    #[allow(dead_code)]
    pub session_id: Option<String>,
    pub created_at: i64,
    /// fresh input(与缓存无关的增量输入)
    ///
    /// codex/gemini/grok 上游 input 含缓存, 已在解析层扣除归一, 全 app 语义统一
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_tokens: u64,
    pub cache_creation_tokens: u64,
    /// 上游自报成本(USD), 仅当来源可信且 >0 时填充
    pub self_cost_usd: Option<f64>,
    /// 最终成本(USD): 自报优先, 否则由定价表估价, 均无则 None(计入 unpriced)
    pub cost_usd: Option<f64>,
}

impl UsageEntry {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        app: AppKind,
        model: String,
        session_id: Option<String>,
        created_at: i64,
        input_tokens: u64,
        output_tokens: u64,
        cache_read_tokens: u64,
        cache_creation_tokens: u64,
        self_cost_usd: Option<f64>,
    ) -> Self {
        Self {
            app,
            model,
            session_id,
            created_at,
            input_tokens,
            output_tokens,
            cache_read_tokens,
            cache_creation_tokens,
            self_cost_usd,
            cost_usd: None,
        }
    }

    #[cfg(test)]
    pub fn total_tokens(&self) -> u64 {
        self.input_tokens + self.output_tokens + self.cache_read_tokens + self.cache_creation_tokens
    }
}

#[derive(Debug, Default, Clone, Copy, PartialEq)]
pub struct TokenTotals {
    pub requests: u64,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_tokens: u64,
    pub cache_creation_tokens: u64,
    /// 已确定成本的请求成本合计(USD), 含自报与估价
    pub cost_usd: f64,
    /// 无任何成本来源的请求数, 成本合计不含这些请求
    pub unpriced: u64,
}

impl TokenTotals {
    pub fn add_entry(&mut self, entry: &UsageEntry) {
        self.requests += 1;
        self.input_tokens += entry.input_tokens;
        self.output_tokens += entry.output_tokens;
        self.cache_read_tokens += entry.cache_read_tokens;
        self.cache_creation_tokens += entry.cache_creation_tokens;
        match entry.cost_usd {
            Some(cost) => self.cost_usd += cost,
            None => self.unpriced += 1,
        }
    }

    pub fn total_tokens(&self) -> u64 {
        self.input_tokens + self.output_tokens + self.cache_read_tokens + self.cache_creation_tokens
    }
}
