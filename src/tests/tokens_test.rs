use super::*;

fn entry(app: AppKind, created_at: i64, input: u64, output: u64) -> UsageEntry {
    UsageEntry::new(
        app,
        "test-model".to_string(),
        None,
        created_at,
        input,
        output,
        0,
        0,
        None,
    )
}

fn local_noon(year: i32, month: u32, day: u32) -> i64 {
    Local
        .with_ymd_and_hms(year, month, day, 12, 0, 0)
        .single()
        .unwrap()
        .timestamp()
}

#[test]
fn test_aggregate_by_app_and_total() {
    let entries = vec![
        entry(AppKind::Claude, 1, 10, 5),
        entry(AppKind::Claude, 2, 20, 5),
        entry(AppKind::Codex, 3, 100, 50),
    ];
    let by_app = aggregate_by_app(&entries);
    assert_eq!(by_app.len(), 2);
    assert_eq!(by_app[&AppKind::Claude].requests, 2);
    assert_eq!(by_app[&AppKind::Claude].input_tokens, 30);
    assert_eq!(by_app[&AppKind::Codex].total_tokens(), 150);
    let total = grand_total(&entries);
    assert_eq!(total.requests, 3);
    assert_eq!(total.input_tokens, 130);
}

#[test]
fn test_aggregate_by_model() {
    let mut e1 = entry(AppKind::Claude, 1, 10, 5);
    e1.model = "claude-sonnet-4".to_string();
    let mut e2 = entry(AppKind::Claude, 2, 7, 3);
    e2.model = "gpt-5".to_string();
    let by_model = aggregate_by_model(&[e1.clone(), e2.clone(), e1.clone()]);
    assert_eq!(by_model.len(), 2);
    assert_eq!(
        by_model[&(AppKind::Claude, "claude-sonnet-4".to_string())].requests,
        2
    );
}

#[test]
fn test_aggregate_by_day_groups_same_local_day() {
    let ts = local_noon(2026, 9, 1);
    let entries = vec![
        entry(AppKind::Claude, ts, 1, 1),
        entry(AppKind::Claude, ts + 3600, 2, 2),
    ];
    let by_day = aggregate_by_day(&entries);
    assert_eq!(by_day.len(), 1);
    assert_eq!(by_day.values().next().unwrap().requests, 2);
}

#[test]
fn test_two_entries_days_apart_are_different_groups() {
    let entries = vec![
        entry(AppKind::Claude, local_noon(2026, 9, 1), 1, 1),
        entry(AppKind::Codex, local_noon(2026, 9, 3), 1, 1),
    ];
    assert_eq!(aggregate_by_day(&entries).len(), 2);
}

#[test]
fn test_filter_by_range() {
    let entries = vec![
        entry(AppKind::Claude, local_noon(2026, 9, 1), 1, 1),
        entry(AppKind::Codex, local_noon(2026, 9, 5), 2, 2),
    ];
    let since = NaiveDate::from_ymd_opt(2026, 9, 2).unwrap();
    let filtered = filter_by_range(entries.clone(), Some(since), None);
    assert_eq!(filtered.len(), 1);
    assert_eq!(filtered[0].app, AppKind::Codex);

    let until = NaiveDate::from_ymd_opt(2026, 9, 2).unwrap();
    let filtered = filter_by_range(entries, None, Some(until));
    assert_eq!(filtered.len(), 1);
    assert_eq!(filtered[0].app, AppKind::Claude);
}

#[test]
fn test_filter_by_range_noop() {
    let entries = vec![entry(AppKind::Claude, 1, 1, 1)];
    let filtered = filter_by_range(entries, None, None);
    assert_eq!(filtered.len(), 1);
}

#[test]
fn test_today_total_counts_only_today() {
    let today = Local::now().date_naive();
    let today_noon = today
        .and_hms_opt(12, 0, 0)
        .unwrap()
        .and_local_timezone(Local)
        .single()
        .unwrap()
        .timestamp();
    let old_noon = today_noon - 10 * 86_400;
    let entries = vec![
        entry(AppKind::Claude, today_noon, 10, 5),
        entry(AppKind::Codex, old_noon, 100, 50),
    ];
    let today = today_total(&entries);
    assert_eq!(today.requests, 1);
    assert_eq!(today.input_tokens, 10);
    assert_eq!(today.output_tokens, 5);
    assert_eq!(today_total(&[]), TokenTotals::default());
}

#[test]
fn test_totals_accumulate_cost_and_unpriced() {
    let mut priced = entry(AppKind::Claude, 1, 10, 5);
    priced.cost_usd = Some(0.25);
    let unpriced = entry(AppKind::Claude, 2, 10, 5);
    let total = grand_total(&[priced, unpriced]);
    assert_eq!(total.requests, 2);
    assert!((total.cost_usd - 0.25).abs() < f64::EPSILON);
    assert_eq!(total.unpriced, 1);
}
