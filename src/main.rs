//! `pecu` — a Verus wallet for the terminal, built on the Verus Rust SDK.

mod cli;
mod cmd;
mod config;
mod explain;
mod failure;
mod keystore;
mod node;
mod payload;
mod ui;

use std::process::ExitCode;

use clap::Parser;

fn main() -> ExitCode {
    let cli = cli::Cli::parse();
    // Read before `dispatch` takes the `Cli` by value. Returning
    // `miette::Result` from `main` was the whole of issue #49: std's
    // `Termination` impl is what printed the failure, and it can no more see
    // `--json` than it can choose an exit code other than 1.
    let json = cli.globals.json;

    match cmd::dispatch(cli) {
        // The document a `--json` run built is printed here rather than where
        // it was built, so a command that prints one and *then* fails cannot
        // put two on stdout. See `failure::PENDING`.
        Ok(()) => {
            failure::flush();
            ExitCode::SUCCESS
        }
        Err(report) => failure::finish(report, json),
    }
}
