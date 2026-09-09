use super::*;
use std::fs;
use std::io::Cursor;

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

/// 参考实现(旧 windows 全位置 memcmp 版), 用于等价性对照
fn reference_contains(line: &[u8], needle: &[u8]) -> bool {
    !needle.is_empty() && line.windows(needle.len()).any(|w| w == needle)
}

#[test]
fn test_contains_needle_adversarial_cases() {
    // 结构化对抗语料: 命中/前缀/后缀/重叠模式/跨界部分命中/needle 长于行
    let corpus: Vec<Vec<u8>> = vec![
        b"".to_vec(),
        b"x".to_vec(),
        b"hit".to_vec(),
        b"hitx".to_vec(),
        b"xhit".to_vec(),
        b"abababab".to_vec(),
        b"aaaa".to_vec(),
        b"prefix-token_count-suffix".to_vec(),
        b"\"session_meta\"".to_vec(),
        b"line with \"token\" and \"count\" separately".to_vec(),
        b"\"token_coun".to_vec(),
        b"token_count".to_vec(),
    ];
    let needles: Vec<&[u8]> = vec![
        b"token_count",
        b"\"session_meta\"",
        b"\"turn_context\"",
        b"a",
        b"abab",
        b"aaaa",
        b"zzz",
        b"xhitx",
        b"tly",
    ];
    for line in &corpus {
        for needle in &needles {
            assert_eq!(
                contains_needle(line, needle),
                reference_contains(line, needle),
                "line={:?} needle={:?}",
                String::from_utf8_lossy(line),
                String::from_utf8_lossy(needle),
            );
        }
    }
}

#[test]
fn test_contains_needle_matches_reference_on_random_data() {
    // 确定性伪随机语料(xorshift 固定种子, 不引 rand 依赖):
    // 一半用例把 needle 嵌入随机位置保证命中路径覆盖, 两实现须逐例等价
    let mut state = 0x9E37_79B9_7F4A_7C15u64;
    let mut rnd = move || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        state
    };
    for case in 0..300u64 {
        let line_len = (rnd() % 48) as usize;
        let needle_len = 1 + (rnd() % 8) as usize;
        let line: Vec<u8> = (0..line_len).map(|_| (rnd() % 256) as u8).collect();
        let needle: Vec<u8> = (0..needle_len).map(|_| (rnd() % 256) as u8).collect();
        let line = if case % 2 == 0 && line_len >= needle_len {
            let pos = (rnd() as usize) % (line_len - needle_len + 1);
            let mut embedded = line[..pos].to_vec();
            embedded.extend_from_slice(&needle);
            embedded.extend_from_slice(&line[pos..]);
            embedded
        } else {
            line
        };
        assert_eq!(
            contains_needle(&line, &needle),
            reference_contains(&line, &needle),
            "case={case} line={line:?} needle={needle:?}"
        );
    }
}

#[test]
fn test_line_contains_filters_empty_needles() {
    // 空 needle 被过滤, 不触发 windows(0) 语义
    assert!(!line_contains(b"anything", &[""]));
    assert!(line_contains(b"has needle", &["", "needle"]));
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
fn test_progress_reader_forwards_content() {
    // ProgressReader: 透传内容并逐块向进度条上报字节(非 TTY 下 Progress 静默)
    let mut progress = Progress::start("test", 5);
    let mut reader = ProgressReader::new(Cursor::new("hello"), &mut progress);
    let mut out = String::new();
    reader.read_to_string(&mut out).unwrap();
    assert_eq!(out, "hello");
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
