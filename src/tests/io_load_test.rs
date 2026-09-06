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
fn test_read_jsonl_skips_malformed_lines() {
    let path = temp_dir("jsonl").join("f.jsonl");
    fs::write(&path, "{\"a\":1}\nnot-json\n{\"a\":2}\n\n{\"a\":3}").unwrap();
    let rows = read_jsonl(&path).unwrap();
    assert_eq!(rows.len(), 3);
    assert_eq!(rows[2]["a"], 3);
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
