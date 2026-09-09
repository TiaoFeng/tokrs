pub mod claude;
pub mod codex;
pub mod dsh;
pub mod gemini;
pub mod grok;
pub mod kimi;
pub mod opencode;
pub mod pi;
pub mod prince;

use crate::error::AppError;
use crate::model::{AppKind, UsageEntry};
use serde_json::Value;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

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

/// 剥离模型名的 provider 前缀
///
/// 取最后一个 '/' 之后的段(moonshot-cn/kimi-k3 -> kimi-k3), 使同模型跨渠道合并计价;
/// 斜杠后为空或无斜杠时原样返回, 由 normalize_model 兜底 unknown
pub fn strip_provider(model: &str) -> &str {
    match model.rfind('/') {
        Some(pos) if pos + 1 < model.len() => &model[pos + 1..],
        _ => model,
    }
}

/// 模型名统一归一化(全 app 解析层唯一入口)
///
/// trim -> 剥 provider 前缀 -> ASCII 小写; 剥后为空或尾斜杠(段为空)兜底 "unknown",
/// 不让空值/垃圾名进入定价表; 不剥日期后缀(claude-sonnet-4-5-20250929 等日期变体
/// 保留原样, 由定价前缀边界匹配兼容); 未来日期别名/大小写兼容只改此处
pub fn normalize_model(raw: &str) -> String {
    let model = strip_provider(raw.trim()).to_ascii_lowercase();
    if model.is_empty() || model.ends_with('/') {
        "unknown".to_string()
    } else {
        model
    }
}

/// JSON 值内容哈希(去重键兜底, gemini/pi 共享, 原各解析器本地实现合并于此)
///
/// 哈希完整 JSON 值: 任何内容差异(响应文本/额外字段)都各自计数, 不折叠进固定
/// 键互相覆盖(漏计); 字节级相同的条目(如 fork 逐字节副本)仍去重.
/// serde_json 默认用 BTreeMap 存对象, 遍历与哈希顺序确定;
/// DefaultHasher::new() 固定 key(0,0), 单次运行内去重即足够
/// (本工具全量重扫无状态, 跨进程稳定性非必需), 故用 std 哈希不加 sha2 依赖
pub fn value_hash(value: &Value) -> u64 {
    let mut hasher = DefaultHasher::new();
    value.hash(&mut hasher);
    hasher.finish()
}

/// 汇总各 app 用量条目
///
/// threads: 每个文件型 app 内部的并行解析线程数(None = auto:
/// min(逻辑核数, 16, 文件数), 见 io/load::auto_threads); 跨 app 串行;
/// opencode 单 DB 查询无文件流, 忽略该参数
pub fn collect(apps: &[AppKind], threads: Option<usize>) -> Result<Vec<UsageEntry>, AppError> {
    let mut entries = Vec::new();
    for &app in apps {
        match app {
            AppKind::Claude => entries.extend(claude::collect(threads)?),
            AppKind::Codex => entries.extend(codex::collect(threads)?),
            AppKind::OpenCode => entries.extend(opencode::collect()?),
            AppKind::Gemini => entries.extend(gemini::collect(threads)?),
            AppKind::Grok => entries.extend(grok::collect(threads)?),
            AppKind::Pi => entries.extend(pi::collect(threads)?),
            AppKind::Kimi => entries.extend(kimi::collect(threads)?),
            AppKind::Dsh => entries.extend(dsh::collect(threads)?),
        }
    }
    Ok(entries)
}

#[cfg(test)]
mod tests {
    use super::{fresh_input, normalize_model, strip_provider};

    #[test]
    fn test_fresh_input() {
        assert_eq!(fresh_input(100, 50, 0), 50);
        assert_eq!(fresh_input(100, 50, 20), 30);
        assert_eq!(fresh_input(50, 0, 0), 50);
        assert_eq!(fresh_input(10, 20, 0), 10); // 上游数据不一致时保守不扣
    }

    #[test]
    fn test_strip_provider() {
        assert_eq!(strip_provider("moonshot-cn/kimi-k3"), "kimi-k3");
        assert_eq!(strip_provider("a/b/kimi-k3"), "kimi-k3"); // 多级取最后一段
        assert_eq!(strip_provider("kimi-k3"), "kimi-k3"); // 无斜杠原样
        assert_eq!(strip_provider("moonshot-cn/"), "moonshot-cn/"); // 斜杠后为空, 原样交给调用方兜底
    }

    #[test]
    fn test_normalize_model() {
        // trim + 小写 + 剥前缀, 日期后缀保留
        assert_eq!(
            normalize_model("  OpenAI/GPT-5.1-2025-01-01 "),
            "gpt-5.1-2025-01-01"
        );
        assert_eq!(normalize_model("moonshot-cn/kimi-k3"), "kimi-k3");
        assert_eq!(
            normalize_model("a/b/Claude-Sonnet-4-5"),
            "claude-sonnet-4-5"
        ); // 多级取最后一段
        assert_eq!(normalize_model("gpt-5"), "gpt-5"); // 无斜杠透传
        assert_eq!(
            normalize_model("claude-sonnet-4-5-20250929"),
            "claude-sonnet-4-5-20250929"
        );
        assert_eq!(normalize_model(""), "unknown"); // 空串兜底
        assert_eq!(normalize_model("   "), "unknown"); // 纯空白兜底
        assert_eq!(normalize_model("moonshot-cn/"), "unknown"); // 尾斜杠(段为空)兜底
    }
}
