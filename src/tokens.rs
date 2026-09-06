//! 项目核心函数
//!
//! 用于按照用户的要求分类并统计Token用量
//!
use chrono::{Local, NaiveDate, TimeZone};
use std::collections::BTreeMap;

use crate::model::{AppKind, TokenTotals, UsageEntry};

/// 将Unix时间戳转换为系统本地时间
///
/// 若转换失败则返回当前时间
pub fn local_date(created_at: i64) -> NaiveDate {
    Local
        .timestamp_opt(created_at, 0)
        .single()
        .map(|dt| dt.date_naive())
        .unwrap_or_else(|| Local::now().date_naive())
}

/// 筛选从Since到Until时间段里的用户请求
pub fn filter_by_range(
    entries: Vec<UsageEntry>,
    since: Option<NaiveDate>,
    until: Option<NaiveDate>,
) -> Vec<UsageEntry> {
    match (since, until) {
        (None, None) => entries,
        _ => entries
            .into_iter()
            .filter(|e| {
                let day = local_date(e.created_at);
                since.is_none_or(|s| day >= s) && until.is_none_or(|u| day <= u)
            })
            .collect(),
    }
}

/// 按照app从用户的请求统计出每个app的Token用量
pub fn aggregate_by_app(entries: &[UsageEntry]) -> BTreeMap<AppKind, TokenTotals> {
    let mut result: BTreeMap<AppKind, TokenTotals> = BTreeMap::new();
    for entry in entries {
        result.entry(entry.app).or_default().add_entry(entry);
    }
    result
}

/// 按照model从用户的请求统计出每个模型的Token用量
pub fn aggregate_by_model(entries: &[UsageEntry]) -> BTreeMap<(AppKind, String), TokenTotals> {
    let mut result: BTreeMap<(AppKind, String), TokenTotals> = BTreeMap::new();
    for entry in entries {
        result
            .entry((entry.app, entry.model.clone()))
            .or_default()
            .add_entry(entry);
    }
    result
}

/// 按照日期从用户的请求统计出每天的Token用量
pub fn aggregate_by_day(entries: &[UsageEntry]) -> BTreeMap<NaiveDate, TokenTotals> {
    let mut result: BTreeMap<NaiveDate, TokenTotals> = BTreeMap::new();
    for entry in entries {
        result
            .entry(local_date(entry.created_at))
            .or_default()
            .add_entry(entry);
    }
    result
}

/// 从用户的请求中统计所有的Token用量
pub fn grand_total(entries: &[UsageEntry]) -> TokenTotals {
    let mut total = TokenTotals::default();
    for entry in entries {
        total.add_entry(entry);
    }
    total
}

/// 统计今天当天的Token用量
pub fn today_total(entries: &[UsageEntry]) -> TokenTotals {
    let today = Local::now().date_naive();
    let mut total = TokenTotals::default();
    for entry in entries {
        if local_date(entry.created_at) == today {
            total.add_entry(entry);
        }
    }
    total
}

#[cfg(test)]
#[path = "tests/tokens_test.rs"]
mod tests;
