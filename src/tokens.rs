use std::collections::BTreeMap;

use chrono::{Local, NaiveDate, TimeZone};

use crate::model::{AppKind, TokenTotals, UsageEntry};

pub fn local_date(created_at: i64) -> NaiveDate {
    Local
        .timestamp_opt(created_at, 0)
        .single()
        .map(|dt| dt.date_naive())
        .unwrap_or_else(|| Local::now().date_naive())
}

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

pub fn aggregate_by_app(entries: &[UsageEntry]) -> BTreeMap<AppKind, TokenTotals> {
    let mut result: BTreeMap<AppKind, TokenTotals> = BTreeMap::new();
    for entry in entries {
        result.entry(entry.app).or_default().add_entry(entry);
    }
    result
}

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

pub fn grand_total(entries: &[UsageEntry]) -> TokenTotals {
    let mut total = TokenTotals::default();
    for entry in entries {
        total.add_entry(entry);
    }
    total
}

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
