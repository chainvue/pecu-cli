//! Command dispatch.
//!
//! Commands land one milestone at a time. Everything not yet implemented
//! answers with [`NotYet`] rather than a panic or a silent no-op, so
//! `pecu --help` is an honest map of what this build can do.

pub mod airgap;
pub mod dev;
pub mod doctor;
pub mod id;
pub mod key;
pub mod send;
pub mod tx;
pub mod wallet;

use clap::CommandFactory;
use miette::Diagnostic;
use thiserror::Error;

use crate::cli::{Cli, Command, DevCommand, IdCommand, PlanCommand, TxCommand, WalletCommand};
use crate::config::Settings;
use crate::ui::Ui;

/// A command that exists in the tree but has not been built yet.
#[derive(Debug, Error, Diagnostic)]
#[error("`pecu {command}` is not implemented in this build")]
#[diagnostic(
    code(pecu::not_implemented),
    help("it lands in milestone {milestone} — see the status table in README.md")
)]
pub struct NotYet {
    command: String,
    milestone: &'static str,
}

impl NotYet {
    fn at(command: impl Into<String>, milestone: &'static str) -> miette::Report {
        Self {
            command: command.into(),
            milestone,
        }
        .into()
    }
}

pub fn dispatch(cli: Cli) -> miette::Result<()> {
    let ui = Ui::new(cli.globals.theme, cli.globals.json, cli.globals.explain);

    match &cli.command {
        Command::Dev { command } => match command {
            DevCommand::Ui => {
                dev::gallery(&ui);
                Ok(())
            }
        },

        Command::Completions { shell } => {
            let mut command = Cli::command();
            let name = command.get_name().to_string();
            clap_complete::generate(*shell, &mut command, name, &mut std::io::stdout());
            Ok(())
        }

        Command::Doctor => doctor::run(&ui, &Settings::resolve(&cli.globals)?),

        Command::Key { command } => key::run(
            &ui,
            &Settings::resolve(&cli.globals)?,
            &cli.globals,
            command,
        ),

        Command::Wallet { command } => {
            let settings = Settings::resolve(&cli.globals)?;
            match command {
                WalletCommand::Balance { target } => wallet::balance(
                    &ui,
                    &settings,
                    target.address.as_deref(),
                    target.key.as_deref(),
                ),
                WalletCommand::Utxos { target } => wallet::utxos(
                    &ui,
                    &settings,
                    target.address.as_deref(),
                    target.key.as_deref(),
                ),
            }
        }

        Command::Tx { command } => match command {
            TxCommand::Explain { input } => {
                tx::explain(&ui, &Settings::resolve(&cli.globals)?, input.as_deref())
            }
        },

        Command::Send(args) => {
            send::run(&ui, &Settings::resolve(&cli.globals)?, &cli.globals, args)
        }

        Command::Plan { command } => match command {
            PlanCommand::Send(args) => {
                airgap::plan_send(&ui, &Settings::resolve(&cli.globals)?, &cli.globals, args)
            }
        },

        Command::Sign(args) => {
            airgap::sign(&ui, &Settings::resolve(&cli.globals)?, &cli.globals, args)
        }
        Command::Broadcast(args) => {
            airgap::broadcast(&ui, &Settings::resolve(&cli.globals)?, &cli.globals, args)
        }

        Command::Id {
            command: IdCommand::Show { name },
        } => id::show(&ui, &Settings::resolve(&cli.globals)?, name),

        Command::Id {
            command: IdCommand::Register(args),
        } => id::register(&ui, &Settings::resolve(&cli.globals)?, &cli.globals, args),

        Command::Id { command } => Err(match command {
            IdCommand::Show { .. } | IdCommand::Register(_) => unreachable!("handled above"),
            IdCommand::Update => NotYet::at("id update", "M7"),
            IdCommand::Revoke => NotYet::at("id revoke", "M7"),
            IdCommand::Recover => NotYet::at("id recover", "M7"),
            IdCommand::Login => NotYet::at("id login", "M8"),
            IdCommand::Publish => NotYet::at("id publish", "M8"),
            IdCommand::Read => NotYet::at("id read", "M8"),
        }),
    }
}
