use super::*;
use std::fs;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

const TS: i64 = 1_788_256_800;

fn temp_dir() -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "tokrs-grok-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    fs::create_dir_all(&dir).unwrap();
    dir
}

fn write_updates(base: &Path, root: &str, session: &str, lines: &[String]) {
    let dir = base.join(root).join("enc-cwd").join(session);
    fs::create_dir_all(&dir).unwrap();
    let mut content = lines.join("\n");
    content.push('\n');
    fs::write(dir.join("updates.jsonl"), content).unwrap();
}

fn counters(input: u64, output: u64, cached: u64) -> String {
    format!(r#"{{"inputTokens":{input},"outputTokens":{output},"cachedReadTokens":{cached}}}"#)
}

fn model_usage(pairs: &[(&str, String)]) -> String {
    let inner = pairs
        .iter()
        .map(|(m, c)| format!(r#""{m}":{c}"#))
        .collect::<Vec<_>>()
        .join(",");
    format!(r#"{{"modelUsage":{{{inner}}}}}"#)
}

fn turn_line(ts: i64, prompt_id: Option<&str>, usage: &str) -> String {
    let pid = match prompt_id {
        Some(p) => format!(r#""prompt_id":"{p}","#),
        None => String::new(),
    };
    format!(
        r#"{{"timestamp":{ts},"method":"_x.ai/session/update","params":{{"update":{{"sessionUpdate":"turn_completed",{pid}"usage":{usage}}}}}}}"#
    )
}

#[test]
fn test_face_value_and_snapshot_filtering() {
    let base = temp_dir();
    write_updates(
        &base,
        "sessions",
        "sess-7",
        &[
            r#"{"method":"other","params":{"update":{}}}"#.to_string(),
            // 显式 usage_snapshot 带 usage: 不得导入(防中途快照双算)
            turn_line(TS, Some("px"), &model_usage(&[("m", counters(9999, 9, 0))]))
                .replace("turn_completed", "usage_snapshot"),
            turn_line(
                TS,
                Some("p1"),
                &model_usage(&[("grok-4.5-build", counters(100, 10, 5))]),
            ),
            // 下一轮面值与前一轮相同: 仍是两笔真实用量
            turn_line(
                TS + 60,
                Some("p2"),
                &model_usage(&[("grok-4.5-build", counters(100, 10, 5))]),
            ),
        ],
    );
    let entries = collect_from(&base).unwrap();
    assert_eq!(entries.len(), 2);
    for e in &entries {
        assert_eq!(e.model, "grok-4.5-build");
        assert_eq!(e.input_tokens, 100);
        assert_eq!(e.output_tokens, 10);
        assert_eq!(e.cache_read_tokens, 5);
        assert_eq!(e.cache_creation_tokens, 0);
        assert_eq!(e.session_id.as_deref(), Some("sess-7"));
    }
    fs::remove_dir_all(&base).ok();
}

#[test]
fn test_missing_session_update_and_prompt_id_accepted() {
    let base = temp_dir();
    // 缺 sessionUpdate 字段: 向后兼容放行; 缺 prompt_id: 用事件序号
    let line = format!(
        r#"{{"timestamp":{TS},"method":"_x.ai/session/update","params":{{"update":{{"usage":{}}}}}}}"#,
        counters(5, 1, 0)
    );
    write_updates(&base, "sessions", "s", &[line.clone(), line]);
    let entries = collect_from(&base).unwrap();
    // 两条同内容事件序号不同(idx0/idx1), 不互相吞并
    assert_eq!(entries.len(), 2);
    assert_eq!(entries[0].model, "unknown");
    assert_eq!(entries[0].created_at, TS);
    fs::remove_dir_all(&base).ok();
}

#[test]
fn test_multi_model_and_top_level_fallback() {
    let base = temp_dir();
    write_updates(
        &base,
        "sessions",
        "s",
        &[
            turn_line(
                TS,
                Some("p1"),
                &model_usage(&[
                    ("model-b", counters(2, 2, 0)),
                    ("model-a", counters(1, 1, 0)),
                ]),
            ),
            // 缺 modelUsage: 回退顶层 usage, 模型名 unknown
            turn_line(TS, Some("p2"), &counters(50, 5, 3)),
        ],
    );
    let mut entries = collect_from(&base).unwrap();
    entries.sort_by_key(|e| e.model.clone());
    assert_eq!(entries.len(), 3);
    assert_eq!(entries[0].model, "model-a");
    assert_eq!(entries[1].model, "model-b");
    assert_eq!(entries[2].model, "unknown");
    assert_eq!(entries[2].input_tokens, 50);
    assert_eq!(entries[2].cache_read_tokens, 3);
    fs::remove_dir_all(&base).ok();
}

#[test]
fn test_same_prompt_id_last_wins() {
    let base = temp_dir();
    write_updates(
        &base,
        "sessions",
        "s",
        &[
            turn_line(TS, Some("p1"), &model_usage(&[("m", counters(100, 10, 0))])),
            turn_line(TS, Some("p1"), &model_usage(&[("m", counters(200, 20, 0))])),
        ],
    );
    let entries = collect_from(&base).unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].input_tokens, 200);
    fs::remove_dir_all(&base).ok();
}

#[test]
fn test_zero_and_timestampless_skipped() {
    let base = temp_dir();
    let no_ts = format!(
        r#"{{"method":"_x.ai/session/update","params":{{"update":{{"sessionUpdate":"turn_completed","prompt_id":"p9","usage":{}}}}}}}"#,
        model_usage(&[("m", counters(77, 7, 0))])
    );
    write_updates(
        &base,
        "sessions",
        "s",
        &[
            turn_line(TS, Some("p1"), &model_usage(&[("m", counters(0, 0, 0))])),
            no_ts,
        ],
    );
    assert!(collect_from(&base).unwrap().is_empty());
    fs::remove_dir_all(&base).ok();
}

#[test]
fn test_archived_converges_and_other_files_ignored() {
    let base = temp_dir();
    let lines = vec![turn_line(
        TS,
        Some("p1"),
        &model_usage(&[("m", counters(9, 1, 0))]),
    )];
    write_updates(&base, "sessions", "s1", &lines);
    write_updates(&base, "archived_sessions", "s1", &lines);
    // 非 updates.jsonl 不采集
    let other = base
        .join("sessions")
        .join("enc-cwd")
        .join("s1")
        .join("summary.json");
    fs::write(&other, lines[0].clone()).unwrap();
    let entries = collect_from(&base).unwrap();
    // 同 session_id 归档副本与活跃副本经去重收敛
    assert_eq!(entries.len(), 1);
    fs::remove_dir_all(&base).ok();
}

#[test]
fn test_missing_base_returns_empty() {
    let base = temp_dir();
    fs::remove_dir_all(&base).unwrap();
    assert!(collect_from(&base).unwrap().is_empty());
}
