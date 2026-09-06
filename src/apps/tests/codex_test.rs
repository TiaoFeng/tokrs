use super::*;
use std::fs;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

const THREAD_ID: &str = "11111111-2222-3333-4444-555555555555";

fn temp_base() -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "tokrs-codex-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    fs::create_dir_all(dir.join("sessions/2026/09/04")).unwrap();
    fs::create_dir_all(dir.join("archived_sessions")).unwrap();
    dir
}

fn write_rollout(base: &Path, rel: &str, lines: &[String]) {
    let mut content = lines.join("\n");
    content.push('\n');
    fs::write(base.join(rel), content).unwrap();
}

fn meta_line() -> String {
    format!(
        r#"{{"timestamp":"2026-09-04T05:58:05.965Z","type":"session_meta","payload":{{"id":"{THREAD_ID}","cwd":"/tmp/x"}}}}"#
    )
}

fn turn_context_line(model: &str) -> String {
    format!(
        r#"{{"timestamp":"2026-09-04T05:58:06Z","type":"turn_context","payload":{{"model":"{model}"}}}}"#
    )
}

fn token_count_line(
    ts: &str,
    total: Option<(u64, u64, u64, u64)>,
    last: Option<(u64, u64, u64, u64)>,
    model: Option<&str>,
) -> String {
    let usage = |t: (u64, u64, u64, u64)| {
        format!(
            r#"{{"input_tokens":{},"cached_input_tokens":{},"cache_write_input_tokens":{},"output_tokens":{},"reasoning_output_tokens":0,"total_tokens":{}}}"#,
            t.0,
            t.1,
            t.2,
            t.3,
            t.0 + t.3
        )
    };
    let mut info = String::from("{");
    if let Some(t) = total {
        info.push_str(&format!(r#""total_token_usage":{},"#, usage(t)));
    }
    info.push_str(r#""rate_limits":{"limit_id":"codex"}"#);
    if let Some(t) = last {
        info.push_str(&format!(r#","last_token_usage":{}"#, usage(t)));
    }
    if let Some(m) = model {
        info.push_str(&format!(r#","model":"{m}""#));
    }
    info.push('}');
    format!(
        r#"{{"timestamp":"{ts}","type":"event_msg","payload":{{"type":"token_count","info":{info}}}}}"#
    )
}

fn collect_at(base: &Path) -> Vec<UsageEntry> {
    collect_from(base).unwrap()
}

#[test]
fn test_last_token_usage_wins_and_duplicates_skipped() {
    let base = temp_base();
    write_rollout(
        &base,
        "sessions/2026/09/04/rollout-2026-09-04T13-58-05-01a06afe-e959-7c23-b5ec-30b29cd3bd35.jsonl",
        &[
            meta_line(),
            turn_context_line("GPT-5-Codex"),
            token_count_line("2026-09-04T05:58:07Z", Some((100, 50, 0, 10)), None, None),
            token_count_line(
                "2026-09-04T05:59:00Z",
                Some((300, 150, 0, 30)),
                Some((200, 100, 0, 20)),
                None,
            ),
            token_count_line(
                "2026-09-04T05:59:30Z",
                Some((300, 150, 0, 30)),
                Some((200, 100, 0, 20)),
                None,
            ),
        ],
    );
    let entries = collect_at(&base);
    assert_eq!(entries.len(), 2);
    // input 已归一为 fresh: 100-50=50, 200-100=100
    assert_eq!(
        (
            entries[0].input_tokens,
            entries[0].cache_read_tokens,
            entries[0].output_tokens
        ),
        (50, 50, 10)
    );
    assert_eq!(
        (
            entries[1].input_tokens,
            entries[1].cache_read_tokens,
            entries[1].output_tokens
        ),
        (100, 100, 20)
    );
    assert_eq!(entries[0].model, "gpt-5-codex");
    assert_eq!(entries[0].session_id.as_deref(), Some(THREAD_ID));
    assert_eq!(entries[0].created_at, 1_788_501_487);
}

#[test]
fn test_total_high_water_delta() {
    let base = temp_base();
    write_rollout(
        &base,
        "sessions/2026/09/04/rollout-00000000-2222-3333-4444-555555555555.jsonl",
        &[
            token_count_line("2026-09-04T06:00:00Z", Some((100, 50, 0, 10)), None, None),
            token_count_line("2026-09-04T06:01:00Z", Some((150, 60, 0, 20)), None, None),
        ],
    );
    let entries = collect_at(&base);
    assert_eq!(entries.len(), 2);
    // d=[50,10,0,10] -> fresh input = 50-10 = 40
    assert_eq!(
        (
            entries[1].input_tokens,
            entries[1].cache_read_tokens,
            entries[1].output_tokens
        ),
        (40, 10, 10)
    );
}

#[test]
fn test_cached_clamped_to_input() {
    let base = temp_base();
    write_rollout(
        &base,
        "sessions/2026/09/04/rollout-00000000-2222-3333-4444-555555555555.jsonl",
        &[token_count_line(
            "2026-09-04T06:00:00Z",
            None,
            Some((10, 20, 0, 5)),
            None,
        )],
    );
    let entries = collect_at(&base);
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].cache_read_tokens, 10);
    // cached 被 clamp 到 input(10) 后, fresh input 归零但缓存命中仍保留
    assert_eq!(entries[0].input_tokens, 0);
}

#[test]
fn test_empty_last_object_yields_no_entry() {
    let base = temp_base();
    write_rollout(
            &base,
            "sessions/2026/09/04/rollout-00000000-2222-3333-4444-555555555555.jsonl",
            &[r#"{"timestamp":"2026-09-04T06:00:00Z","type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":100,"cached_input_tokens":50,"output_tokens":10},"last_token_usage":{}}}}"#.to_string()],
        );
    assert!(collect_at(&base).is_empty());
}

#[test]
fn test_info_model_overrides_turn_context_with_normalization() {
    let base = temp_base();
    write_rollout(
        &base,
        "sessions/2026/09/04/rollout-00000000-2222-3333-4444-555555555555.jsonl",
        &[
            turn_context_line("FooProvider/GPT-5.4-2026-01-01"),
            token_count_line(
                "2026-09-04T06:00:00Z",
                None,
                Some((10, 0, 0, 5)),
                Some("Bar/qwen3-20250515"),
            ),
            token_count_line("2026-09-04T06:01:00Z", None, Some((11, 0, 0, 5)), None),
        ],
    );
    let entries = collect_at(&base);
    assert_eq!(entries.len(), 2);
    assert_eq!(entries[0].model, "qwen3");
    assert_eq!(entries[1].model, "gpt-5.4");
}

#[test]
fn test_replayed_events_deduped_across_files() {
    let base = temp_base();
    let parent_events = vec![
        meta_line(),
        token_count_line("2026-09-04T05:58:07Z", Some((100, 50, 0, 10)), None, None),
    ];
    write_rollout(
        &base,
        "sessions/2026/09/04/rollout-2026-09-04T13-58-05-11111111-2222-3333-4444-555555555555.jsonl",
        &parent_events,
    );
    write_rollout(
        &base,
        "sessions/2026/09/04/rollout-2026-09-04T14-00-00-99999999-2222-3333-4444-555555555555.jsonl",
        &[
            parent_events[1].clone(),
            token_count_line(
                "2026-09-04T06:00:00Z",
                Some((150, 60, 0, 20)),
                Some((50, 10, 0, 10)),
                None,
            ),
        ],
    );
    let entries = collect_at(&base);
    assert_eq!(entries.len(), 2);
    // 均为 fresh: 100-50=50, last 50-10=40
    assert_eq!(entries[0].input_tokens, 50);
    assert_eq!(entries[1].input_tokens, 40);
}

#[test]
fn test_archived_sessions_collected() {
    let base = temp_base();
    write_rollout(
        &base,
        "archived_sessions/rollout-2026-08-29T20-12-53-01a04d6f-e247-7d72-b6f5-46b6f4fa8269.jsonl",
        &[token_count_line(
            "2026-08-29T12:00:00Z",
            None,
            Some((10, 2, 0, 3)),
            None,
        )],
    );
    write_rollout(
        &base,
        "archived_sessions/not-a-rollout.jsonl",
        &[token_count_line(
            "2026-08-29T12:00:00Z",
            None,
            Some((99, 2, 0, 3)),
            None,
        )],
    );
    let entries = collect_at(&base);
    assert_eq!(entries.len(), 1);
    // fresh: 10-2=8
    assert_eq!(entries[0].input_tokens, 8);
}

#[test]
fn test_cache_write_parsed_and_delta_tracked() {
    let base = temp_base();
    write_rollout(
        &base,
        "sessions/2026/09/04/rollout-00000000-2222-3333-4444-555555555555.jsonl",
        &[
            token_count_line("2026-09-04T06:00:00Z", Some((100, 10, 5, 20)), None, None),
            token_count_line("2026-09-04T06:01:00Z", Some((150, 10, 8, 30)), None, None),
            token_count_line("2026-09-04T06:02:00Z", None, Some((10, 0, 4, 5)), None),
        ],
    );
    let entries = collect_at(&base);
    assert_eq!(entries.len(), 3);
    assert_eq!(entries[0].cache_creation_tokens, 5);
    // fresh: 100-10-5=85, d=[50,0,3,10]->47, last [10,0,4,5]->6
    assert_eq!(entries[0].input_tokens, 85);
    assert_eq!(entries[1].input_tokens, 47);
    assert_eq!(entries[1].cache_creation_tokens, 3);
    assert_eq!(entries[2].cache_creation_tokens, 4);
    assert_eq!(entries[2].cache_read_tokens, 0);
}

#[test]
fn test_normalize_model() {
    assert_eq!(normalize_model("Foo/GPT-5.4-2026-01-01"), "gpt-5.4");
    assert_eq!(normalize_model("qwen3-20250515"), "qwen3");
    assert_eq!(normalize_model("deepseek-v3.2"), "deepseek-v3.2");
    assert_eq!(normalize_model(" GPT-5 "), "gpt-5");
}

#[test]
fn test_is_rollout_filename() {
    let good =
        Path::new("/x/rollout-2026-09-04T13-58-05-01a06afe-e959-7c23-b5ec-30b29cd3bd35.jsonl");
    assert!(is_rollout_filename(good));
    let bad = Path::new("/x/session.jsonl");
    assert!(!is_rollout_filename(bad));
}
