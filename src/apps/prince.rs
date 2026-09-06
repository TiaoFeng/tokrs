//! prince定价模块
//!
//! 成本解析: 自报成本优先, 无自报则按 pricing.json 估价(Commit C 接入)
//!

use crate::model::UsageEntry;

/// 为每条 entry 解析最终成本
///
/// 当前仅透传自报成本; 定价表估价在后续接入
pub fn resolve(entries: &mut [UsageEntry]) {
    for entry in entries {
        entry.cost_usd = entry.self_cost_usd;
    }
}

#[cfg(test)]
mod tests {
    use super::resolve;
    use crate::model::{AppKind, UsageEntry};

    fn e(cost: Option<f64>) -> UsageEntry {
        UsageEntry::new(AppKind::Claude, "m".into(), None, 0, 1, 1, 0, 0, cost)
    }

    #[test]
    fn test_resolve_prefers_self_cost() {
        let mut entries = vec![e(Some(0.5)), e(None)];
        resolve(&mut entries);
        assert_eq!(entries[0].cost_usd, Some(0.5));
        assert_eq!(entries[1].cost_usd, None);
    }
}
