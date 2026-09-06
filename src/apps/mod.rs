pub mod claude;
pub mod codex;
pub mod gemini;
pub mod grok;
pub mod opencode;
pub mod pi;
pub mod prince;

use crate::error::AppError;
use crate::model::{AppKind, UsageEntry};

/// 上游 input 含缓存时归一为 fresh input
///
/// 对齐 cc-switch CACHE_INCLUSIVE_APP_TYPES(codex/gemini/grok): 这三家的
/// input 字段包含 cache read(及 codex 的 cache write), 直接相加会双算.
/// 数据不一致(input < read+write)时保守不扣, 避免把上游异常抹成负数.
pub fn fresh_input(input: u64, cache_read: u64, cache_creation: u64) -> u64 {
    input
        .checked_sub(cache_read.saturating_add(cache_creation))
        .unwrap_or(input)
}

pub fn collect(apps: &[AppKind]) -> Result<Vec<UsageEntry>, AppError> {
    let mut entries = Vec::new();
    for &app in apps {
        match app {
            AppKind::Claude => entries.extend(claude::collect()?),
            AppKind::Codex => entries.extend(codex::collect()?),
            AppKind::OpenCode => entries.extend(opencode::collect()?),
            AppKind::Gemini => entries.extend(gemini::collect()?),
            AppKind::Grok => entries.extend(grok::collect()?),
            AppKind::Pi => entries.extend(pi::collect()?),
        }
    }
    Ok(entries)
}

#[cfg(test)]
mod tests {
    use super::fresh_input;

    #[test]
    fn test_fresh_input() {
        assert_eq!(fresh_input(100, 50, 0), 50);
        assert_eq!(fresh_input(100, 50, 20), 30);
        assert_eq!(fresh_input(50, 0, 0), 50);
        assert_eq!(fresh_input(10, 20, 0), 10); // 上游数据不一致时保守不扣
    }
}
