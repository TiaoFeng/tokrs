//! commands 分组聚合的单元测试
//!
use super::*;
use chrono::{Local, TimeZone};

fn entry(app: AppKind, model: &str, created_at: i64) -> UsageEntry {
    UsageEntry::new(app, model.to_string(), None, created_at, 10, 20, 5, 3, None)
}

/// 本地正午时间戳(与 tokens_test 同款, 规避时区/DST 边界)
fn local_noon(year: i32, month: u32, day: u32) -> i64 {
    Local
        .with_ymd_and_hms(year, month, day, 12, 0, 0)
        .single()
        .unwrap()
        .timestamp()
}

#[test]
fn test_build_rows_by_app() {
    let entries = vec![
        entry(AppKind::Claude, "m", local_noon(2026, 9, 1)),
        entry(AppKind::Codex, "m", local_noon(2026, 9, 1)),
        entry(AppKind::Codex, "m", local_noon(2026, 9, 1)),
    ];
    let rows = build_rows(&entries, GroupBy::App);
    // BTreeMap 迭代有序: claude < codex
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].0, "claude");
    assert_eq!(rows[0].1.requests, 1);
    assert_eq!(rows[1].0, "codex");
    assert_eq!(rows[1].1.requests, 2);
}

#[test]
fn test_build_rows_by_model() {
    let entries = vec![
        entry(AppKind::Claude, "claude-sonnet-4", local_noon(2026, 9, 1)),
        entry(AppKind::Codex, "gpt-5", local_noon(2026, 9, 1)),
    ];
    let rows = build_rows(&entries, GroupBy::Model);
    // 键为 "{app}/{model}", BTreeMap 有序
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].0, "claude/claude-sonnet-4");
    assert_eq!(rows[1].0, "codex/gpt-5");
}

#[test]
fn test_build_rows_by_day() {
    let entries = vec![
        entry(AppKind::Claude, "m", local_noon(2026, 9, 1)),
        entry(AppKind::Claude, "m", local_noon(2026, 9, 1) + 3600),
        entry(AppKind::Claude, "m", local_noon(2026, 9, 3)),
    ];
    let rows = build_rows(&entries, GroupBy::Day);
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].0, "2026-09-01");
    assert_eq!(rows[0].1.requests, 2);
    assert_eq!(rows[1].0, "2026-09-03");
}

#[test]
fn test_resolve_threads() {
    // 缺省/0 → auto(None)
    assert_eq!(resolve_threads(None), None);
    assert_eq!(resolve_threads(Some(0)), None);
    // 合法范围直用
    assert_eq!(resolve_threads(Some(1)), Some(1));
    assert_eq!(resolve_threads(Some(16)), Some(16));
    // 超上限: 提示后收敛(意图明确的越界, 资源保护)
    assert_eq!(resolve_threads(Some(120000)), Some(16));
    // 负数/非数字由 clap 原生报错终止(见 resolve_threads 文档), 不进入本函数
}
