use std::collections::HashSet;
use std::path::Path;

use serde_json::Value;

use crate::error::AppError;
use crate::io::load;
use crate::model::{AppKind, UsageEntry};

const SESSIONS_MAX_DEPTH: usize = 3;

pub fn collect() -> Result<Vec<UsageEntry>, AppError> {
    let base = load::home_dir()?.join(".codex");
    if !base.is_dir() {
        return Ok(Vec::new());
    }
    collect_from(&base)
}

pub fn collect_from(base: &Path) -> Result<Vec<UsageEntry>, AppError> {
    let mut files = load::discover_files(&base.join("sessions"), "jsonl", SESSIONS_MAX_DEPTH);
    files.extend(load::discover_files(
        &base.join("archived_sessions"),
        "jsonl",
        0,
    ));
    files.retain(|f| is_rollout_filename(f));
    files.sort();
    files.dedup();

    let mut seen_signatures: HashSet<Signature> = HashSet::new();
    let mut entries = Vec::new();
    for file in files {
        parse_file(&file, &mut entries, &mut seen_signatures)?;
    }
    Ok(entries)
}

fn is_rollout_filename(path: &Path) -> bool {
    path.file_name()
        .and_then(|n| n.to_str())
        .is_some_and(|name| name.starts_with("rollout-") && name.ends_with(".jsonl"))
}

#[derive(Clone, Copy, Default, PartialEq, Eq, Hash)]
struct Signature([u64; 12]);

#[derive(Clone, Copy, Default)]
struct Counters {
    input: u64,
    cached: u64,
    cache_write: u64,
    output: u64,
    reasoning: u64,
    total: u64,
}

impl Counters {
    fn parse(info: &Value, key: &str) -> Option<Self> {
        let obj = info.get(key)?.as_object()?;
        Some(Self {
            input: obj.get("input_tokens").and_then(Value::as_u64).unwrap_or(0),
            cached: obj
                .get("cached_input_tokens")
                .and_then(Value::as_u64)
                .or_else(|| obj.get("cache_read_input_tokens").and_then(Value::as_u64))
                .unwrap_or(0),
            cache_write: obj
                .get("cache_write_input_tokens")
                .and_then(Value::as_u64)
                .unwrap_or(0),
            output: obj
                .get("output_tokens")
                .and_then(Value::as_u64)
                .unwrap_or(0),
            reasoning: obj
                .get("reasoning_output_tokens")
                .and_then(Value::as_u64)
                .unwrap_or(0),
            total: obj.get("total_tokens").and_then(Value::as_u64).unwrap_or(0),
        })
    }

    fn billable(&self) -> [u64; 4] {
        [self.input, self.cached, self.cache_write, self.output]
    }

    fn fields(self) -> [u64; 6] {
        [
            self.input,
            self.cached,
            self.cache_write,
            self.output,
            self.reasoning,
            self.total,
        ]
    }
}

fn signature(total: Option<Counters>, last: Option<Counters>) -> Signature {
    let mut fields = [0u64; 12];
    if let Some(t) = total {
        fields[..6].copy_from_slice(&t.fields());
    }
    if let Some(l) = last {
        fields[6..].copy_from_slice(&l.fields());
    }
    Signature(fields)
}

struct FileState {
    thread_id: Option<String>,
    model: Option<String>,
    high_water: Option<[u64; 4]>,
}

fn parse_file(
    file: &Path,
    entries: &mut Vec<UsageEntry>,
    seen: &mut HashSet<Signature>,
) -> Result<(), AppError> {
    let mut state = FileState {
        thread_id: None,
        model: None,
        high_water: None,
    };
    for line in load::read_jsonl(file)? {
        match load::str_get(&line, &["type"]) {
            Some("session_meta") => {
                if state.thread_id.is_none() {
                    state.thread_id = ["id", "thread_id", "threadId", "session_id"]
                        .iter()
                        .find_map(|k| load::str_get(&line, &["payload", k]))
                        .map(str::to_string);
                }
            }
            Some("turn_context") => {
                if let Some(m) = load::str_get(&line, &["payload", "model"])
                    .or_else(|| load::str_get(&line, &["payload", "info", "model"]))
                {
                    state.model = Some(normalize_model(m));
                }
            }
            Some("event_msg") => parse_token_count(&line, &mut state, entries, seen),
            _ => {}
        }
    }
    Ok(())
}

fn parse_token_count(
    line: &Value,
    state: &mut FileState,
    entries: &mut Vec<UsageEntry>,
    seen: &mut HashSet<Signature>,
) {
    if load::str_get(line, &["payload", "type"]) != Some("token_count") {
        return;
    }
    let Some(info) = line.pointer("/payload/info") else {
        return;
    };
    let total = Counters::parse(info, "total_token_usage");
    let last = Counters::parse(info, "last_token_usage");
    if total.is_none() && last.is_none() {
        return;
    }

    let [d_input, d_cached, d_write, d_output] = match last {
        Some(l) => l.billable(),
        None => delta_from_total(state, &total.unwrap_or_default()),
    };

    if !seen.insert(signature(total, last)) {
        return;
    }
    let d_cached = d_cached.min(d_input);
    if d_input == 0 && d_cached == 0 && d_write == 0 && d_output == 0 {
        return;
    }

    let model = load::str_get(info, &["model"])
        .or_else(|| load::str_get(info, &["model_name"]))
        .map(normalize_model)
        .or_else(|| state.model.clone())
        .unwrap_or_else(|| "unknown".to_string());
    let created_at = line
        .get("timestamp")
        .and_then(load::timestamp_to_epoch)
        .unwrap_or_else(load::now_epoch);

    entries.push(UsageEntry {
        app: AppKind::Codex,
        model,
        session_id: state.thread_id.clone(),
        created_at,
        input_tokens: d_input,
        output_tokens: d_output,
        cache_read_tokens: d_cached,
        cache_creation_tokens: d_write,
    });
}

fn delta_from_total(state: &mut FileState, counters: &Counters) -> [u64; 4] {
    let current = counters.billable();
    let base = *state.high_water.get_or_insert([0; 4]);
    state.high_water = Some(std::array::from_fn(|i| base[i].max(current[i])));
    std::array::from_fn(|i| current[i].saturating_sub(base[i]))
}

fn normalize_model(raw: &str) -> String {
    let mut s = raw.trim().to_ascii_lowercase();
    if let Some(pos) = s.rfind('/') {
        s = s[pos + 1..].to_string();
    }
    strip_date_suffix(&s).to_string()
}

fn strip_date_suffix(s: &str) -> &str {
    let bytes = s.as_bytes();
    let all_digits = |b: &[u8]| b.iter().all(u8::is_ascii_digit);
    if bytes.len() >= 11
        && bytes[bytes.len() - 11] == b'-'
        && bytes[bytes.len() - 6] == b'-'
        && bytes[bytes.len() - 3] == b'-'
        && all_digits(&bytes[bytes.len() - 10..bytes.len() - 6])
        && all_digits(&bytes[bytes.len() - 5..bytes.len() - 3])
        && all_digits(&bytes[bytes.len() - 2..])
    {
        return &s[..s.len() - 11];
    }
    if bytes.len() >= 9 && bytes[bytes.len() - 9] == b'-' && all_digits(&bytes[bytes.len() - 8..]) {
        return &s[..s.len() - 9];
    }
    s
}

#[cfg(test)]
mod tests {
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
        assert_eq!(
            (
                entries[0].input_tokens,
                entries[0].cache_read_tokens,
                entries[0].output_tokens
            ),
            (100, 50, 10)
        );
        assert_eq!(
            (
                entries[1].input_tokens,
                entries[1].cache_read_tokens,
                entries[1].output_tokens
            ),
            (200, 100, 20)
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
        assert_eq!(
            (
                entries[1].input_tokens,
                entries[1].cache_read_tokens,
                entries[1].output_tokens
            ),
            (50, 10, 10)
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
        assert_eq!(entries[0].input_tokens, 100);
        assert_eq!(entries[1].input_tokens, 50);
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
        assert_eq!(entries[0].input_tokens, 10);
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
        assert_eq!(entries[1].input_tokens, 50);
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
}
