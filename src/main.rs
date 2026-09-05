mod apps;
mod commands;
mod error;
mod io;
mod model;
mod tokens;

use clap::Parser;

fn main() {
    if let Err(err) = commands::run(commands::Cli::parse()) {
        eprintln!("error: {err}");
        std::process::exit(1);
    }
}
