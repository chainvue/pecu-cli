//! Command dispatch.
//!
//! Commands land one milestone at a time. Everything not yet implemented
//! answers with [`NotYet`] rather than a panic or a silent no-op, so
//! `pecu --help` is an honest map of what this build can do.

pub mod dev;

use clap::CommandFactory;
use miette::Diagnostic;
use thiserror::Error;

use crate::cli::{
    Cli, Command, DevCommand, IdCommand, KeyCommand, PlanCommand, TxCommand, WalletCommand,
};
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
    let ui = Ui::new(cli.globals.theme, cli.globals.json);

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

        Command::Doctor => Err(NotYet::at("doctor", "M2")),

        Command::Key { command } => Err(NotYet::at(
            match command {
                KeyCommand::Gen => "key gen",
                KeyCommand::Import => "key import",
                KeyCommand::List => "key list",
                KeyCommand::Show => "key show",
                KeyCommand::Export => "key export",
                KeyCommand::Phrase => "key phrase",
            },
            "M3",
        )),

        Command::Wallet { command } => Err(NotYet::at(
            match command {
                WalletCommand::Balance => "wallet balance",
                WalletCommand::Utxos => "wallet utxos",
            },
            "M4",
        )),

        Command::Tx { command } => Err(NotYet::at(
            match command {
                TxCommand::Explain => "tx explain",
            },
            "M4",
        )),

        Command::Send => Err(NotYet::at("send", "M5")),

        Command::Plan { command } => Err(NotYet::at(
            match command {
                PlanCommand::Send => "plan send",
            },
            "M6",
        )),

        Command::Sign => Err(NotYet::at("sign", "M6")),
        Command::Broadcast => Err(NotYet::at("broadcast", "M6")),

        Command::Id { command } => Err(match command {
            IdCommand::Show => NotYet::at("id show", "M7"),
            IdCommand::Register => NotYet::at("id register", "M7"),
            IdCommand::Update => NotYet::at("id update", "M7"),
            IdCommand::Revoke => NotYet::at("id revoke", "M7"),
            IdCommand::Recover => NotYet::at("id recover", "M7"),
            IdCommand::Login => NotYet::at("id login", "M8"),
            IdCommand::Publish => NotYet::at("id publish", "M8"),
            IdCommand::Read => NotYet::at("id read", "M8"),
        }),
    }
}
