use super::*;
use crate::model::AppKind;
use chrono::{Local, TimeZone};
use std::time::{SystemTime, UNIX_EPOCH};

fn entry(model: &str, created_at: i64, input: u64, output: u64) -> UsageEntry {
    UsageEntry::new(
        AppKind::Claude,
        model.into(),
        None,
        created_at,
        input,
        output,
        0,
        0,
        None,
    )
}

fn table(json: &str) -> PricingFile {
    serde_json::from_str(json).unwrap()
}

fn local_ts(year: i32, month: u32, day: u32, hour: u32) -> i64 {
    Local
        .with_ymd_and_hms(year, month, day, hour, 0, 0)
        .single()
        .unwrap()
        .timestamp()
}

#[test]
fn test_estimate_basic_and_self_cost_priority() {
    let table = table(
        r#"{"version":1,"models":{"m":[{"input":3.0,"output":15.0,"cache_read":0.3,"cache_write":3.75}]}}"#,
    );
    let m = 1_000_000;
    let mut entries = vec![entry("m", local_ts(2026, 9, 1, 12), m, m)];
    entries[0].cache_read_tokens = m;
    entries[0].cache_creation_tokens = m;
    resolve(&mut entries, &table);
    // 3 + 15 + 0.3 + 3.75 (每百万 tokens)
    assert!((entries[0].cost_usd.unwrap() - 22.05).abs() < 1e-9);
    // 自报成本无条件优先
    let mut reported = entry("m", local_ts(2026, 9, 1, 12), m, m);
    reported.self_cost_usd = Some(0.01);
    resolve(std::slice::from_mut(&mut reported), &table);
    assert_eq!(reported.cost_usd, Some(0.01));
}

#[test]
fn test_null_base_is_unpriced_but_partial_null_zeroes() {
    let table = table(
        r#"{"version":1,"models":{"allnull":[{"input":null,"output":null,"cache_read":null,"cache_write":null}],"partial":[{"input":2.0,"output":null}]}}"#,
    );
    let mut entries = vec![
        entry("allnull", 0, 100, 100),
        entry("partial", 0, 500_000, 999_999_999),
    ];
    resolve(&mut entries, &table);
    assert_eq!(entries[0].cost_usd, None);
    // 未填字段按 0 计: 500k * 2.0 / 1M = 1.0
    assert!((entries[1].cost_usd.unwrap() - 1.0).abs() < 1e-9);
}

#[test]
fn test_longest_prefix_match_with_boundary() {
    let table =
        table(r#"{"version":1,"models":{"gpt-5":[{"input":1.0}],"gpt-5-codex":[{"input":2.0}]}}"#);
    let per_m = |model: &str| {
        let mut e = entry(model, 0, 1_000_000, 0);
        resolve(std::slice::from_mut(&mut e), &table);
        e.cost_usd
    };
    assert_eq!(per_m("gpt-5-codex"), Some(2.0));
    assert_eq!(per_m("gpt-5-codex-2026-01"), Some(2.0));
    assert_eq!(per_m("gpt-5.4"), Some(1.0));
    // "gpt-5" 前缀后紧跟字母数字 => 边界不成立, 不误配
    assert_eq!(per_m("gpt-51x"), None);
}

#[test]
fn test_version_selection_by_local_date() {
    let table = table(
        r#"{"version":1,"models":{"m":[{"since":"2026-01-01","input":1.0},{"since":"2026-03-15","input":2.0}]}}"#,
    );
    let cost_at = |ts: i64| {
        let mut e = entry("m", ts, 1_000_000, 0);
        resolve(std::slice::from_mut(&mut e), &table);
        e.cost_usd
    };
    assert_eq!(cost_at(local_ts(2026, 2, 1, 12)), Some(1.0));
    assert_eq!(cost_at(local_ts(2026, 3, 15, 0)), Some(2.0));
    assert_eq!(cost_at(local_ts(2026, 9, 1, 12)), Some(2.0));
    // 早于所有 since 版本 => 无生效价格
    assert_eq!(cost_at(local_ts(2025, 12, 31, 12)), None);
}

#[test]
fn test_peak_hours_multi_ranges_and_wrapping() {
    // 基础 1.0, 峰时(UTC 8-12, 14-18) 覆盖为 2.0
    let peak_table = table(
        r#"{"version":1,"models":{"m":[{"input":1.0,"peak":{"hours":[[8,12],[14,18]],"utc_offset":0,"input":2.0}}]}}"#,
    );
    let cost_at_utc_hour = |hour: u32| {
        let mut e = entry("m", i64::from(hour) * 3600, 1_000_000, 0);
        resolve(std::slice::from_mut(&mut e), &peak_table);
        e.cost_usd.unwrap()
    };
    assert_eq!(cost_at_utc_hour(9), 2.0);
    assert_eq!(cost_at_utc_hour(12), 1.0); // 区间左闭右开
    assert_eq!(cost_at_utc_hour(15), 2.0);
    assert_eq!(cost_at_utc_hour(20), 1.0);
    // 跨午夜回绕 [22,8) + utc_offset 8 时区换算
    let wrap_table = table(
        r#"{"version":1,"models":{"m":[{"input":1.0,"peak":{"hours":[[22,8]],"utc_offset":8,"input":2.0}}]}}"#,
    );
    let mut e = entry("m", 15 * 3600, 1_000_000, 0); // UTC 15 = 东八区 23 点
    resolve(std::slice::from_mut(&mut e), &wrap_table);
    assert_eq!(e.cost_usd.unwrap(), 2.0);
}

#[test]
fn test_long_context_threshold_and_field_inheritance() {
    let table = table(
        r#"{"version":1,"models":{"m":[{"input":1.0,"output":10.0,"cache_read":0.1,"long_context":{"above":200000,"input":6.0}}]}}"#,
    );
    // 上下文 = fresh input + cache_read + cache_creation = 100k + 150k = 250k >= 200k
    // 覆盖块只改 input 价, cache_read 继承基础价
    let mut big = UsageEntry::new(
        AppKind::Claude,
        "m".into(),
        None,
        0,
        100_000,
        0,
        150_000,
        0,
        None,
    );
    resolve(std::slice::from_mut(&mut big), &table);
    let expect = (100_000.0 * 6.0 + 150_000.0 * 0.1) / 1e6;
    assert!((big.cost_usd.unwrap() - expect).abs() < 1e-9);
    // 50k < 200k: 用基础价 1.0
    let mut small = entry("m", 0, 50_000, 0);
    resolve(std::slice::from_mut(&mut small), &table);
    assert!((small.cost_usd.unwrap() - 0.05).abs() < 1e-9);
}

#[test]
fn test_sync_models_appends_template_without_touching_existing() {
    let dir = std::env::temp_dir().join(format!(
        "tokrs-pricing-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("pricing.json");
    // 用户已填 m1 价格
    std::fs::write(
        &path,
        r#"{"version":1,"models":{"m1":[{"input":5.0,"output":null,"cache_read":null,"cache_write":null}]}}"#,
    )
    .unwrap();
    let mut table = load_pricing(&path).unwrap();
    let entries = vec![
        entry("m1", 0, 1, 1),
        entry("m2", 0, 1, 1),
        entry("m2", 1, 1, 1),
    ];
    assert_eq!(sync_models(&mut table, &path, &entries).unwrap(), 1);
    // 重新加载: 已有条目未被覆盖, 新模型带上可编辑的 null 模板
    let mut reloaded = load_pricing(&path).unwrap();
    assert_eq!(reloaded.models["m1"][0].fields.input, Some(5.0));
    assert!(reloaded.models["m2"][0].fields.is_empty());
    // 再次同步无变化, 不再写文件
    let mtime = std::fs::metadata(&path).unwrap().modified().unwrap();
    assert_eq!(sync_models(&mut reloaded, &path, &entries).unwrap(), 0);
    assert_eq!(std::fs::metadata(&path).unwrap().modified().unwrap(), mtime);
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn test_force_overrides_self_cost() {
    let forced_table = table(
        r#"{"version":1,"models":{"m":[{"force":true,"input":1.0,"output":0.0,"cache_read":0.0,"cache_write":0.0}]}}"#,
    );
    let empty_force = table(r#"{"version":1,"models":{"m":[{"force":true}]}}"#);
    let plain = table(r#"{"version":1,"models":{"m":[{"input":1.0}]}}"#);
    // force 且已填价: 无视自报 99, 按表计 1.0
    let mut forced = entry("m", 0, 1_000_000, 0);
    forced.self_cost_usd = Some(99.0);
    resolve(std::slice::from_mut(&mut forced), &forced_table);
    assert_eq!(forced.cost_usd, Some(1.0));
    // force 但基础价未填: 回退自报, 不吞数据
    let mut fallback = entry("m", 0, 1_000_000, 0);
    fallback.self_cost_usd = Some(99.0);
    resolve(std::slice::from_mut(&mut fallback), &empty_force);
    assert_eq!(fallback.cost_usd, Some(99.0));
    // 非 force: 自报照常优先
    let mut normal = entry("m", 0, 1_000_000, 0);
    normal.self_cost_usd = Some(99.0);
    resolve(std::slice::from_mut(&mut normal), &plain);
    assert_eq!(normal.cost_usd, Some(99.0));
}

#[test]
fn test_sync_skips_unknown_and_empty_models() {
    let dir = std::env::temp_dir().join(format!(
        "tokrs-pricing-unknown-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let path = dir.join("pricing.json");
    let mut table = PricingFile::default();
    let entries = vec![
        entry("unknown", 0, 1, 1),
        entry("", 0, 1, 1),
        entry("real-model", 0, 1, 1),
    ];
    assert_eq!(sync_models(&mut table, &path, &entries).unwrap(), 1);
    assert!(table.models.contains_key("real-model"));
    assert!(!table.models.contains_key("unknown"));
    assert!(!table.models.keys().any(|k| k.is_empty()));
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn test_load_missing_file_is_empty_but_corrupted_is_err() {
    let dir = std::env::temp_dir().join(format!(
        "tokrs-pricing-missing-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let missing = dir.join("pricing.json");
    assert_eq!(load_pricing(&missing).unwrap(), PricingFile::default());
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(&missing, "{ not json").unwrap();
    assert!(matches!(
        load_pricing(&missing),
        Err(AppError::Corrupted { .. })
    ));
    // 版本号不支持同样报错(损坏即终止, 由用户修复)
    std::fs::write(&missing, r#"{"version":99,"models":{}}"#).unwrap();
    assert!(matches!(
        load_pricing(&missing),
        Err(AppError::Corrupted { .. })
    ));
    std::fs::remove_dir_all(&dir).ok();
}
