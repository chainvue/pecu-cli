//! `pecu` — a Verus wallet for the terminal, built on the Verus Rust SDK.

mod cli;
mod cmd;
mod ui;

use clap::Parser;

fn main() -> miette::Result<()> {
    let cli = cli::Cli::parse();
    cmd::dispatch(cli)
}
