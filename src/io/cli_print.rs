//! Cli格式化输出
//!
use comfy_table::presets::UTF8_FULL;
use comfy_table::{Attribute, Cell, Table};
use serde_json::json;

use crate::model::TokenTotals;

const HEADERS: [&str; 7] = [
    "Key",
    "Requests",
    "Input",
    "Output",
    "Cache Read",
    "Cache Write",
    "Total",
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
    })
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
mod tests {
    use super::*;

    #[test]
    fn test_thousands() {
        assert_eq!(thousands(0), "0");
        assert_eq!(thousands(999), "999");
        assert_eq!(thousands(1_000), "1,000");
        assert_eq!(thousands(76_780_408), "76,780,408");
    }
}
