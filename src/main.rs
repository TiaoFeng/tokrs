//! 程序入口
//!
mod apps;
mod commands;
mod error;
mod io;
mod model;
mod tokens;

use clap::Parser;
use std::error::Error;

/// 程序入口
fn main() {
    if let Err(err) = commands::run(commands::Cli::parse()) {
        eprintln!(":( error: {err}");
        let mut source = err.source();
        while let Some(src) = source {
            eprintln!("Caused by: {src}");
            source = src.source()
        }
        std::process::exit(1);
    }
}
