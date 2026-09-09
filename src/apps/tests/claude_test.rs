use super::*;
use std::ffi::OsStr;
use std::fs;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

fn temp_dir() -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "tokrs-claude-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    fs::create_dir_all(dir.join("proj-hash")).unwrap();
    dir
}

fn write_session(base: &Path, name: &str, lines: &[String]) {
    let mut content = lines.join("\n");
    content.push('\n');
    fs::write(base.join("proj-hash").join(name), content).unwrap();
}

fn assistant_line(id: &str, output: u64, stop_reason: Option<&str>) -> String {
    let stop = match stop_reason {
        Some(s) => format!("\"stop_reason\":\"{s}\","),
        None => String::new(),
    };
    format!(
        r#"{{"type":"assistant","sessionId":"sess-1","timestamp":"2026-09-01T10:00:00Z","message":{{"id":"{id}","model":"claude-sonnet-4",{stop}"usage":{{"input_tokens":10,"output_tokens":{output},"cache_read_input_tokens":100,"cache_creation_input_tokens":20}}}}}}"#
    )
}

#[test]
fn test_dedup_by_message_id_prefers_stop_reason() {
    let base = temp_dir();
    write_session(
            &base,
            "session.jsonl",
            &[
                assistant_line("m1", 26, None),
                assistant_line("m1", 1349, Some("end_turn")),
                assistant_line("m2", 7, None),
                r#"{"type":"user","sessionId":"sess-1"}"#.to_string(),
                r#"{"type":"assistant","sessionId":"sess-1","message":{"id":"m3","usage":{"input_tokens":0,"output_tokens":0,"cache_read_input_tokens":0,"cache_creation_input_tokens":0}}}"#.to_string(),
            ],
        );
    let entries = collect_from(&base).unwrap();
    assert_eq!(entries.len(), 2);
    let m1 = entries
        .iter()
        .find(|e| e.model == "claude-sonnet-4" && e.output_tokens == 1349);
    assert!(m1.is_some(), "expected m1 winner with stop_reason");
    assert_eq!(m1.unwrap().total_tokens(), 10 + 1349 + 100 + 20);
    assert_eq!(m1.unwrap().session_id.as_deref(), Some("sess-1"));
    assert_eq!(m1.unwrap().created_at, 1_788_256_800);
    fs::remove_dir_all(&base).ok();
}

#[test]
fn test_same_stop_status_takes_larger_output() {
    let base = temp_dir();
    write_session(
        &base,
        "session.jsonl",
        &[
            assistant_line("m1", 1349, None),
            assistant_line("m1", 2000, None),
        ],
    );
    let entries = collect_from(&base).unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].output_tokens, 2000);
    fs::remove_dir_all(&base).ok();
}

#[test]
fn test_session_id_fallback_from_user_line() {
    let base = temp_dir();
    write_session(
            &base,
            "session.jsonl",
            &[
                r#"{"type":"user","sessionId":"sess-9"}"#.to_string(),
                r#"{"type":"assistant","timestamp":"2026-09-01T10:00:00Z","message":{"id":"m1","model":"m","usage":{"input_tokens":1,"output_tokens":1}}}"#.to_string(),
            ],
        );
    let entries = collect_from(&base).unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].session_id.as_deref(), Some("sess-9"));
    assert_eq!(entries[0].model, "m");
    fs::remove_dir_all(&base).ok();
}

#[test]
fn test_missing_base_returns_empty() {
    let base = temp_dir();
    fs::remove_dir_all(&base).unwrap();
    assert!(collect_from(&base).unwrap().is_empty());
}

#[test]
fn test_model_normalization() {
    let base = temp_dir();
    write_session(
        &base,
        "session.jsonl",
        &[r#"{"type":"assistant","sessionId":"sess-1","timestamp":"2026-09-01T10:00:00Z","message":{"id":"m1","model":"openrouter/anthropic/Claude-Sonnet-4-5","usage":{"input_tokens":1,"output_tokens":1}}}"#.to_string()],
    );
    let entries = collect_from(&base).unwrap();
    assert_eq!(entries.len(), 1);
    // 全 app 统一归一化: 多级前缀剥除 + 小写
    assert_eq!(entries[0].model, "claude-sonnet-4-5");
    fs::remove_dir_all(&base).ok();
}

#[test]
#[cfg(unix)]
fn test_unreadable_file_warns_and_continues() {
    use std::os::unix::fs::PermissionsExt;
    let base = temp_dir();
    write_session(
        &base,
        "session.jsonl",
        &[assistant_line("m1", 5, Some("end_turn"))],
    );
    let bad = base.join("proj-hash").join("broken.jsonl");
    fs::write(&bad, "{}").unwrap();
    let mut perms = fs::metadata(&bad).unwrap().permissions();
    perms.set_mode(0o000);
    fs::set_permissions(&bad, perms).unwrap();
    // 单文件不可读: 警告后跳过, 其余文件仍解析(依赖非 root 环境, CI 为非 root runner)
    let entries = collect_from(&base).unwrap();
    assert_eq!(entries.len(), 1);
    fs::remove_dir_all(&base).ok();
}

#[test]
fn test_claude_base_env_resolution() {
    let home = Path::new("/home/u");
    // 未设/空串 → 默认; ~/ 展开; 绝对直用
    assert_eq!(claude_base(home, None), PathBuf::from("/home/u/.claude"));
    assert_eq!(
        claude_base(home, Some(OsStr::new(""))),
        PathBuf::from("/home/u/.claude")
    );
    assert_eq!(
        claude_base(home, Some(OsStr::new("~/cd"))),
        PathBuf::from("/home/u/cd")
    );
    assert_eq!(
        claude_base(home, Some(OsStr::new("/abs/cd"))),
        PathBuf::from("/abs/cd")
    );
    // 非绝对路径: 警告后回退默认
    assert_eq!(
        claude_base(home, Some(OsStr::new("rel/cd"))),
        PathBuf::from("/home/u/.claude")
    );
}

/// 确定性排序(HashMap 输出序不定, 比较前排序)
fn sorted(mut entries: Vec<UsageEntry>) -> Vec<UsageEntry> {
    entries.sort_by(|x, y| {
        x.created_at
            .cmp(&y.created_at)
            .then(x.model.cmp(&y.model))
            .then(x.total_tokens().cmp(&y.total_tokens()))
            .then(x.input_tokens.cmp(&y.input_tokens))
            .then(x.output_tokens.cmp(&y.output_tokens))
    });
    entries
}

#[test]
fn test_parallel_scan_deterministic() {
    let base = temp_dir();
    // 同 id 跨文件: 跨文件合并规则与两条执行路径同时覆盖
    write_session(
        &base,
        "a.jsonl",
        &[
            assistant_line("m1", 10, None),
            assistant_line("m2", 20, Some("end_turn")),
        ],
    );
    write_session(
        &base,
        "b.jsonl",
        &[
            assistant_line("m1", 30, None),
            assistant_line("m3", 5, None),
        ],
    );
    let one = sorted(collect_from_with(&base, Some(1)).unwrap());
    let four = sorted(collect_from_with(&base, Some(4)).unwrap());
    assert_eq!(one, four);
    // 合并语义不因并行改变: 同 id 取 stop_reason 优先者, 共 3 个去重后条目
    assert_eq!(four.len(), 3);
    fs::remove_dir_all(&base).ok();
}
