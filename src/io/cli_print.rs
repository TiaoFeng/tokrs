//! Cli格式化输出
//!
use comfy_table::presets::UTF8_FULL;
use comfy_table::{Attribute, Cell, Table};
use serde_json::json;

use crate::model::TokenTotals;

const HEADERS: [&str; 8] = [
    "Key",
    "Requests",
    "Input",
    "Output",
    "Cache Read",
    "Cache Write",
    "Total",
    "Cost",
];

pub fn print_report(rows: &[(String, TokenTotals)], total: &TokenTotals, today: &TokenTotals) {
    if rows.is_empty() {
        println!(">_ No usage data found.");
        return;
    }
    let mut table = Table::new();
    table.load_style(UTF8_FULL);
    table.set_header(HEADERS);
    table.add_row(bold_row("Today", today));
    for (key, totals) in rows {
        table.add_row(row_cells(key, totals));
    }
    table.add_row(bold_row("Total", total));
    println!("{}", table);
    if total.unpriced > 0 {
        println!(
            "  * {} request(s) unpriced, cost not counted",
            total.unpriced
        );
    }
}

fn bold_row(key: &str, totals: &TokenTotals) -> Vec<Cell> {
    row_cells(key, totals)
        .into_iter()
        .map(|c| Cell::new(c).add_attribute(Attribute::Bold))
        .collect()
}

pub fn print_json(rows: &[(String, TokenTotals)], total: &TokenTotals, today: &TokenTotals) {
    let doc = json!({
        "rows": rows
            .iter()
            .map(|(key, totals)| json!({ "key": key, "totals": totals_json(totals) }))
            .collect::<Vec<_>>(),
        "total": totals_json(total),
        "today": totals_json(today),
    });
    println!("{}", serde_json::to_string_pretty(&doc).unwrap_or_default());
}

fn totals_json(totals: &TokenTotals) -> serde_json::Value {
    json!({
        "requests": totals.requests,
        "input_tokens": totals.input_tokens,
        "output_tokens": totals.output_tokens,
        "cache_read_tokens": totals.cache_read_tokens,
        "cache_creation_tokens": totals.cache_creation_tokens,
        "total_tokens": totals.total_tokens(),
        "cost_usd": totals.cost_usd,
        "unpriced": totals.unpriced,
    })
}

/// 成本单元格: 全部无价显示 "-", 有价显示美元(小额 4 位小数), 部分无价加 "*"
fn cost_text(totals: &TokenTotals) -> String {
    if totals.cost_usd == 0.0 && totals.unpriced > 0 {
        return "-".to_string();
    }
    let s = fmt_usd(totals.cost_usd);
    if totals.unpriced > 0 {
        format!("{s}*")
    } else {
        s
    }
}

fn fmt_usd(cost: f64) -> String {
    if cost > 0.0 && cost < 0.01 {
        format!("${cost:.4}")
    } else {
        format!("${cost:.2}")
    }
}

fn row_cells(key: &str, totals: &TokenTotals) -> Vec<String> {
    vec![
        key.to_string(),
        thousands(totals.requests),
        thousands(totals.input_tokens),
        thousands(totals.output_tokens),
        thousands(totals.cache_read_tokens),
        thousands(totals.cache_creation_tokens),
        thousands(totals.total_tokens()),
        cost_text(totals),
    ]
}

fn thousands(n: u64) -> String {
    let digits = n.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (idx, b) in digits.bytes().enumerate() {
        if idx > 0 && (digits.len() - idx).is_multiple_of(3) {
            out.push(',');
        }
        out.push(char::from(b));
    }
    out
}

#[cfg(test)]
#[path = "../tests/io_cli_print_test.rs"]
mod tests;
