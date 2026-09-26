use super::*;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

fn temp_dir() -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "tokrs-copilot-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    fs::create_dir_all(&dir).unwrap();
    dir
}

/// 在 <user>/workspaceStorage/<ws>/chatSessions/<id>.jsonl 写入操作行
fn write_session(user: &Path, ws: &str, id: &str, ops: &[String]) -> PathBuf {
    let dir = user.join("workspaceStorage").join(ws).join("chatSessions");
    write_ops(&dir, id, ops)
}

/// 在 <user>/globalStorage/emptyWindowChatSessions/<id>.jsonl 写入操作行(空窗口会话)
fn write_empty_window(user: &Path, id: &str, ops: &[String]) -> PathBuf {
    write_ops(
        &user.join("globalStorage").join("emptyWindowChatSessions"),
        id,
        ops,
    )
}

fn write_ops(dir: &Path, id: &str, ops: &[String]) -> PathBuf {
    fs::create_dir_all(dir).unwrap();
    let mut content = ops.join("\n");
    content.push('\n');
    let path = dir.join(format!("{id}.jsonl"));
    fs::write(&path, content).unwrap();
    path
}

fn init_op(session_id: &str, creation: i64) -> String {
    format!(
        r#"{{"kind":0,"v":{{"version":3,"creationDate":{creation},"sessionId":"{session_id}","requests":[]}}}}"#
    )
}

fn push_op(request: &str) -> String {
    format!(r#"{{"kind":2,"k":["requests"],"i":null,"v":[{request}]}}"#)
}

fn insert_op(index: usize, request: &str) -> String {
    format!(r#"{{"kind":2,"k":["requests"],"i":{index},"v":[{request}]}}"#)
}

fn request_obj(request_id: &str, model: &str, timestamp: i64) -> String {
    format!(r#"{{"requestId":"{request_id}","modelId":"{model}","timestamp":{timestamp}}}"#)
}

fn set_op(index: usize, field: &str, value: &str) -> String {
    format!(r#"{{"kind":1,"k":["requests",{index},"{field}"],"v":{value}}}"#)
}

#[test]
fn test_face_value_ingest_and_field_mapping() {
    let user = temp_dir();
    write_session(
        &user,
        "aa11",
        "sess_alpha",
        &[
            init_op("sess_alpha", 1_790_000_000_000),
            // 无关路径操作与 response 巨型推送行: 均不影响用量
            r#"{"kind":1,"k":["inputState","inputText"],"v":"hi"}"#.to_string(),
            push_op(&request_obj(
                "request_1",
                "copilot/gpt-4.1",
                1_790_003_600_000,
            )),
            r#"{"kind":2,"k":["requests",0,"response"],"v":[{"kind":"markdown","content":"x"}]}"#
                .to_string(),
            set_op(0, "promptTokens", "3991"),
            set_op(0, "completionTokens", "618"),
        ],
    );
    let entries = collect_from(&user).unwrap();
    assert_eq!(entries.len(), 1);
    let e = &entries[0];
    assert_eq!(e.app, AppKind::Copilot);
    // copilot/ 前缀已剥, 毫秒时间戳转秒
    assert_eq!(e.model, "gpt-4.1");
    assert_eq!(e.session_id.as_deref(), Some("sess_alpha"));
    assert_eq!(e.created_at, 1_790_003_600);
    // input 直用 promptTokens(上游无缓存拆分), cache 恒 0, 无自报成本
    assert_eq!(e.input_tokens, 3991);
    assert_eq!(e.output_tokens, 618);
    assert_eq!(e.cache_read_tokens, 0);
    assert_eq!(e.cache_creation_tokens, 0);
    assert!(e.self_cost_usd.is_none());
    fs::remove_dir_all(&user).ok();
}

#[test]
fn test_streaming_last_value_wins() {
    let user = temp_dir();
    write_session(
        &user,
        "bb22",
        "sess_stream",
        &[
            init_op("sess_stream", 1),
            push_op(&request_obj("request_s", "copilot/auto", 1_790_100_000_000)),
            set_op(0, "promptTokens", "100"),
            set_op(0, "completionTokens", "10"),
            set_op(0, "promptTokens", "20872"),
            set_op(0, "completionTokens", "2368"),
        ],
    );
    let entries = collect_from(&user).unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].input_tokens, 20872);
    assert_eq!(entries[0].output_tokens, 2368);
    // Auto 模式实际模型不落盘: copilot/auto 按 auto 入账
    assert_eq!(entries[0].model, "auto");
    fs::remove_dir_all(&user).ok();
}

#[test]
fn test_push_snapshot_with_tokens() {
    let user = temp_dir();
    // 请求表推送时已带 token(终态快照, 无后续 set)
    let request = r#"{"requestId":"request_snap","modelId":"copilot/claude-sonnet-4","timestamp":1790200000000,"promptTokens":140860,"completionTokens":39860}"#;
    write_session(
        &user,
        "cc33",
        "sess_snap",
        &[init_op("sess_snap", 1), push_op(request)],
    );
    let entries = collect_from(&user).unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].input_tokens, 140860);
    assert_eq!(entries[0].output_tokens, 39860);
    assert_eq!(entries[0].model, "claude-sonnet-4");
    fs::remove_dir_all(&user).ok();
}

#[test]
fn test_incomplete_and_zero_requests_skipped() {
    let user = temp_dir();
    write_session(
        &user,
        "dd44",
        "sess_skip",
        &[
            init_op("sess_skip", 1),
            // 未完成(取消): token 从未写入
            push_op(&request_obj("request_cancel", "copilot/auto", 1_000)),
            // 全零: set 写入 0/0
            push_op(&request_obj("request_zero", "copilot/auto", 2_000)),
            set_op(1, "promptTokens", "0"),
            set_op(1, "completionTokens", "0"),
        ],
    );
    let entries = collect_from(&user).unwrap();
    assert!(entries.is_empty());
    fs::remove_dir_all(&user).ok();
}

#[test]
fn test_insert_semantics() {
    let user = temp_dir();
    // 分支/重试场景: 新请求插入到索引 0, 后续 set 按操作时索引寻址
    write_session(
        &user,
        "ee55",
        "sess_branch",
        &[
            init_op("sess_branch", 1),
            insert_op(0, &request_obj("request_a", "copilot/a", 1_000)),
            push_op(&request_obj("request_b", "copilot/b", 2_000)),
            insert_op(0, &request_obj("request_c", "copilot/c", 3_000)),
            set_op(0, "promptTokens", "10"),
            set_op(1, "completionTokens", "20"),
            set_op(2, "promptTokens", "30"),
            set_op(2, "completionTokens", "40"),
        ],
    );
    let mut entries = collect_from(&user).unwrap();
    entries.sort_by_key(|e| e.model.clone());
    // 实例顺序 [C, A, B], 各自携带对应 set 值
    let got: Vec<(&str, u64, u64)> = entries
        .iter()
        .map(|e| (e.model.as_str(), e.input_tokens, e.output_tokens))
        .collect();
    assert_eq!(got, vec![("a", 0, 20), ("b", 30, 40), ("c", 10, 0)]);
    fs::remove_dir_all(&user).ok();
}

#[test]
fn test_dedup_across_workspace_copies() {
    let user = temp_dir();
    let ops = vec![
        init_op("sess_copy", 1),
        push_op(&request_obj(
            "request_dup",
            "copilot/gpt-4.1",
            1_790_200_000_000,
        )),
        set_op(0, "promptTokens", "500"),
        set_op(0, "completionTokens", "60"),
    ];
    // 同一会话副本出现在两个工作区(复制/移动场景), first-wins 不双算
    write_session(&user, "aa_first", "sess_copy", &ops);
    write_session(&user, "zz_last", "sess_copy", &ops);
    let entries = collect_from(&user).unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].input_tokens, 500);
    fs::remove_dir_all(&user).ok();
}

#[test]
fn test_model_normalization() {
    let user = temp_dir();
    let models = [
        ("copilot/gpt-4.1", "gpt-4.1"),
        ("customendpoint/opencode/omen-alpha", "omen-alpha"), // 多级前缀取最后一段
        ("", "unknown"),
        ("   ", "unknown"),
        ("OpenAI/GPT-5", "gpt-5"), // 大写归一
        // 扩展装饰解码: 剥 vendor 同名命名空间与 ::variant, 对齐其它渠道的同名模型
        (
            "opencodego/OpenCode Go/opencodego:deepseek-v4.1-flash::session-2026-05-21-b",
            "deepseek-v4.1-flash",
        ),
        // 冒号守护: 前缀与 vendor 不同不剥(ollama 风格单冒号模型名, 冒号是名字一部分)
        ("customendpoint/ollama/llama3:70b", "llama3:70b"),
        // 仅 ::variant 装饰(无命名空间): 剥除
        ("acme/tool::v2", "tool"),
    ];
    let mut ops = vec![init_op("sess_models", 1)];
    for (i, (raw, _)) in models.iter().enumerate() {
        let request = request_obj(
            &format!("request_m{i}"),
            raw,
            1_790_000_000_000 + i as i64 * 1000,
        );
        ops.push(push_op(&request));
        ops.push(set_op(i, "completionTokens", "1"));
    }
    write_session(&user, "ff66", "sess_models", &ops);
    let mut entries = collect_from(&user).unwrap();
    entries.sort_by_key(|e| e.model.clone());
    let got: Vec<&str> = entries.iter().map(|e| e.model.as_str()).collect();
    let mut want: Vec<&str> = models.iter().map(|(_, want)| *want).collect();
    want.sort_unstable();
    assert_eq!(got, want);
    fs::remove_dir_all(&user).ok();
}

#[test]
fn test_time_fallback_chain() {
    let user = temp_dir();
    write_session(
        &user,
        "gg77",
        "sess_time",
        &[
            init_op("sess_time", 1_790_300_000_000),
            // timestamp 缺失 → responseTimestamp 兜底
            r#"{"kind":2,"k":["requests"],"i":null,"v":[{"requestId":"request_rts","modelId":"copilot/x","responseTimestamp":1790300600000}]}"#
                .to_string(),
            set_op(0, "completionTokens", "1"),
            // 两者缺失 → 会话 creationDate 兜底
            r#"{"kind":2,"k":["requests"],"i":null,"v":[{"requestId":"request_created","modelId":"copilot/x"}]}"#
                .to_string(),
            set_op(1, "completionTokens", "1"),
        ],
    );
    // 三处数据缺失(连 creationDate 也无)视为损坏跳过
    write_session(
        &user,
        "gg77",
        "sess_no_time",
        &[
            r#"{"kind":0,"v":{"version":3,"sessionId":"sess_no_time","requests":[]}}"#.to_string(),
            r#"{"kind":2,"k":["requests"],"i":null,"v":[{"requestId":"request_lost","modelId":"copilot/x"}]}"#
                .to_string(),
            set_op(0, "completionTokens", "5"),
        ],
    );
    let mut entries = collect_from(&user).unwrap();
    entries.sort_by_key(|e| e.created_at);
    assert_eq!(entries.len(), 2);
    assert_eq!(entries[0].created_at, 1_790_300_000);
    assert_eq!(entries[1].created_at, 1_790_300_600);
    fs::remove_dir_all(&user).ok();
}

#[test]
fn test_corrupt_and_truncated_tolerated() {
    let user = temp_dir();
    let dir = user
        .join("workspaceStorage")
        .join("hh88")
        .join("chatSessions");
    fs::create_dir_all(&dir).unwrap();
    let mut content = String::new();
    content.push_str(&init_op("sess_bad", 1));
    content.push('\n');
    content.push_str("not-json-garbage\n"); // 畸形行跳过
    content.push_str(&push_op(&request_obj(
        "request_ok",
        "copilot/x",
        1_790_400_000_000,
    )));
    content.push('\n');
    content.push_str(&set_op(0, "promptTokens", "77"));
    content.push('\n');
    content.push_str(&set_op(0, "completionTokens", "7"));
    content.push('\n');
    // 截断末行(会话在写属常态): 无效 JSON 跳过, 已解析条目保留
    content.push_str(r#"{"kind":1,"k":["requests",0,"completionTokens"],"v":404"#);
    fs::write(dir.join("sess_bad.jsonl"), content).unwrap();
    let entries = collect_from(&user).unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].input_tokens, 77);
    assert_eq!(entries[0].output_tokens, 7);
    fs::remove_dir_all(&user).ok();
}

#[test]
fn test_full_replace_defensive_paths() {
    let user = temp_dir();
    write_session(
        &user,
        "jj00",
        "sess_replace",
        &[
            init_op("sess_replace", 1),
            push_op(&request_obj("request_old", "copilot/old", 1_000)),
            // 整表替换: 旧请求被移除, 不残留旧值
            r#"{"kind":1,"k":["requests"],"v":[{"requestId":"request_new","modelId":"copilot/new","timestamp":1790600000000,"completionTokens":5}]}"#
                .to_string(),
            // 整请求替换(防御分支)
            r#"{"kind":1,"k":["requests",0],"v":{"requestId":"request_new","modelId":"copilot/new","timestamp":1790600000000,"completionTokens":8}}"#
                .to_string(),
        ],
    );
    let entries = collect_from(&user).unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].model, "new");
    assert_eq!(entries[0].output_tokens, 8);
    fs::remove_dir_all(&user).ok();
}

#[test]
fn test_set_ops_and_push_edges() {
    let user = temp_dir();
    write_session(
        &user,
        "kk11",
        "sess_edges",
        &[
            init_op("sess_edges", 1),
            // 单个(非数组)推送值与越界插入索引: i 收敛到末尾
            r#"{"kind":2,"k":["requests"],"i":9,"v":{"requestId":"request_e","modelId":"copilot/x"}}"#
                .to_string(),
            // modelId 的字段级 set(needle 覆盖内, 末值生效)
            set_op(0, "modelId", "\"copilot/gpt-5\""),
            set_op(0, "completionTokens", "12"),
            // 同 requestId 重复推送: 文件内 first-wins
            push_op(
                r#"{"requestId":"request_e","modelId":"copilot/other","timestamp":1790700002000,"completionTokens":99}"#,
            ),
        ],
    );
    let entries = collect_from(&user).unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].model, "gpt-5");
    assert_eq!(entries[0].output_tokens, 12);
    fs::remove_dir_all(&user).ok();
}

#[test]
fn test_auto_resolution_and_fallback() {
    let user = temp_dir();
    write_session(
        &user,
        "ll22",
        "sess_auto",
        &[
            init_op("sess_auto", 1),
            // 快照推送即含解析片段: copilot/auto -> gpt-5.6-luna
            r#"{"kind":2,"k":["requests"],"i":null,"v":[{"requestId":"request_a","modelId":"copilot/auto","timestamp":1790800000000,"response":[{"kind":"autoModeResolution","resolved":{"id":"gpt-5.6-luna","name":"GPT-5.6 Luna"}},{"kind":"markdown"}]}]}"#
                .to_string(),
            set_op(0, "completionTokens", "10"),
            // 无解析片段的 auto: 回退 auto
            push_op(&request_obj("request_b", "copilot/auto", 1_790_800_100_000)),
            set_op(1, "completionTokens", "20"),
        ],
    );
    let mut entries = collect_from(&user).unwrap();
    entries.sort_by_key(|e| e.model.clone());
    let got: Vec<(&str, u64)> = entries
        .iter()
        .map(|e| (e.model.as_str(), e.output_tokens))
        .collect();
    assert_eq!(got, vec![("auto", 20), ("gpt-5.6-luna", 10)]);
    fs::remove_dir_all(&user).ok();
}

#[test]
fn test_auto_resolution_streamed_and_last_wins() {
    let user = temp_dir();
    write_session(
        &user,
        "mm33",
        "sess_auto_stream",
        &[
            init_op("sess_auto_stream", 1),
            push_op(&request_obj("request_s", "copilot/auto", 1_790_810_000_000)),
            set_op(0, "completionTokens", "5"),
            // 响应片段推送(流式): 经 autoModeResolution needle 到达
            r#"{"kind":2,"k":["requests",0,"response"],"v":[{"kind":"autoModeResolution","resolved":{"id":"mai-code-1.1-flash","name":"MAI"}}]}"#
                .to_string(),
            // 整表替换响应数组(防御分支): 末次解析为准
            r#"{"kind":1,"k":["requests",0,"response"],"v":[{"kind":"autoModeResolution","resolved":{"id":"gpt-5.6-luna","name":"Luna"}}]}"#
                .to_string(),
        ],
    );
    let entries = collect_from(&user).unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].model, "gpt-5.6-luna");
    assert_eq!(entries[0].output_tokens, 5);
    fs::remove_dir_all(&user).ok();
}

#[test]
fn test_auto_model_switch_between_requests() {
    let user = temp_dir();
    // 同一会话相邻两次 auto 请求解析到不同模型: 各自独立入账, 分开计价
    write_session(
        &user,
        "nn44",
        "sess_switch",
        &[
            init_op("sess_switch", 1),
            r#"{"kind":2,"k":["requests"],"i":null,"v":[{"requestId":"request_1","modelId":"copilot/auto","timestamp":1790900000000,"response":[{"kind":"autoModeResolution","resolved":{"id":"gpt-5.6-luna","name":"Luna"}}]}]}"#
                .to_string(),
            set_op(0, "completionTokens", "10"),
            r#"{"kind":2,"k":["requests"],"i":null,"v":[{"requestId":"request_2","modelId":"copilot/auto","timestamp":1790900001000,"response":[{"kind":"autoModeResolution","resolved":{"id":"mai-code-1.1-flash","name":"MAI"}}]}]}"#
                .to_string(),
            set_op(1, "completionTokens", "20"),
        ],
    );
    let mut entries = collect_from(&user).unwrap();
    entries.sort_by_key(|e| e.model.clone());
    let got: Vec<(&str, u64)> = entries
        .iter()
        .map(|e| (e.model.as_str(), e.output_tokens))
        .collect();
    // 切换即分流: 两条请求分别归入各自实际模型
    assert_eq!(got, vec![("gpt-5.6-luna", 10), ("mai-code-1.1-flash", 20)]);
    fs::remove_dir_all(&user).ok();
}

#[test]
fn test_empty_window_session_discovered() {
    let user = temp_dir();
    write_empty_window(
        &user,
        "sess_window",
        &[
            init_op("sess_window", 1),
            push_op(&request_obj(
                "request_w",
                "copilot/gpt-5",
                1_790_500_000_000,
            )),
            set_op(0, "completionTokens", "9"),
        ],
    );
    let entries = collect_from(&user).unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].session_id.as_deref(), Some("sess_window"));
    assert_eq!(entries[0].output_tokens, 9);
    fs::remove_dir_all(&user).ok();
}

#[test]
fn test_missing_root_and_empty_session_ok() {
    let user = temp_dir();
    // 数据根不存在: 空结果不报错
    assert!(collect_from(&user).unwrap().is_empty());
    // 只有 init 的空会话: 无条目
    write_session(&user, "ii99", "sess_empty", &[init_op("sess_empty", 1)]);
    assert!(collect_from(&user).unwrap().is_empty());
    fs::remove_dir_all(&user).ok();
}
