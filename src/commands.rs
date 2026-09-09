//! 命令解析与运行入口
//!
//! 可以指定：
//! - app种类
//! - 按照day, app, model分组
//! - 按照起始日期统计
//! - 按照结束日期统计
//! - 输出json文件
//!
use chrono::NaiveDate;
use clap::{Parser, ValueEnum};
use std::collections::BTreeSet;

use crate::{
    apps,
    error::AppError,
    io::cli_print,
    model::{AppKind, TokenTotals, UsageEntry},
    tokens,
};

/// 分类方法枚举
#[derive(Clone, Copy, Debug, ValueEnum)]
enum GroupBy {
    App,
    Model,
    Day,
}

/// Cli命令结构体
#[derive(Parser)]
#[command(name = "tokrs", about = "Local Token Usage Statistics CLI")]
pub struct Cli {
    #[arg(
        long,
        value_delimiter = ',',
        ignore_case = true,
        help = "Applications of Statistics (Claude, Codex, OpenCode, Gemini, Grok, Pi, Kimi)—all by default"
    )]
    app: Vec<AppKind>,
    #[arg(
        long,
        value_enum,
        default_value_t = GroupBy::App,
        ignore_case = true,
        help = "Grouping"
    )]
    by: GroupBy,
    #[arg(long, short, help = "Start Date YYYY-MM-DD (inclusive)")]
    since: Option<NaiveDate>,
    #[arg(long, short, help = "End Date YYYY-MM-DD (inclusive)")]
    until: Option<NaiveDate>,
    #[arg(long, help = "Output in JSON format")]
    json: bool,
}

/// cli运行函数
pub fn run(cli: Cli) -> Result<(), AppError> {
    let app_kinds: Vec<AppKind> = if cli.app.is_empty() {
        AppKind::ALL.to_vec()
    } else {
        cli.app
            .iter()
            .copied()
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect()
    };

    let mut entries = apps::collect(&app_kinds)?;
    // 价目表: 损坏直接报错不回写; 未覆盖模型自动追加 null 模板(用全量模型, 先于日期过滤)
    let pricing_path = apps::prince::pricing_path()?;
    let mut table = apps::prince::load_pricing(&pricing_path)?;
    let added = apps::prince::sync_models(&mut table, &pricing_path, &entries)?;
    if added > 0 {
        eprintln!(
            "> added {added} model(s) to {} (prices null until filled in)",
            pricing_path.display()
        );
    }
    apps::prince::resolve(&mut entries, &table);
    let entries = tokens::filter_by_range(entries, cli.since, cli.until);
    let total = tokens::grand_total(&entries);
    let today = tokens::today_total(&entries);
    let rows = build_rows(&entries, cli.by);

    if cli.json {
        cli_print::print_json(&rows, &total, &today);
    } else {
        cli_print::print_report(&rows, &total, &today);
    }
    Ok(())
}

/// 按分组维度聚合出报表行(纯函数, 便于单测覆盖分组分发)
fn build_rows(entries: &[UsageEntry], by: GroupBy) -> Vec<(String, TokenTotals)> {
    match by {
        GroupBy::App => tokens::aggregate_by_app(entries)
            .into_iter()
            .map(|(app, totals)| (app.to_string(), totals))
            .collect(),
        GroupBy::Model => tokens::aggregate_by_model(entries)
            .into_iter()
            .map(|((app, model), totals)| (format!("{app}/{model}"), totals))
            .collect(),
        GroupBy::Day => tokens::aggregate_by_day(entries)
            .into_iter()
            .map(|(day, totals)| (day.format("%Y-%m-%d").to_string(), totals))
            .collect(),
    }
}

#[cfg(test)]
#[path = "tests/commands_test.rs"]
mod tests;
