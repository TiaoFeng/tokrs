use super::*;
use std::fs;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

const THREAD_ID: &str = "11111111-2222-3333-4444-555555555555";
const CHILD_ID: &str = "99999999-8888-7777-6666-555555555555";
const OTHER_ID: &str = "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee";

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
    meta_line_for(THREAD_ID)
}

fn meta_line_for(id: &str) -> String {
    format!(
        r#"{{"timestamp":"2026-09-04T05:58:05.965Z","type":"session_meta","payload":{{"id":"{id}","cwd":"/tmp/x"}}}}"#
    )
}

fn fork_meta_line(id: &str, parent: &str, ts: &str) -> String {
    format!(
        r#"{{"timestamp":"{ts}","type":"session_meta","payload":{{"id":"{id}","forked_from_id":"{parent}","cwd":"/tmp/x"}}}}"#
    )
}

fn turn_context_line(model: &str) -> String {
    format!(
        r#"{{"timestamp":"2026-09-04T05:58:06Z","type":"turn_context","payload":{{"model":"{model}"}}}}"#
    )
}

fn token_count_line_src(
    ts: &str,
    total: Option<(u64, u64, u64, u64)>,
    last: Option<(u64, u64, u64, u64)>,
    model: Option<&str>,
    limit_id: &str,
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
    info.push_str(&format!(r#""rate_limits":{{"limit_id":"{limit_id}"}}"#));
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

fn token_count_line(
    ts: &str,
    total: Option<(u64, u64, u64, u64)>,
    last: Option<(u64, u64, u64, u64)>,
    model: Option<&str>,
) -> String {
    token_count_line_src(ts, total, last, model, "codex")
}

fn collect_at(base: &Path) -> Vec<UsageEntry> {
    collect_from(base).unwrap()
}

#[test]
fn test_last_token_usage_wins_and_duplicates_skipped() {
    let base = temp_base();
    write_rollout(
        &base,
        "sessions/2026/09/04/rollout-2026-09-04T13-58-05-11111111-2222-3333-4444-555555555555.jsonl",
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
        "sessions/2026/09/04/rollout-11111111-2222-3333-4444-555555555555.jsonl",
        &[
            meta_line(),
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
fn test_total_high_water_advances_on_last_events() {
    let base = temp_base();
    write_rollout(
        &base,
        "sessions/2026/09/04/rollout-11111111-2222-3333-4444-555555555555.jsonl",
        &[
            meta_line(),
            token_count_line(
                "2026-09-04T06:00:00Z",
                Some((100, 50, 0, 10)),
                Some((100, 50, 0, 10)),
                None,
            ),
            // last 缺失回退 total 差分: 高水位已被上一条 total 推进到 [300,150,0,30],
            // 差分 [200,100,0,20] 而非全量 300(修复高水位只在 last 缺失分支推进的超计)
            token_count_line("2026-09-04T06:01:00Z", Some((300, 150, 0, 30)), None, None),
        ],
    );
    let entries = collect_at(&base);
    assert_eq!(entries.len(), 2);
    assert_eq!(entries[0].input_tokens, 50); // last 面值: 100-50
    assert_eq!(entries[1].input_tokens, 100); // total 差分: 200-100
}

#[test]
fn test_cached_clamped_to_input() {
    let base = temp_base();
    write_rollout(
        &base,
        "sessions/2026/09/04/rollout-11111111-2222-3333-4444-555555555555.jsonl",
        &[
            meta_line(),
            token_count_line("2026-09-04T06:00:00Z", None, Some((10, 20, 0, 5)), None),
        ],
    );
    let entries = collect_at(&base);
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].cache_read_tokens, 10);
    // cached 被 clamp 到 input(10) 后, fresh input 归零但缓存命中仍保留
    assert_eq!(entries[0].input_tokens, 0);
}

#[test]
fn test_empty_last_falls_back_to_total_delta() {
    let base = temp_base();
    write_rollout(
        &base,
        "sessions/2026/09/04/rollout-11111111-2222-3333-4444-555555555555.jsonl",
        &[meta_line(), r#"{"timestamp":"2026-09-04T06:00:00Z","type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":100,"cached_input_tokens":50,"output_tokens":10},"last_token_usage":{}}}}"#.to_string()],
    );
    // 空 last 对象不是有效快照(对齐 cc-switch), 视为缺失回退 total 差分
    let entries = collect_at(&base);
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].input_tokens, 50);
    assert_eq!(entries[0].cache_read_tokens, 50);
}

#[test]
fn test_info_model_persists_and_turn_context_fallback() {
    let base = temp_base();
    write_rollout(
        &base,
        "sessions/2026/09/04/rollout-11111111-2222-3333-4444-555555555555.jsonl",
        &[
            meta_line(),
            turn_context_line("FooProvider/GPT-5.4-2026-01-01"),
            token_count_line("2026-09-04T06:00:00Z", None, Some((10, 0, 0, 5)), None),
            token_count_line(
                "2026-09-04T06:01:00Z",
                None,
                Some((11, 0, 0, 5)),
                Some("Bar/qwen3-20250515"),
            ),
            token_count_line("2026-09-04T06:02:00Z", None, Some((12, 0, 0, 5)), None),
        ],
    );
    let entries = collect_at(&base);
    assert_eq!(entries.len(), 3);
    // 全 app 统一归一化(剥前缀/小写), 日期后缀保留
    assert_eq!(entries[0].model, "gpt-5.4-2026-01-01");
    assert_eq!(entries[1].model, "qwen3-20250515");
    // info.model 持久化(对齐 cc-switch): 后续无 model 事件沿用最近一次
    assert_eq!(entries[2].model, "qwen3-20250515");
}

#[test]
fn test_cache_write_parsed_and_delta_tracked() {
    let base = temp_base();
    write_rollout(
        &base,
        "sessions/2026/09/04/rollout-11111111-2222-3333-4444-555555555555.jsonl",
        &[
            meta_line(),
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
fn test_identical_usage_in_different_sessions_both_counted() {
    let base = temp_base();
    let ev = token_count_line("2026-09-04T06:00:00Z", Some((100, 50, 0, 10)), None, None);
    write_rollout(
        &base,
        "sessions/2026/09/04/rollout-2026-09-04T13-58-05-11111111-2222-3333-4444-555555555555.jsonl",
        &[meta_line_for(THREAD_ID), ev.clone()],
    );
    write_rollout(
        &base,
        "sessions/2026/09/04/rollout-2026-09-04T14-00-00-aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee.jsonl",
        &[meta_line_for(OTHER_ID), ev],
    );
    let entries = collect_at(&base);
    // 不同会话首轮用量雷同不误吞(修复全局签名去重的跨会话漏计)
    assert_eq!(entries.len(), 2);
    assert!(
        entries
            .iter()
            .all(|e| e.input_tokens == 50 && e.cache_read_tokens == 50)
    );
    assert_eq!(entries[0].session_id.as_deref(), Some(THREAD_ID));
    assert_eq!(entries[1].session_id.as_deref(), Some(OTHER_ID));
}

#[test]
fn test_fork_replayed_events_skipped() {
    let base = temp_base();
    // 父在 fork 后继续写入(:11), 保证父最大时间戳 >= 子 meta 时刻
    write_rollout(
        &base,
        "sessions/2026/09/04/rollout-2026-09-04T13-58-05-11111111-2222-3333-4444-555555555555.jsonl",
        &[
            meta_line(),
            token_count_line("2026-09-04T05:58:07Z", Some((100, 50, 0, 10)), None, None),
            token_count_line("2026-09-04T05:58:11Z", Some((200, 100, 0, 20)), None, None),
        ],
    );
    write_rollout(
        &base,
        "sessions/2026/09/04/rollout-2026-09-04T14-00-00-99999999-8888-7777-6666-555555555555.jsonl",
        &[
            fork_meta_line(CHILD_ID, THREAD_ID, "2026-09-04T05:58:08Z"),
            // 回放父事件: 与父时间线签名一致, 前缀命中跳过
            token_count_line("2026-09-04T05:58:09Z", Some((100, 50, 0, 10)), None, None),
            // 子文件自身的新请求: 计入
            token_count_line(
                "2026-09-04T05:58:10Z",
                Some((150, 60, 0, 20)),
                Some((50, 10, 0, 10)),
                None,
            ),
        ],
    );
    let entries = collect_at(&base);
    assert_eq!(entries.len(), 3);
    assert_eq!(entries[0].input_tokens, 50); // 父 ev1: 100-50
    assert_eq!(entries[1].input_tokens, 50); // 父 ev2: 200-100-50
    assert_eq!(entries[2].input_tokens, 40); // 子: 50-10
    assert_eq!(entries[2].session_id.as_deref(), Some(CHILD_ID));
}

#[test]
fn test_fork_cutoff_ignores_parent_future_events() {
    let base = temp_base();
    write_rollout(
        &base,
        "sessions/2026/09/04/rollout-2026-09-04T13-58-05-11111111-2222-3333-4444-555555555555.jsonl",
        &[
            meta_line(),
            token_count_line("2026-07-10T03:00:01Z", Some((100, 50, 0, 10)), None, None),
            token_count_line("2026-07-10T03:00:06Z", Some((200, 100, 0, 20)), None, None),
        ],
    );
    write_rollout(
        &base,
        "sessions/2026/09/04/rollout-2026-09-04T14-00-00-99999999-8888-7777-6666-555555555555.jsonl",
        &[
            // 子 fork 时刻 :05, 父 :06 的事件晚于 fork, 不参与回放匹配
            fork_meta_line(CHILD_ID, THREAD_ID, "2026-07-10T03:00:05Z"),
            token_count_line("2026-07-10T03:00:07Z", Some((200, 100, 0, 20)), None, None),
        ],
    );
    let entries = collect_at(&base);
    assert_eq!(entries.len(), 3);
    assert_eq!(entries[0].input_tokens, 50); // 父 ev1: 100-50
    assert_eq!(entries[1].input_tokens, 50); // 父 ev2: 200-100-50
    // 父在 cutoff 后的事件不匹配, 子事件视为新请求
    assert_eq!(entries[2].input_tokens, 100);
}

#[test]
fn test_fork_of_completed_parent_skipped() {
    let base = temp_base();
    write_rollout(
        &base,
        "sessions/2026/09/04/rollout-2026-09-04T13-58-05-11111111-2222-3333-4444-555555555555.jsonl",
        &[
            meta_line(),
            token_count_line("2026-07-10T03:00:01Z", Some((100, 50, 0, 10)), None, None),
            token_count_line("2026-07-10T03:00:02Z", Some((200, 100, 0, 20)), None, None),
        ],
    );
    write_rollout(
        &base,
        "sessions/2026/09/04/rollout-2026-09-04T14-00-00-99999999-8888-7777-6666-555555555555.jsonl",
        &[
            // 父全部事件早于子 meta 时刻(:05): 父时间线相对回放拷贝不完整,
            // 回放无法可靠判定, 子文件不计(cc-switch defer 语义)
            fork_meta_line(CHILD_ID, THREAD_ID, "2026-07-10T03:00:05Z"),
            token_count_line("2026-07-10T03:00:06Z", Some((100, 50, 0, 10)), None, None),
            token_count_line("2026-07-10T03:00:07Z", Some((200, 100, 0, 20)), None, None),
            token_count_line(
                "2026-07-10T03:00:08Z",
                Some((300, 150, 0, 30)),
                Some((100, 50, 0, 10)),
                None,
            ),
        ],
    );
    let entries = collect_at(&base);
    // 父 2 条照常计入, 子文件不计
    assert_eq!(entries.len(), 2);
    assert!(
        entries
            .iter()
            .all(|e| e.session_id.as_deref() != Some(CHILD_ID))
    );
}

#[test]
fn test_fork_with_missing_parent_skipped() {
    let base = temp_base();
    write_rollout(
        &base,
        "sessions/2026/09/04/rollout-2026-09-04T13-58-05-99999999-8888-7777-6666-555555555555.jsonl",
        &[
            fork_meta_line(
                CHILD_ID,
                "00000000-0000-0000-0000-00000000000f",
                "2026-09-04T05:58:08Z",
            ),
            token_count_line("2026-09-04T05:58:09Z", Some((100, 50, 0, 10)), None, None),
        ],
    );
    // 父缺失时回放无法判定(cc-switch defer 语义), 子文件不计;
    // 本工具全量重扫, 父文件出现后下次运行自然恢复
    assert!(collect_at(&base).is_empty());
}

#[test]
fn test_fork_self_parent_skipped() {
    let base = temp_base();
    write_rollout(
        &base,
        "sessions/2026/09/04/rollout-2026-09-04T13-58-05-99999999-8888-7777-6666-555555555555.jsonl",
        &[
            // parent_id 与自身线程 id 相同: 异常元数据, 不计(cc-switch defer 语义)
            fork_meta_line(CHILD_ID, CHILD_ID, "2026-09-04T05:58:08Z"),
            token_count_line("2026-09-04T05:58:09Z", Some((100, 50, 0, 10)), None, None),
        ],
    );
    assert!(collect_at(&base).is_empty());
}

#[test]
fn test_conflicting_parent_ids_skipped() {
    let base = temp_base();
    write_rollout(
        &base,
        "sessions/2026/09/04/rollout-2026-09-04T13-58-05-99999999-8888-7777-6666-555555555555.jsonl",
        &[
            // forked_from_id 与 thread_spawn.parent_thread_id 不一致
            format!(
                r#"{{"timestamp":"2026-09-04T05:58:08Z","type":"session_meta","payload":{{"id":"{CHILD_ID}","forked_from_id":"{THREAD_ID}","source":{{"subagent":{{"thread_spawn":{{"parent_thread_id":"{OTHER_ID}"}}}}}},"cwd":"/tmp/x"}}}}"#
            ),
            token_count_line("2026-09-04T05:58:09Z", Some((100, 50, 0, 10)), None, None),
        ],
    );
    // 两处父声明冲突无法确定回放来源, 子文件不计
    assert!(collect_at(&base).is_empty());
}

#[test]
fn test_billable_file_without_meta_skipped() {
    let base = temp_base();
    write_rollout(
        &base,
        "sessions/2026/09/04/rollout-99999999-8888-7777-6666-555555555555.jsonl",
        &[token_count_line(
            "2026-09-04T05:58:09Z",
            Some((100, 50, 0, 10)),
            None,
            None,
        )],
    );
    // 无 session_meta 的计费文件不计(无法建立线程身份, cc-switch 同)
    assert!(collect_at(&base).is_empty());
}

#[test]
fn test_meta_filename_mismatch_skipped() {
    let base = temp_base();
    write_rollout(
        &base,
        "sessions/2026/09/04/rollout-2026-09-04T13-58-05-99999999-8888-7777-6666-555555555555.jsonl",
        // meta id 与文件名尾部/前置 uuid 均不一致: 异常/错拷文件整体不计
        &[
            meta_line(),
            token_count_line("2026-09-04T05:58:07Z", Some((100, 50, 0, 10)), None, None),
        ],
    );
    assert!(collect_at(&base).is_empty());
}

#[test]
fn test_double_segment_filename_accepted() {
    let base = temp_base();
    write_rollout(
        &base,
        "sessions/2026/09/04/rollout-2026-09-04T13-58-05-11111111-2222-3333-4444-555555555555_00000000-4444-8888-cccc-111111111111.jsonl",
        // 双段文件名: meta.id 匹配前置(线程本体) uuid, 放行
        &[
            meta_line(),
            token_count_line("2026-09-04T05:58:07Z", Some((100, 50, 0, 10)), None, None),
        ],
    );
    assert_eq!(collect_at(&base).len(), 1);
}

#[test]
fn test_uuid_case_insensitive_accepted() {
    let base = temp_base();
    // 文件名 uuid 小写, meta.id 大写: 归一后比较, 放行
    write_rollout(
        &base,
        "sessions/2026/09/04/rollout-2026-09-04T13-58-05-aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee.jsonl",
        &[
            meta_line_for(&OTHER_ID.to_ascii_uppercase()),
            token_count_line("2026-09-04T05:58:07Z", Some((100, 50, 0, 10)), None, None),
        ],
    );
    let entries = collect_at(&base);
    assert_eq!(entries.len(), 1);
    // session_id 落表为归一后的小写
    assert_eq!(entries[0].session_id.as_deref(), Some(OTHER_ID));
}

#[test]
fn test_uppercase_parent_id_resolved() {
    let base = temp_base();
    write_rollout(
        &base,
        "sessions/2026/09/04/rollout-2026-09-04T13-58-05-11111111-2222-3333-4444-555555555555.jsonl",
        &[
            meta_line(),
            token_count_line("2026-09-04T05:58:07Z", Some((100, 50, 0, 10)), None, None),
            token_count_line("2026-09-04T05:58:11Z", Some((200, 100, 0, 20)), None, None),
        ],
    );
    write_rollout(
        &base,
        "sessions/2026/09/04/rollout-2026-09-04T14-00-00-99999999-8888-7777-6666-555555555555.jsonl",
        &[
            // forked_from_id 大写: 归一后命中父时间线, 回放过滤照常生效
            fork_meta_line(
                CHILD_ID,
                &THREAD_ID.to_ascii_uppercase(),
                "2026-09-04T05:58:08Z",
            ),
            token_count_line("2026-09-04T05:58:09Z", Some((100, 50, 0, 10)), None, None),
            token_count_line(
                "2026-09-04T05:58:10Z",
                Some((150, 60, 0, 20)),
                Some((50, 10, 0, 10)),
                None,
            ),
        ],
    );
    let entries = collect_at(&base);
    assert_eq!(entries.len(), 3);
    assert_eq!(entries[0].input_tokens, 50); // 父 ev1
    assert_eq!(entries[1].input_tokens, 50); // 父 ev2
    assert_eq!(entries[2].input_tokens, 40); // 子自有新请求
    assert_eq!(entries[2].session_id.as_deref(), Some(CHILD_ID));
}

#[test]
fn test_cache_write_only_usage_accepted() {
    let base = temp_base();
    write_rollout(
        &base,
        "sessions/2026/09/04/rollout-11111111-2222-3333-4444-555555555555.jsonl",
        &[
            meta_line(),
            // 仅含 cache_write_input_tokens 的快照: 本工具扩展解析字段, 有效
            r#"{"timestamp":"2026-09-04T06:00:00Z","type":"event_msg","payload":{"type":"token_count","info":{"last_token_usage":{"cache_write_input_tokens":500}}}}"#.to_string(),
        ],
    );
    let entries = collect_at(&base);
    assert_eq!(entries.len(), 1);
    // input 不负扣除(fresh_input 保守不扣), cache write 计入
    assert_eq!(entries[0].input_tokens, 0);
    assert_eq!(entries[0].cache_creation_tokens, 500);
}

#[test]
fn test_ambiguous_parent_candidates_skipped() {
    let base = temp_base();
    fs::create_dir_all(base.join("sessions/2026/09/05")).unwrap();
    // 同一文件名 uuid 的两个内容不同的 rollout(异常状态, 不盲并时间线)
    write_rollout(
        &base,
        "sessions/2026/09/04/rollout-2026-09-04T13-58-05-11111111-2222-3333-4444-555555555555.jsonl",
        &[
            meta_line(),
            token_count_line("2026-09-04T05:58:07Z", Some((100, 50, 0, 10)), None, None),
        ],
    );
    write_rollout(
        &base,
        "sessions/2026/09/05/rollout-2026-09-05T10-00-00-11111111-2222-3333-4444-555555555555.jsonl",
        &[
            meta_line(),
            token_count_line("2026-09-05T10:00:00Z", Some((200, 50, 0, 10)), None, None),
        ],
    );
    write_rollout(
        &base,
        "sessions/2026/09/04/rollout-2026-09-04T14-00-00-99999999-8888-7777-6666-555555555555.jsonl",
        &[
            fork_meta_line(CHILD_ID, THREAD_ID, "2026-09-04T05:58:08Z"),
            token_count_line("2026-09-04T05:58:09Z", Some((100, 50, 0, 10)), None, None),
        ],
    );
    let entries = collect_at(&base);
    // 父文件本身照常计入; 子文件因父候选内容不一致不计
    assert_eq!(entries.len(), 2);
    assert!(
        entries
            .iter()
            .all(|e| e.session_id.as_deref() != Some(CHILD_ID))
    );
}

#[test]
fn test_archived_sessions_collected() {
    let base = temp_base();
    write_rollout(
        &base,
        "archived_sessions/rollout-2026-08-29T20-12-53-11111111-2222-3333-4444-555555555555.jsonl",
        &[
            meta_line(),
            token_count_line("2026-08-29T12:00:00Z", None, Some((10, 2, 0, 3)), None),
        ],
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
fn test_archived_duplicate_keeps_longest() {
    let base = temp_base();
    let name = "rollout-2026-08-29T20-12-53-11111111-2222-3333-4444-555555555555.jsonl";
    write_rollout(
        &base,
        &format!("archived_sessions/{name}"),
        &[
            meta_line(),
            token_count_line("2026-08-29T12:00:00Z", Some((100, 50, 0, 10)), None, None),
        ],
    );
    write_rollout(
        &base,
        &format!("sessions/2026/09/04/{name}"),
        &[
            meta_line(),
            token_count_line("2026-08-29T12:00:00Z", Some((100, 50, 0, 10)), None, None),
            token_count_line("2026-08-29T12:01:00Z", Some((150, 60, 0, 20)), None, None),
        ],
    );
    let entries = collect_at(&base);
    // archived 副本是同名 rollout 的旧快照, 保留字节最长者避免双算
    assert_eq!(entries.len(), 2);
    assert_eq!(entries[0].input_tokens, 50);
    assert_eq!(entries[1].input_tokens, 40);
}

#[test]
fn test_cross_source_adjacent_repeat_deduped() {
    let base = temp_base();
    write_rollout(
        &base,
        "sessions/2026/09/04/rollout-11111111-2222-3333-4444-555555555555.jsonl",
        &[
            meta_line(),
            token_count_line("2026-09-04T06:00:00Z", Some((100, 50, 0, 10)), None, None),
            // rate-limit 刷新换 limit_id 原样重发: 紧邻签名一致判零
            token_count_line_src(
                "2026-09-04T06:00:30Z",
                Some((100, 50, 0, 10)),
                None,
                None,
                "other",
            ),
        ],
    );
    assert_eq!(collect_at(&base).len(), 1);
}

#[test]
fn test_same_source_repeat_after_other_source_advance_deduped() {
    let base = temp_base();
    write_rollout(
        &base,
        "sessions/2026/09/04/rollout-11111111-2222-3333-4444-555555555555.jsonl",
        &[
            meta_line(),
            token_count_line_src(
                "2026-09-04T06:00:00Z",
                Some((100, 50, 0, 10)),
                Some((50, 10, 0, 10)),
                None,
                "a",
            ),
            token_count_line_src(
                "2026-09-04T06:01:00Z",
                Some((200, 100, 0, 20)),
                Some((60, 20, 0, 10)),
                None,
                "b",
            ),
            // a 源在 b 源推进后重发旧快照: 同源最新一致仍判零, 不因间隔失效
            token_count_line_src(
                "2026-09-04T06:02:00Z",
                Some((100, 50, 0, 10)),
                Some((50, 10, 0, 10)),
                None,
                "a",
            ),
        ],
    );
    let entries = collect_at(&base);
    assert_eq!(entries.len(), 2);
    assert_eq!(entries[0].input_tokens, 40);
    assert_eq!(entries[1].input_tokens, 40);
}

#[test]
fn test_is_rollout_filename() {
    let good =
        Path::new("/x/rollout-2026-09-04T13-58-05-01a06afe-e959-7c23-b5ec-30b29cd3bd35.jsonl");
    assert!(is_rollout_filename(good));
    let bad = Path::new("/x/session.jsonl");
    assert!(!is_rollout_filename(bad));
}

#[test]
fn test_thread_id_from_filename() {
    let f = |name: &str| thread_id_from_filename(Path::new(name));
    assert_eq!(
        f("/x/rollout-2026-09-04T13-58-05-11111111-2222-3333-4444-555555555555.jsonl"),
        Some("11111111-2222-3333-4444-555555555555".to_string())
    );
    // 双段文件名(thread/revert 替换 rollout): 尾段为物理 rollout id
    assert_eq!(
        f(
            "/x/rollout-2026-09-04T13-58-05-11111111-2222-3333-4444-555555555555_99999999-8888-7777-6666-555555555555.jsonl"
        ),
        Some("99999999-8888-7777-6666-555555555555".to_string())
    );
    assert_eq!(f("/x/session.jsonl"), None);
    assert_eq!(f("/x/rollout-short.jsonl"), None);
}

#[test]
fn test_leading_thread_id_from_filename() {
    let f = |name: &str| leading_thread_id_from_filename(Path::new(name));
    // 单段文件名无前置 uuid
    assert_eq!(
        f("/x/rollout-2026-09-04T13-58-05-11111111-2222-3333-4444-555555555555.jsonl"),
        None
    );
    assert_eq!(
        f(
            "/x/rollout-2026-09-04T13-58-05-11111111-2222-3333-4444-555555555555_99999999-8888-7777-6666-555555555555.jsonl"
        ),
        Some("11111111-2222-3333-4444-555555555555".to_string())
    );
    assert_eq!(f("/x/rollout-short.jsonl"), None);
}
