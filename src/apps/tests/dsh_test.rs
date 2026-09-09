use super::*;
use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

/// 现实量级的 epoch 毫秒(> 1e11, timestamp_to_epoch 按毫秒自适应转秒)
const TS: i64 = 1_788_950_002_000;

fn temp_dir() -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "tokrs-dsh-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    fs::create_dir_all(&dir).unwrap();
    dir
}

/// 在 base/<ws>/session-<id>/session.jsonl.zstd 写入 zstd 压缩的事件行
fn write_session(base: &Path, ws: &str, id: &str, lines: &[String]) {
    let dir = base.join(ws).join(format!("session-{id}"));
    fs::create_dir_all(&dir).unwrap();
    let mut content = lines.join("\n");
    content.push('\n');
    let compressed = zstd::encode_all(content.as_bytes(), 3).unwrap();
    fs::write(dir.join("session.jsonl.zstd"), compressed).unwrap();
}

fn header(id: &str, ts_ms: i64) -> String {
    format!(
        r#"{{"type":"session","version":0,"id":"{id}","createdAt":{ts_ms},"cwd":"/tmp/ws","delegationDepth":0,"agentPreset":"standard"}}"#
    )
}

fn selection(ts_ms: i64, model: &str) -> String {
    format!(
        r#"{{"type":"model/selection","seq":1,"time":{ts_ms},"data":{{"provider":"deepseek-official","model":"{model}","reasoningEffort":"low"}}}}"#
    )
}

#[allow(clippy::too_many_arguments)]
fn assistant_message(
    id: &str,
    ts_ms: i64,
    model: Option<&str>,
    input: u64,
    output: u64,
    cache_read: u64,
    cache_write: u64,
    reasoning: u64,
) -> String {
    let source = match model {
        Some(m) => {
            format!(r#""source":{{"kind":"model","provider":"deepseek-official","model":"{m}"}}"#)
        }
        None => r#""source":{"kind":"model"}"#.to_string(),
    };
    format!(
        r#"{{"type":"assistant/message","seq":9,"time":{ts_ms},"data":{{"turn":1,"step":1,"message":{{"role":"assistant","content":[{{"type":"text","text":"hi"}}],{source},"id":"{id}"}},"usage":{{"inputTokens":{input},"outputTokens":{output},"totalTokens":{},"cacheReadTokens":{cache_read},"cacheWriteTokens":{cache_write},"reasoningTokens":{reasoning}}}}},"surfaceOp":"append"}}"#,
        input + output + cache_read + reasoning
    )
}

/// assistant/chunk 的 usage 预发布事件(与 message 同值)——不应入账
#[allow(clippy::too_many_arguments)]
fn usage_chunk(ts_ms: i64, input: u64, output: u64, cache_read: u64) -> String {
    format!(
        r#"{{"type":"assistant/chunk","seq":8,"time":{ts_ms},"data":{{"turn":1,"step":1,"chunk":{{"type":"usage","usage":{{"inputTokens":{input},"outputTokens":{output},"totalTokens":{},"cacheReadTokens":{cache_read},"reasoningTokens":0}}}}}}}}"#,
        input + output + cache_read
    )
}

#[test]
fn test_face_value_and_field_mapping() {
    let base = temp_dir();
    write_session(
        &base,
        "--home-u-ws--",
        "uuid-1",
        &[
            header("session-uuid-1", 1_788_950_000_000),
            selection(1_788_950_001_000, "deepseek-official/deepseek-v4-flash"),
            usage_chunk(TS, 8016, 313, 5000),
            assistant_message(
                "msg-1",
                TS,
                Some("deepseek-v4-flash"),
                8016,
                313,
                5000,
                200,
                100,
            ),
        ],
    );
    let entries = collect_from(&base).unwrap();
    // usage chunk 不计, 恰一条(防双算)
    assert_eq!(entries.len(), 1);
    let e = &entries[0];
    assert_eq!(e.app, AppKind::Dsh);
    assert_eq!(e.model, "deepseek-v4-flash");
    assert_eq!(e.session_id.as_deref(), Some("session-uuid-1"));
    // input 与 cacheRead/cacheWrite 互不包含: fresh 直用; reasoning 已含于 output 不另加
    assert_eq!(e.input_tokens, 8016);
    assert_eq!(e.output_tokens, 313);
    assert_eq!(e.cache_read_tokens, 5000);
    assert_eq!(e.cache_creation_tokens, 200);
    // epoch 毫秒自适应转秒
    assert_eq!(e.created_at, 1_788_950_002);
    assert!(e.self_cost_usd.is_none());
    fs::remove_dir_all(&base).ok();
}

#[test]
fn test_chunk_usage_not_counted() {
    let base = temp_dir();
    // 仅 usage chunk(无 assistant/message) → 无条目
    write_session(
        &base,
        "ws",
        "a",
        &[header("session-a", TS), usage_chunk(TS, 5, 5, 0)],
    );
    assert!(collect_from(&base).unwrap().is_empty());
    // chunk + message 同值并存 → 恰一条
    write_session(
        &base,
        "ws",
        "b",
        &[
            header("session-b", TS),
            usage_chunk(TS, 5, 5, 0),
            assistant_message("m1", TS, Some("m"), 5, 5, 0, 0, 0),
        ],
    );
    assert_eq!(collect_from(&base).unwrap().len(), 1);
    fs::remove_dir_all(&base).ok();
}

#[test]
fn test_header_required_and_nested_layout() {
    let base = temp_dir();
    // 首条有效 JSON 不是 session header: 整个文件跳过
    write_session(
        &base,
        "ws",
        "bad",
        &[assistant_message("m1", TS, Some("m"), 1, 1, 0, 0, 0)],
    );
    // <ws>/session-<id>/ 嵌套布局应被发现
    write_session(
        &base,
        "--home-u-proj--",
        "ok",
        &[
            header("session-ok", TS),
            assistant_message("m2", TS, Some("m"), 4, 4, 0, 0, 0),
        ],
    );
    let entries = collect_from(&base).unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].session_id.as_deref(), Some("session-ok"));
    fs::remove_dir_all(&base).ok();
}

#[test]
fn test_model_fallback_chain() {
    let base = temp_dir();
    write_session(
        &base,
        "ws",
        "s",
        &[
            header("session-s", TS),
            // 会话级 selection: 前缀剥除 + 小写归一
            selection(1_788_950_001_000, "DeepSeek-Official/DeepSeek-V4-Flash"),
            // 消息级缺 source.model → 回退最近 selection
            assistant_message("m1", TS, None, 1, 1, 0, 0, 0),
            // 后续无模型消息沿用会话级当前模型(codex turn_context 式持久化)
            assistant_message("m2", TS + 1000, None, 1, 1, 0, 0, 0),
            // 消息级 source.model 优先
            assistant_message("m3", TS + 2000, Some("deepseek-chat"), 1, 1, 0, 0, 0),
        ],
    );
    let mut entries = collect_from(&base).unwrap();
    entries.sort_by_key(|e| e.created_at);
    assert_eq!(entries.len(), 3);
    assert_eq!(entries[0].model, "deepseek-v4-flash");
    assert_eq!(entries[1].model, "deepseek-v4-flash");
    assert_eq!(entries[2].model, "deepseek-chat");
    fs::remove_dir_all(&base).ok();
    // 无 selection 且消息级无 model → unknown
    let base2 = temp_dir();
    write_session(
        &base2,
        "ws",
        "t",
        &[
            header("session-t", TS),
            assistant_message("m9", TS, None, 1, 1, 0, 0, 0),
        ],
    );
    assert_eq!(collect_from(&base2).unwrap()[0].model, "unknown");
    fs::remove_dir_all(&base2).ok();
}

#[test]
fn test_dedup_first_wins() {
    let base = temp_dir();
    write_session(
        &base,
        "ws",
        "s",
        &[
            header("session-s", TS),
            assistant_message("m1", TS, Some("m"), 10, 10, 0, 0, 0),
            // 同 id 重发(不同用量): first-wins 保留先到者, 防副本双算
            assistant_message("m1", TS + 1000, Some("m"), 30, 30, 0, 0, 0),
        ],
    );
    let entries = collect_from(&base).unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].input_tokens, 10);
    assert_eq!(entries[0].created_at, 1_788_950_002);
    fs::remove_dir_all(&base).ok();
}

#[test]
fn test_zero_and_missing_usage_filtering() {
    let base = temp_dir();
    write_session(
        &base,
        "ws",
        "s",
        &[
            header("session-s", TS),
            // 四项全零 → 跳过
            assistant_message("m0", TS, Some("m"), 0, 0, 0, 0, 0),
            // 仅 cache_read → 保留(纯缓存命中)
            assistant_message("m1", TS, Some("m"), 0, 0, 500, 0, 0),
            // 仅 cache_write → 保留
            assistant_message("m2", TS, Some("m"), 0, 0, 0, 300, 0),
            // 真实样例形状: 无 cacheWriteTokens/reasoningTokens 字段 → 缺失按 0
            format!(
                r#"{{"type":"assistant/message","seq":9,"time":{TS},"data":{{"turn":1,"step":1,"message":{{"role":"assistant","id":"m4","source":{{"kind":"model","provider":"deepseek-official","model":"m"}}}},"usage":{{"inputTokens":3,"outputTokens":4,"totalTokens":7,"cacheReadTokens":0,"reasoningTokens":0}}}},"surfaceOp":"append"}}"#
            ),
            // usage 缺失 → 跳过
            format!(
                r#"{{"type":"assistant/message","seq":9,"time":{TS},"data":{{"turn":1,"step":1,"message":{{"role":"assistant","id":"m5"}}}},"surfaceOp":"append"}}"#
            ),
        ],
    );
    let mut entries = collect_from(&base).unwrap();
    entries.sort_by_key(|e| (e.input_tokens, e.cache_read_tokens));
    assert_eq!(entries.len(), 3);
    // 排序后: (0,0)纯 cache_write → (0,500)纯 cache_read → (3,0)混合
    assert_eq!(entries[0].cache_creation_tokens, 300);
    assert_eq!(entries[1].cache_read_tokens, 500);
    assert_eq!(entries[2].input_tokens, 3);
    fs::remove_dir_all(&base).ok();
}

#[test]
fn test_needle_false_positive_ignored() {
    let base = temp_dir();
    // 正文含 needle 字面量的非入账行: 命中预过滤被解析, 但 type 不符被忽略
    write_session(
        &base,
        "ws",
        "s",
        &[
            header("session-s", TS),
            format!(
                r#"{{"type":"user/message","seq":2,"time":{TS},"data":{{"content":[{{"type":"text","text":"what is assistant/message?"}}],"role":"user","id":"u1"}},"surfaceOp":"append"}}"#
            ),
            assistant_message("m1", TS, Some("m"), 7, 3, 0, 0, 0),
        ],
    );
    let entries = collect_from(&base).unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].input_tokens, 7);
    fs::remove_dir_all(&base).ok();
}

#[test]
fn test_truncated_zstd_keeps_partial() {
    let base = temp_dir();
    // ~1000 条消息(跨多个 zstd block), 截断帧尾: 已解压出的前段条目保留
    let mut lines = vec![header("session-t", TS)];
    for i in 0..1000 {
        lines.push(assistant_message(
            &format!("m{i}"),
            TS + i,
            Some("m"),
            10,
            5,
            0,
            0,
            0,
        ));
    }
    let dir = base.join("ws").join("session-t");
    fs::create_dir_all(&dir).unwrap();
    let raw = lines.join("\n") + "\n";
    let compressed = zstd::encode_all(raw.as_bytes(), 3).unwrap();
    fs::write(
        dir.join("session.jsonl.zstd"),
        &compressed[..compressed.len() - 8],
    )
    .unwrap();
    let entries = collect_from(&base).unwrap();
    assert!(
        entries.len() >= 500,
        "截断后应保留已解压的前段条目, 实得 {}",
        entries.len()
    );
    fs::remove_dir_all(&base).ok();
}

#[test]
fn test_missing_base_returns_empty() {
    let base = temp_dir();
    fs::remove_dir_all(&base).unwrap();
    assert!(collect_from(&base).unwrap().is_empty());
}

#[test]
fn test_dsh_base_env_resolution() {
    let home = Path::new("/home/u");
    // 未设/空串 → 默认; ~/ 展开; 绝对直用
    assert_eq!(dsh_base(home, None), PathBuf::from("/home/u/.dsh"));
    assert_eq!(
        dsh_base(home, Some(OsStr::new(""))),
        PathBuf::from("/home/u/.dsh")
    );
    assert_eq!(
        dsh_base(home, Some(OsStr::new("~/ds"))),
        PathBuf::from("/home/u/ds")
    );
    assert_eq!(
        dsh_base(home, Some(OsStr::new("/abs/ds"))),
        PathBuf::from("/abs/ds")
    );
    // 非绝对路径: 警告后回退默认
    assert_eq!(
        dsh_base(home, Some(OsStr::new("rel/ds"))),
        PathBuf::from("/home/u/.dsh")
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
    for i in 0..4 {
        write_session(
            &base,
            &format!("ws{i}"),
            &format!("u{i}"),
            &[
                header(&format!("session-u{i}"), TS + i * 2000),
                assistant_message(
                    &format!("m{i}a"),
                    TS + i * 2000,
                    Some("m"),
                    10 + i as u64,
                    5,
                    2,
                    1,
                    0,
                ),
                assistant_message(
                    &format!("m{i}b"),
                    TS + i * 2000 + 1000,
                    Some("m2"),
                    3,
                    7,
                    0,
                    0,
                    0,
                ),
            ],
        );
    }
    let a = sorted(collect_from(&base).unwrap());
    let b = sorted(collect_from(&base).unwrap());
    assert_eq!(a.len(), 8);
    assert_eq!(a, b);
    fs::remove_dir_all(&base).ok();
}
