use std::collections::BTreeSet;

use chrono::NaiveDate;
use clap::{Parser, ValueEnum};

use crate::apps;
use crate::error::AppError;
use crate::io::cli_print;
use crate::model::{AppKind, TokenTotals};
use crate::tokens;

#[derive(Clone, Copy, Debug, ValueEnum)]
enum GroupBy {
    App,
    Model,
    Day,
}

#[derive(Clone, Copy, Debug, ValueEnum)]
enum AppArg {
    Claude,
    Codex,
    #[value(name = "opencode")]
    OpenCode,
}

impl AppArg {
    fn kind(self) -> AppKind {
        match self {
            AppArg::Claude => AppKind::Claude,
            AppArg::Codex => AppKind::Codex,
            AppArg::OpenCode => AppKind::OpenCode,
        }
    }
}

#[derive(Parser)]
#[command(name = "tokrs", about = "本地 Token 用量统计 CLI")]
pub struct Cli {
    #[arg(
        long,
        value_delimiter = ',',
        help = "统计的应用 (claude,codex,opencode)，默认全部"
    )]
    app: Vec<AppArg>,
    #[arg(long, value_enum, default_value_t = GroupBy::App, help = "分组维度")]
    by: GroupBy,
    #[arg(long, help = "起始日期 YYYY-MM-DD（含）")]
    since: Option<NaiveDate>,
    #[arg(long, help = "结束日期 YYYY-MM-DD（含）")]
    until: Option<NaiveDate>,
    #[arg(long, help = "以 JSON 格式输出")]
    json: bool,
}

pub fn run(cli: Cli) -> Result<(), AppError> {
    let app_kinds: Vec<AppKind> = if cli.app.is_empty() {
        AppKind::ALL.to_vec()
    } else {
        cli.app
            .iter()
            .map(|a| a.kind())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect()
    };

    let entries = apps::collect(&app_kinds)?;
    let entries = tokens::filter_by_range(entries, cli.since, cli.until);
    let total = tokens::grand_total(&entries);
    let today = tokens::today_total(&entries);

    let rows: Vec<(String, TokenTotals)> = match cli.by {
        GroupBy::App => tokens::aggregate_by_app(&entries)
            .into_iter()
            .map(|(app, totals)| (app.to_string(), totals))
            .collect(),
        GroupBy::Model => tokens::aggregate_by_model(&entries)
            .into_iter()
            .map(|((app, model), totals)| (format!("{app}/{model}"), totals))
            .collect(),
        GroupBy::Day => tokens::aggregate_by_day(&entries)
            .into_iter()
            .map(|(day, totals)| (day.format("%Y-%m-%d").to_string(), totals))
            .collect(),
    };

    if cli.json {
        cli_print::print_json(&rows, &total, &today);
    } else {
        cli_print::print_report(&rows, &total, &today);
    }
    Ok(())
}
