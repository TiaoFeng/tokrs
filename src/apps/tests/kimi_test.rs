use super::*;
use std::fs;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

fn temp_dir() -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "tokrs-kimi-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    fs::create_dir_all(&dir).unwrap();
    dir
}

/// 在 base/<session>/agents/<agent>/wire.jsonl 写入若干事件行
fn write_wire(base: &Path, session: &str, agent: &str, lines: &[String]) {
    let dir = base.join(session).join("agents").join(agent);
    fs::create_dir_all(&dir).unwrap();
    let mut content = lines.join("\n");
    content.push('\n');
    fs::write(dir.join("wire.jsonl"), content).unwrap();
}

#[allow(clippy::too_many_arguments)]
fn usage_line(
    agent: &str,
    model: &str,
    time: i64,
    input: u64,
    output: u64,
    cache_read: u64,
    cache_creation: u64,
    scope: Option<&str>,
) -> String {
    let scope_field = match scope {
        Some(s) => format!("\"usageScope\":\"{s}\","),
        None => String::new(),
    };
    format!(
        r#"{{"type":"usage.record","agentId":"{agent}","model":"{model}",{scope_field}"usage":{{"inputOther":{input},"output":{output},"inputCacheRead":{cache_read},"inputCacheCreation":{cache_creation}}},"time":{time}}}"#
    )
}

#[test]
fn test_face_value_ingest_and_field_mapping() {
    let base = temp_dir();
    write_wire(
        &base,
        "session_a",
        "main",
        &[
            usage_line(
                "main",
                "moonshot-cn/kimi-k3",
                1_787_984_194_054,
                2077,
                197,
                19200,
                0,
                Some("turn"),
            ),
            // 非 usage.record 事件应被忽略
            r#"{"type":"llm.request","agentId":"main","model":"kimi-k3","time":1787984190000}"#
                .to_string(),
        ],
    );
    let entries = collect_from(&base).unwrap();
    assert_eq!(entries.len(), 1);
    let e = &entries[0];
    assert_eq!(e.app, AppKind::Kimi);
    // 完整别名 moonshot-cn/kimi-k3 已剥前缀归一(与 codex 同款, 合并计价)
    assert_eq!(e.model, "kimi-k3");
    assert_eq!(e.session_id.as_deref(), Some("session_a"));
    // inputOther 原样入账(不扣缓存): 2077, 毫秒时间戳转秒
    assert_eq!(e.input_tokens, 2077);
    assert_eq!(e.output_tokens, 197);
    assert_eq!(e.cache_read_tokens, 19200);
    assert_eq!(e.cache_creation_tokens, 0);
    assert_eq!(e.created_at, 1_787_984_194);
    assert!(e.self_cost_usd.is_none());
    fs::remove_dir_all(&base).ok();
}

#[test]
fn test_model_normalization() {
    let base = temp_dir();
    write_wire(
        &base,
        "session_a",
        "main",
        &[
            // 多级前缀取最后一段
            usage_line(
                "main",
                "openrouter/moonshot/kimi-k3",
                10,
                1,
                1,
                0,
                0,
                Some("turn"),
            ),
            // 空串 model -> unknown
            usage_line("main", "", 20, 1, 1, 0, 0, Some("turn")),
            // 全空格 model -> unknown
            usage_line("main", "   ", 30, 1, 1, 0, 0, Some("turn")),
        ],
    );
    let mut entries = collect_from(&base).unwrap();
    entries.sort_by_key(|e| e.created_at);
    assert_eq!(entries.len(), 3);
    assert_eq!(entries[0].model, "kimi-k3");
    assert_eq!(entries[1].model, "unknown");
    assert_eq!(entries[2].model, "unknown");
    fs::remove_dir_all(&base).ok();
}

#[test]
fn test_fork_copy_dedup_and_retry_kept() {
    let base = temp_dir();
    let u1 = usage_line("main", "m1", 1000, 10, 20, 100, 5, Some("turn"));
    let u2 = usage_line("main", "m1", 1500, 30, 40, 200, 6, Some("turn"));
    // 真实会话 session_a: u1 + u2, 其中 u2 有一次失败重试(不同毫秒同用量, 官方二次计费, 应各自入账)
    let retry = usage_line("main", "m1", 1600, 30, 40, 200, 6, Some("turn"));
    write_wire(&base, "session_a", "main", &[u1.clone(), u2.clone(), retry]);
    // fork 副本 session_b: 逐字节复制 u1/u2, 不应重复入账
    write_wire(&base, "session_b", "main", &[u1, u2]);
    let mut entries = collect_from(&base).unwrap();
    // 总会话归属: u1/u2 各自唯一, 副本 0 条, retry 1 条
    assert_eq!(entries.len(), 3, "fork 副本去重后应剩 u1/u2/retry");
    let sess_a = entries
        .iter()
        .filter(|e| e.session_id.as_deref() == Some("session_a"))
        .count();
    assert_eq!(sess_a, 3, "三条真实记录都归属先到者 session_a");
    entries.sort_by_key(|e| (e.created_at, e.input_tokens));
    assert_eq!(entries[0].total_tokens(), 10 + 20 + 100 + 5);
    assert_eq!(entries[1].total_tokens(), 30 + 40 + 200 + 6);
    assert_eq!(entries[2].total_tokens(), 30 + 40 + 200 + 6);
    // time=1600 小于毫秒阈值, 按秒处理
    assert_eq!(entries[2].created_at, 1600);
    fs::remove_dir_all(&base).ok();
}

#[test]
fn test_subagent_files_summed_and_non_wire_ignored() {
    let base = temp_dir();
    write_wire(
        &base,
        "session_a",
        "main",
        &[usage_line("main", "m", 10, 1, 2, 0, 0, Some("turn"))],
    );
    write_wire(
        &base,
        "session_a",
        "agent-0",
        &[usage_line("agent-0", "m", 10, 1, 2, 0, 0, Some("turn"))],
    );
    // 同目录下的其它 jsonl 不是 wire 记录, 应忽略
    let dir = base.join("session_a").join("agents").join("main");
    fs::write(
        dir.join("other.jsonl"),
        format!(
            "{}\n",
            usage_line("main", "m", 10, 1, 2, 0, 0, Some("turn"))
        ),
    )
    .unwrap();
    let entries = collect_from(&base).unwrap();
    // main 与 agent-0 的 agentId 不同, 各自独立计费; other.jsonl 不采
    assert_eq!(entries.len(), 2);
    fs::remove_dir_all(&base).ok();
}

#[test]
fn test_scope_and_zero_filtering() {
    let base = temp_dir();
    write_wire(
        &base,
        "session_a",
        "main",
        &[
            // 显式 session 级聚合快照 → 跳过防双算
            usage_line("main", "m", 100, 5, 5, 5, 5, Some("session")),
            // 四项全零 → 跳过
            usage_line("main", "m", 200, 0, 0, 0, 0, Some("turn")),
            // scope 缺失 → 向后兼容放行; 模型缺失 → unknown
            r#"{"type":"usage.record","agentId":"main","usage":{"inputOther":7,"output":8,"inputCacheRead":9,"inputCacheCreation":1},"time":300}"#.to_string(),
        ],
    );
    let entries = collect_from(&base).unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].model, "unknown");
    assert_eq!(entries[0].total_tokens(), 7 + 8 + 9 + 1);
    fs::remove_dir_all(&base).ok();
}

#[test]
fn test_missing_base_returns_empty() {
    let base = temp_dir();
    fs::remove_dir_all(&base).unwrap();
    assert!(collect_from(&base).unwrap().is_empty());
}
