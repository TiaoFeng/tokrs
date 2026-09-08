# tokrs

**English** | [简体中文](README.md)

[![Latest Release](https://img.shields.io/github/v/release/TiaoFeng/tokrs)](https://github.com/TiaoFeng/tokrs/releases/latest)
[![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](LICENSE)
[![Rust](https://img.shields.io/badge/language-Rust-orange.svg)](https://www.rust-lang.org/)

A native token usage statistics CLI written in Rust. It directly reads the log and database files left by Claude Code, Codex, OpenCode, Gemini CLI, Grok Build, Pi, and Kimi Code on your local machine to track token consumption and costs by app, model, and date. It has no daemons, makes no network requests, and accesses data sources in read-only mode.

## Features

- Supports 7 agents: Claude / Codex / OpenCode / Gemini / Grok / Pi / Kimi
- Grouped statistics by three dimensions: app, model, and date
- Supports filtering by start and end dates (`--since` / `--until`)
- Four-tier priority for cost estimation:
  - Pricing table override > Upstream self-reported cost > Pricing table estimate > Unpriced (counted as a warning, not as 0)
- The `pricing.json` pricing table supports time-based pricing per model, long-context surcharges, and peak-hour surcharges
- During aggregation, automatically adds an empty template for missing models in the pricing table without overwriting existing entries
- Upstream costs included in the input token are already deducted at the parsing layer; “Total” is not double-counted
- Duplicate removal is performed independently for each data source; results remain consistent across repeated runs
- Supports `--json` machine-readable output
- Terminal output in UTF-8 tables, with thousands separators

## Supported Data Sources

| App | Data Location | Description |
|---|---|---|
| Claude | `~/.claude/projects/**/*.jsonl` | Dedupe by `message.id` |
| Codex | `~/.codex/{sessions/**,archived_sessions/*.jsonl}` | `token_count` events; in-file same-source/adjacent snapshot repeats zeroed, fork replays filtered via parent-chain prefix match, same-name archived copies deduplicated (longest wins) |
| OpenCode | `~/.local/share/opencode/opencode.db` | Read-only access to SQLite |
| Gemini | `~/.gemini/tmp/*/chats/session-*.json` | Single JSON object; skip corrupted files |
| Grok | `~/.grok/{sessions,archived_sessions}/**/updates.jsonl` | Check `turn_completed` value per round |
| Pi | `~/.pi/agent/sessions/*.jsonl` (can be overridden with `$PI_CODING_AGENT_SESSION_DIR`) | Deduped by `entry.id` / content hash |
| Kimi | `~/.kimi-code/sessions/**/agents/*/wire.jsonl` | `usage.record`: For each call, models are normalized uniformly (provider prefix stripped, lowercased; same for all apps), and duplicates are removed from the content signatures (fork copies are not counted twice) |


## Installation

Download the latest precompiled version:
[![Latest Release](https://img.shields.io/github/v/release/TiaoFeng/tokrs)](https://github.com/TiaoFeng/tokrs/releases/latest)

Or build from source (see the “Building from Source” section below).

## Quick Start (Example)

```
$ tokrs

--------------------------------------------------------------------------------------------------
 Key        Requests   Input        Output      Cache Read    Cache Write   Total         Cost    
==================================================================================================
 Today      128        18,432       96,510      1,204,881     88,000        1,407,823     $4.21   
--------------------------------------------------------------------------------------------------
 claude     3,412      1,204,553    2,891,004   88,312,440    6,501,220     98,909,217    $156.20 
--------------------------------------------------------------------------------------------------
 codex      1,208      601,220      1,100,322   40,122,884    0             41,824,426    $61.03  
--------------------------------------------------------------------------------------------------
 opencode   875        320,110      441,027     12,088,340    1,022,884     13,872,361    $21.55  
--------------------------------------------------------------------------------------------------
 gemini     642        220,481      310,224     18,204,112    0             18,734,817    $12.77* 
--------------------------------------------------------------------------------------------------
 Total      6,137      2,346,396    4,839,087   159,932,657   7,612,104     174,730,244   $251.55*
--------------------------------------------------------------------------------------------------
  * 64 request(s) unpriced, cost not counted

$ tokrs --by model --app claude --since 2026-09-01

-------------------------------------------------------------------------------------------------------------------
 Key                          Requests   Input       Output      Cache Read   Cache Write   Total       Cost    
===================================================================================================================
 Today                        128        18,432      96,510      1,204,881    88,000        1,407,823   $4.21   
-------------------------------------------------------------------------------------------------------------------
 claude/claude-sonnet-4-5     96         18,220      88,431      1,204,881    88,000        1,399,532   $4.19   
-------------------------------------------------------------------------------------------------------------------
 claude/claude-opus-4-6       32         212         7,941       0            0             8,153       $0.13   
-------------------------------------------------------------------------------------------------------------------
 Total                        128        18,432      96,510      1,204,881    88,000        1,407,823   $4.21 
-------------------------------------------------------------------------------------------------------------------
$ tokrs --by day

----------------------------------------------------------------------------------------------------
 Key          Requests   Input        Output      Cache Read    Cache Write   Total         Cost    
====================================================================================================
 Today        128        18,432       96,510      1,204,881     88,000        1,407,823     $4.21   
----------------------------------------------------------------------------------------------------
 2026-08-30   210        40,112       122,008     3,110,220     210,004       3,482,344     $9.88   
----------------------------------------------------------------------------------------------------
 2026-08-31   165        22,881       90,220      2,011,442     88,000        2,212,543     $6.42   
----------------------------------------------------------------------------------------------------
 2026-09-01   128        18,432       96,510      1,204,881     88,000        1,407,823     $4.21   
----------------------------------------------------------------------------------------------------

$ tokrs --json
{
  "rows": [
    {
      "key": "claude",
      "totals": {
        "requests": 3412,
        "input_tokens": 1204553,
        "output_tokens": 2891004,
        "cache_read_tokens": 88312440,
        "cache_creation_tokens": 6501220,
        "total_tokens": 98909217,
        "cost_usd": 156.2,
        "unpriced": 0
      }
    }
  ],
  "total": { "...": "..." },
  "today": { "...": "..." }
}
```

## CLI Command Description

tokrs has only one entry point; parameters are used to control the scope of the statistics and the output:

```
tokrs [--app <APPS>] [--by <GROUP>] [--since <DATE>] [--until <DATE>] [--json]
```

| Parameter | Description |
|---|---|
| `--app <a,b,c>` | Count only the specified apps (comma-separated); available values: `claude`, `codex`, `opencode`, `gemini`, `grok`, `pi`, `kimi`; by default, count all |
| `--by <GROUP>` | Grouping method: `app` (default) / `model` / `day` |
| `-s, --since <YYYY-MM-DD>` | Start date (inclusive), based on local time zone |
| `-u, --until <YYYY-MM-DD>` | End date (inclusive), based on local time zone |
| `--json` | Output in JSON format for easy processing by scripts |

Example:

```bash
tokrs                                # All apps, grouped by app
tokrs --by day                       # Grouped by date
tokrs --app claude,opencode          # Count only Claude and OpenCode
tokrs -s 2026-09-01 -u 2026-09-30    # Count for September
tokrs --by model --json              # Grouped by model and output as JSON
```

The first row of the output table, **Today**, shows the subtotal for the current day (regardless of whether date filtering is used), and the last row, **Total**, shows the grand total for the filtered range; entries in the Cost column marked with `*` indicate pending pricing requests (whose costs are not included in the totals).

## Pricing Table

> Cost Source Priority: **force (override) > self-reported costs from upstream > pricing table estimates > unpriced**.

- Self-reported costs currently come from the `cost` field in OpenCode, `costUsdTicks` in Grok (ignored when `costIsPartial=true`), and `usage.cost.total` in Pi;
- Requests with an empty pricing table (all `null` values) and no self-reported costs are classified as `unpriced`; they are counted but not billed.
- The pricing table is located at `~/.config/tokrs/pricing.json` (following `$XDG_CONFIG_HOME`).

When run for the first time, it automatically scans all existing models and adds `null` template entries (skipping the `unknown` catch-all name); simply enter the price to calculate the cost.

> The `./pricing.json` file in the repository is based on [opencode](https://opencode.ai/docs/zen/) sample pricing table and is provided for reference.
> If you need to use it, run the following command:
> ```mkdir -p ~/.config/tokrs && cp pricing.json ~/.config/tokrs/```

Examples (all unit prices are in **USD per million tokens**):

```json
{
  "version": 1,
  "models": {
    "deepseek-v4-flash": [
      {
        "since": "2026-01-01",
        "input": 0,
        "output": 0,
        "cache_read": 0,
        "cache_write": 0
      },
      {
        "since": "2026-08-01",
        "input": 0.14,
        "output": 0.28,
        "cache_read": 0.028,
        "cache_write": 0
      }
    ],
    "gpt-5.6-sol": [
      {
        "since": "2026-01-01",
        "input": 2.00,
        "output": 10.00,
        "cache_read": 0.20,
        "cache_write": 2.50,
        "long_context": {
          "above": 252000,
          "input": 4.00,
          "output": 15.00,
          "cache_read": 0.40,
          "cache_write": 5.00
        }
      }
    ],
    "deepseek-v4-pro": [
      {
        "input": 0.66,
        "output": 1.98,
        "cache_read": 0.022,
        "cache_write": 0,
        "peak": {
          "hours": [[9, 12], [14, 18]],
          "utc_offset": 8,
          "input": 1.32,
          "output": 3.96,
          "cache_read": 0.044,
          "cache_write": 0
        }
      }
    ]
  }
}
```

Field Descriptions:

| Field | Description |
|---|---|
| `version` | Price list format version; currently `1`; a mismatch results in an error |
| `models` | Model name -> list of price versions (array; may contain multiple time-based versions) |
| `since` | Effective date for this version (`YYYY-MM-DD`, local time zone); a missing value indicates it is always effective; for the same model, the latest entry where `since` ≤ the usage date is used |
| `force` | If `true` and a base price has been entered for this version, upstream self-reported costs are ignored, and pricing is always based on this table |
| `input` / `output` / `cache_read` / `cache_write` | Base unit price; if all are `null`, the price is considered unset, and no estimate is generated |
| `long_context` | Context-based surcharge: When `input + cache_read + cache_write >= above`, non-empty fields within the block override the base price |
| `peak` | Peak-hour surcharge: `hours` is a list of hour intervals `[start, end)` (`start > end` supports wrapping across midnight), and `utc_offset` is the time zone offset used to interpret the intervals; when the time period is hit, non-empty fields within the block override the base price |

Model Name Lookup Rules: **Exact matches take precedence, followed by the longest prefix matches** (prefix boundaries must consist of non-alphanumeric characters). For example, the key `gpt-5` matches `gpt-5-codex` and `gpt-5.1-2026`, but does not falsely match `gpt-51x`.

## Build from Source Code

> Rust ≥ 1.88

```bash
git clone https://github.com/TiaoFeng/tokrs.git
cd tokrs
cargo build --release
# Output: target/release/tokrs
```

## Project Structure

```
src/
├── main.rs           # Program entry point
├── commands.rs       # CLI argument parsing and command dispatching
├── model.rs          # Shared types (AppKind / UsageEntry / TokenTotals)
├── tokens.rs         # Core statistics: date filtering, grouping by app/model/day, total counts
├── error.rs          # AppError custom error type
├── apps/
│   ├── mod.rs        # Unified collection from various data sources + standardization of “fresh input” and “provider” prefixes
│   ├── claude.rs     # ~/.claude/projects/**/*.jsonl
│   ├── codex.rs      # ~/.codex/{sessions,archived_sessions}
│   ├── opencode.rs   # ~/.local/share/opencode/opencode.db (SQLite read-only)
│   ├── gemini.rs     # ~/.gemini/tmp/*/chats/session-*.json
│   ├── grok.rs       # ~/.grok/{sessions,archived_sessions}/**/updates.jsonl
│   ├── pi.rs         # ~/.pi/agent/sessions/*.jsonl
│   ├── kimi.rs       # ~/.kimi-code/sessions/**/agents/*/wire.jsonl
│   └── prince.rs     # pricing.json (version pricing / long context / peak hours / force)
├── io/
│   ├── load.rs       # JSON/JSONL decoding, timestamp normalization (epoch seconds)
│   └── cli_print.rs  # Terminal tables and JSON output
└── tests/            # Unit tests
```

## License

This project is licensed under the [MIT License](LICENSE).

## Statement and Acknowledgments

- The project is built using GLM-5.3-Flash and Qwen3.8 Flash
- Implementation reference: [cc-switch](https://github.com/farion1231/cc-switch)
- [opencode](https://github.com/anomalyco/opencode) provides excellent, open-source tools
- Translated with DeepL.com (free version)
