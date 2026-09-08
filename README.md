# tokrs

**简体中文** | [English](README.en.md)

[![Latest Release](https://img.shields.io/github/v/release/TiaoFeng/tokrs)](https://github.com/TiaoFeng/tokrs/releases/latest)
[![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](LICENSE)
[![Rust](https://img.shields.io/badge/language-Rust-orange.svg)](https://www.rust-lang.org/)

一个使用 Rust 编写的本地 Token 用量统计 CLI。直接读取 Claude Code、Codex、OpenCode、Gemini CLI、Grok Build、Pi、Kimi Code 在本地留下的日志/数据库文件，统计各 app、各模型、各日期的 Token 消耗与成本。无任何守护进程、无任何网络请求、对数据源只读。

## 特性

- 支持 7 种 agent：Claude / Codex / OpenCode / Gemini / Grok / Pi / Kimi
- 按 app、模型、日期三种维度分组统计
- 支持按起止日期（`--since` / `--until`）筛选
- 成本估算四级优先：
  - 定价表 force 覆盖 > 上游自报成本 > 定价表估价 > unpriced（计数提示，不计为 0）
- 定价表 `pricing.json` 支持每模型时间版本价、长上下文加价、峰时加价
- 统计时自动为定价表中缺失的模型追加空模板，不覆盖已有条目
- 缓存包含于输入 Token 的上游已在解析层扣除，Total 无重复计算
- 各数据源独立去重，重复运行结果稳定
- 支持 `--json` 机器可读输出
- 终端 UTF-8 表格输出，千分位分隔

## 支持的数据源

| App | 数据位置 | 说明 |
|---|---|---|
| Claude | `~/.claude/projects/**/*.jsonl` | 按 `message.id` 去重 |
| Codex | `~/.codex/{sessions/**,archived_sessions/*.jsonl}` | `token_count` 事件；文件内同源快照/紧邻重复判零，fork 回放按父链前缀过滤，archived 同名副本保留最长 |
| OpenCode | `~/.local/share/opencode/opencode.db` | SQLite 只读访问 |
| Gemini | `~/.gemini/tmp/*/chats/session-*.json` | 单 JSON 对象，损坏文件跳过 |
| Grok | `~/.grok/{sessions,archived_sessions}/**/updates.jsonl` | 逐轮 `turn_completed` 面值 |
| Pi | `~/.pi/agent/sessions/*.jsonl`（可用 `$PI_CODING_AGENT_SESSION_DIR` 覆盖） | 按 entry.id / 内容哈希去重 |
| Kimi | `~/.kimi-code/sessions/**/agents/*/wire.jsonl` | `usage.record` 每调用面值，model 剥 provider 前缀归一（与 codex 同款），内容签名去重（fork 副本不双算） |

## 安装

下载预编译的最新版本：
[![Latest Release](https://img.shields.io/github/v/release/TiaoFeng/tokrs)](https://github.com/TiaoFeng/tokrs/releases/latest)

或从源码构建（见下方「从源代码构建」章节）。

## 快速开始（示例）

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

## CLI 命令说明

tokrs 只有一个入口，通过参数控制统计范围与输出：

```
tokrs [--app <APPS>] [--by <GROUP>] [--since <DATE>] [--until <DATE>] [--json]
```

| 参数 | 说明 |
|---|---|
| `--app <a,b,c>` | 只统计指定 app，逗号分隔，可选值：`claude` `codex` `opencode` `gemini` `grok` `pi` `kimi`；缺省统计全部 |
| `--by <GROUP>` | 分组方式：`app`（默认）/ `model` / `day` |
| `-s, --since <YYYY-MM-DD>` | 起始日期（含），按本地时区 |
| `-u, --until <YYYY-MM-DD>` | 结束日期（含），按本地时区 |
| `--json` | 以 JSON 输出，便于脚本二次处理 |

示例：

```bash
tokrs                                # 全部 app，按 app 分组
tokrs --by day                       # 按日期分组
tokrs --app claude,opencode          # 只统计 Claude 与 OpenCode
tokrs -s 2026-09-01 -u 2026-09-30    # 统计 9 月份
tokrs --by model --json              # 按模型分组并输出 JSON
```

输出表格首行 **Today** 为当天小计（无论是否使用日期筛选），末行 **Total** 为筛选范围内总计；Cost 列带 `*` 表示存在未定价请求（其成本未计入合计）。

## 定价表

> 成本来源优先级：**force 强制覆盖 > 上游自报成本 > 定价表估价 > unpriced**。

- 自报成本目前来自 OpenCode 的 `cost` 字段、Grok 的 `costUsdTicks`（`costIsPartial=true` 时不采信）、Pi 的 `usage.cost.total`；
- 定价表未填（全 `null`）且无自报成本的请求计入 `unpriced`，只计数不计价。
- 定价表位于 `~/.config/tokrs/pricing.json`（遵循 `$XDG_CONFIG_HOME`）。

> 仓库中`./pricing.json`是根据[opencode](https://opencode.ai/docs/zen/)的价格表，以供参考。
> 如果需要使用，可执行命令：
> ```mkdir -p ~/.config/tokrs && cp pricing.json ~/.config/tokrs/```

首次运行时自动扫描所有出现过的模型并追加 `null` 模板条目（跳过 `unknown` 兜底名），填好价格即可计价。

示例（单价均为 **USD / 百万 token**）：

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

字段说明：

| 字段 | 说明 |
|---|---|
| `version` | 价目表格式版本，当前为 `1`，不匹配直接报错 |
| `models` | 模型名 -> 价格版本列表（数组，可含多个时间版本） |
| `since` | 该版本的生效日期（`YYYY-MM-DD`，本地时区），缺省表示始终生效；同一模型取「`since` <= 使用日期」中最新的一条 |
| `force` | `true` 且该版本已填基础价时，忽略上游自报成本，一律按本表计价 |
| `input` / `output` / `cache_read` / `cache_write` | 基础单价；全为 `null` 视为未填价，不产生估价 |
| `long_context` | 上下文加价：当 `input + cache_read + cache_write >= above` 时，块内非空字段覆盖基础价 |
| `peak` | 峰时加价：`hours` 为 `[start, end)` 小时区间列表（`start > end` 支持跨午夜回绕），`utc_offset` 为解释区间所用的时区偏移；命中时段时块内非空字段覆盖基础价 |

模型名查找规则：**精确匹配优先，其次最长前缀匹配**（前缀边界须为非字母数字字符）。例如键 `gpt-5` 可匹配 `gpt-5-codex`、`gpt-5.1-2026`，但不会误配 `gpt-51x`。

## 从源代码构建

> Rust ≥ 1.88

```bash
git clone https://github.com/TiaoFeng/tokrs.git
cd tokrs
cargo build --release
# 产物: target/release/tokrs
```

## 项目结构

```
src/
├── main.rs           # 程序入口
├── commands.rs       # CLI 参数解析与命令分发
├── model.rs          # 共享类型（AppKind / UsageEntry / TokenTotals）
├── tokens.rs         # 统计核心：日期筛选、按 app/model/day 分组、总量
├── error.rs          # AppError 自定义错误类型
├── apps/
│   ├── mod.rs        # 各数据源统一收集 + fresh input / provider 前缀归一
│   ├── claude.rs     # ~/.claude/projects/**/*.jsonl
│   ├── codex.rs      # ~/.codex/{sessions,archived_sessions}
│   ├── opencode.rs   # ~/.local/share/opencode/opencode.db（SQLite 只读）
│   ├── gemini.rs     # ~/.gemini/tmp/*/chats/session-*.json
│   ├── grok.rs       # ~/.grok/{sessions,archived_sessions}/**/updates.jsonl
│   ├── pi.rs         # ~/.pi/agent/sessions/*.jsonl
│   ├── kimi.rs       # ~/.kimi-code/sessions/**/agents/*/wire.jsonl
│   └── prince.rs     # pricing.json 定价（版本价 / 长上下文 / 峰时 / force）
├── io/
│   ├── load.rs       # JSON/JSONL 解码、时间戳归一（epoch 秒）
│   └── cli_print.rs  # 终端表格与 JSON 输出
└── tests/            # 单元测试
```

## License

本项目使用 [MIT License](LICENSE)。

## 声明与鸣谢

- 项目由 GLM-5.3-Flash、Qwen3.8 Flash 构建
- 实现参考 [cc-switch](https://github.com/farion1231/cc-switch)
- [opencode](https://github.com/anomalyco/opencode) 提供优秀、开源的工具
