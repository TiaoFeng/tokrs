use super::*;
use std::fs;

fn temp_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "tokrs-load-{name}-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn test_discover_files_respects_extension_and_depth() {
    let base = temp_dir("discover");
    fs::create_dir_all(base.join("a/b")).unwrap();
    fs::write(base.join("root.jsonl"), "{}").unwrap();
    fs::write(base.join("root.txt"), "{}").unwrap();
    fs::write(base.join("a/mid.jsonl"), "{}").unwrap();
    fs::write(base.join("a/b/deep.jsonl"), "{}").unwrap();

    let found = discover_files(&base, "jsonl", 1);
    let names: Vec<String> = found
        .iter()
        .map(|p| p.strip_prefix(&base).unwrap().to_string_lossy().to_string())
        .collect();
    assert_eq!(names, vec!["a/mid.jsonl", "root.jsonl"]);
    fs::remove_dir_all(&base).ok();
}

#[test]
fn test_discover_files_missing_base_is_empty() {
    let base = temp_dir("missing");
    fs::remove_dir_all(&base).ok();
    assert!(discover_files(&base, "jsonl", 3).is_empty());
}

#[test]
fn test_for_each_jsonl_skips_malformed_lines() {
    let path = temp_dir("jsonl").join("f.jsonl");
    // 畸形行/空行跳过; 末行无换行符仍解析; CRLF 行容忍尾随 \r
    fs::write(
        &path,
        "{\"a\":1}\nnot-json\n{\"a\":2}\n\n{\"a\":3}\r\n{\"a\":4}",
    )
    .unwrap();
    let mut rows = Vec::new();
    for_each_jsonl_impl(&path, &[], MAX_LINE_BYTES, None, |v| {
        rows.push(v);
        true
    })
    .unwrap();
    assert_eq!(rows.len(), 4);
    assert_eq!(rows[0]["a"], 1);
    assert_eq!(rows[2]["a"], 3);
    assert_eq!(rows[3]["a"], 4);
}

#[test]
fn test_for_each_jsonl_early_stop_and_needles() {
    let path = temp_dir("needles").join("f.jsonl");
    fs::write(
        &path,
        "{\"t\":\"x\"}\n{\"k\":\"hit\"}\n{\"t\":\"y\"}\n{\"k\":\"hit2\"}",
    )
    .unwrap();
    // needle 预过滤: 不含字面量的行零解析跳过("hit2" 不含 "hit"——缺收尾引号)
    let mut hits = Vec::new();
    for_each_jsonl_impl(&path, &["\"hit\""], MAX_LINE_BYTES, None, |v| {
        hits.push(v);
        true
    })
    .unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0]["k"], "hit");
    // 回调返回 false 提前终止
    let mut stopped = Vec::new();
    for_each_jsonl_impl(&path, &[], MAX_LINE_BYTES, None, |v| {
        stopped.push(v);
        false
    })
    .unwrap();
    assert_eq!(stopped.len(), 1);
}

#[test]
fn test_for_each_jsonl_oversized_line_skipped() {
    let path = temp_dir("oversize").join("f.jsonl");
    // 行上限参数化: 超限行整行跳过(丢弃直到换行), 后续合法行正常解析
    let big = format!("\"{}\"", "x".repeat(300));
    fs::write(&path, format!("{big}\n{{\"ok\":1}}\n")).unwrap();
    let mut rows = Vec::new();
    for_each_jsonl_impl(&path, &[], 128, None, |v| {
        rows.push(v);
        true
    })
    .unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["ok"], 1);
}

#[test]
#[cfg(unix)]
fn test_discover_files_skips_fifo() {
    let base = temp_dir("fifo");
    fs::write(base.join("real.jsonl"), "{}").unwrap();
    let ok = std::process::Command::new("mkfifo")
        .arg(base.join("pipe.jsonl"))
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    if !ok {
        return; // 环境无 mkfifo 时跳过验证
    }
    // FIFO/socket/设备不被收集, 防读取永久阻塞(真卡死)
    let found = discover_files(&base, "jsonl", 1);
    assert_eq!(found, vec![base.join("real.jsonl")]);
    fs::remove_dir_all(&base).ok();
}

#[test]
fn test_read_json_ok_and_corrupted() {
    let dir = temp_dir("readjson");
    let ok = dir.join("a.json");
    fs::write(&ok, "{\"x\":1}").unwrap();
    assert_eq!(read_json(&ok).unwrap()["x"], 1);
    let bad = dir.join("b.json");
    fs::write(&bad, "{oops").unwrap();
    assert!(matches!(read_json(&bad), Err(AppError::Corrupted { .. })));
}

#[test]
fn test_u64_and_str_get() {
    let v: Value = serde_json::from_str(r#"{"a":{"b":42},"s":{"t":"x"},"f":1.7}"#).unwrap();
    assert_eq!(u64_get(&v, &["a", "b"]), 42);
    assert_eq!(u64_get(&v, &["f"]), 1);
    assert_eq!(u64_get(&v, &["nope"]), 0);
    assert_eq!(str_get(&v, &["s", "t"]), Some("x"));
    assert_eq!(str_get(&v, &["a", "t"]), None);
}

#[test]
fn test_cost_get() {
    let v: Value =
        serde_json::from_str(r#"{"a":{"b":0.5},"z":{"b":0},"n":{"b":-1},"s":{"b":"x"}}"#).unwrap();
    assert_eq!(cost_get(&v, &["a", "b"]), Some(0.5));
    assert_eq!(cost_get(&v, &["z", "b"]), None);
    assert_eq!(cost_get(&v, &["n", "b"]), None);
    assert_eq!(cost_get(&v, &["s", "b"]), None);
    assert_eq!(cost_get(&v, &["nope"]), None);
}

#[test]
fn test_bool_get() {
    let v: Value = serde_json::from_str(r#"{"t":{"b":true},"x":{"b":1}}"#).unwrap();
    assert!(bool_get(&v, &["t", "b"]));
    assert!(!bool_get(&v, &["x", "b"]));
    assert!(!bool_get(&v, &["nope"]));
}

#[test]
fn test_timestamp_to_epoch() {
    let v: Value = serde_json::from_str(
        "[1767000000, 1767000000000, \"1767000000\", \"2026-09-01T00:00:00Z\"]",
    )
    .unwrap();
    let arr = v.as_array().unwrap();
    assert_eq!(timestamp_to_epoch(&arr[0]), Some(1_767_000_000));
    assert_eq!(timestamp_to_epoch(&arr[1]), Some(1_767_000_000));
    assert_eq!(timestamp_to_epoch(&arr[2]), Some(1_767_000_000));
    assert_eq!(timestamp_to_epoch(&arr[3]), Some(1_788_220_800));
}
