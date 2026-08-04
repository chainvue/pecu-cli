//! The command surface.
//!
//! Every command in this tree exists to show off one part of the Verus Rust
//! SDK, so the tree is organised the way the SDK is: keys, then reading, then
//! spending, then identities.

use clap::{Args, Parser, Subcommand, ValueEnum};

/// The `verus-sdk` commit this binary is built against, as a literal so it can
/// go through `concat!` into clap's `long_version`. It must match the `rev` in
/// `Cargo.toml`; `pecu --version` and `pecu doctor` both report it, because
/// "which SDK is this?" is the first question when something on chain disagrees.
macro_rules! sdk_rev {
    () => {
        "ae08bc0c806747c088104c003feee9b01171dd05"
    };
}
pub(crate) use sdk_rev;

const ABOUT: &str = "A Verus wallet for the terminal — the example app for the Verus Rust SDK";

const LONG_ABOUT: &str = "\
pecu is a command-line Verus wallet built on the Verus Rust SDK.

Keys are generated, stored and used locally; the public node is only ever asked
questions and handed finished transaction bytes. It defaults to the VRSCTEST
testnet at https://api.verustest.net.

Pass --explain to any command to see the exact verus-sdk calls it makes.";

#[derive(Debug, Parser)]
#[command(
    name = "pecu",
    version,
    long_version = concat!(env!("CARGO_PKG_VERSION"), "\nverus-sdk rev ", sdk_rev!()),
    about = ABOUT,
    long_about = LONG_ABOUT,
    propagate_version = true,
    arg_required_else_help = true
)]
pub struct Cli {
    #[command(flatten)]
    pub globals: Globals,

    #[command(subcommand)]
    pub command: Command,
}

/// Flags every command understands.
#[derive(Debug, Clone, Args)]
pub struct Globals {
    /// Emit machine-readable JSON instead of the rendered output
    #[arg(long, global = true)]
    pub json: bool,

    /// Build and sign, but stop short of broadcasting
    #[arg(long, global = true)]
    pub dry_run: bool,

    /// Print the verus-sdk calls this command makes
    #[arg(long, global = true)]
    pub explain: bool,

    /// Answer yes to every confirmation prompt
    #[arg(long, short = 'y', global = true)]
    pub yes: bool,

    /// Configuration profile to use
    #[arg(long, global = true, value_name = "NAME", env = "PECU_PROFILE")]
    pub profile: Option<String>,

    /// Override the node endpoint for this invocation
    #[arg(long, global = true, value_name = "URL", env = "VERUS_ENDPOINT")]
    pub node: Option<String>,

    /// Visual theme
    #[arg(long, global = true, value_enum, default_value_t = Theme::Auto)]
    pub theme: Theme,

    /// Log more; repeat for more still
    #[arg(long, short = 'v', global = true, action = clap::ArgAction::Count)]
    pub verbose: u8,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum Theme {
    /// Phosphor on a terminal, plain when piped
    Auto,
    /// Green-on-black frames and glyphs
    Phosphor,
    /// No colour, no box drawing
    Plain,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Check the node, the config and this build
    Doctor,

    /// Generate, import and inspect keys
    Key {
        #[command(subcommand)]
        command: KeyCommand,
    },

    /// What an address holds
    Wallet {
        #[command(subcommand)]
        command: WalletCommand,
    },

    /// Read transactions
    Tx {
        #[command(subcommand)]
        command: TxCommand,
    },

    /// Send funds
    Send,

    /// Build an unsigned transaction without touching a key
    Plan {
        #[command(subcommand)]
        command: PlanCommand,
    },

    /// Sign a plan offline
    Sign,

    /// Hand finished bytes to the node
    Broadcast,

    /// The VerusID lifecycle
    Id {
        #[command(subcommand)]
        command: IdCommand,
    },

    /// Print a shell completion script
    Completions {
        /// Shell to generate for
        #[arg(value_enum)]
        shell: clap_complete::Shell,
    },

    /// Helpers for working on pecu itself
    #[command(hide = true)]
    Dev {
        #[command(subcommand)]
        command: DevCommand,
    },
}

#[derive(Debug, Subcommand)]
pub enum DevCommand {
    /// Render every widget in the UI kit
    Ui,
}

#[derive(Debug, Subcommand)]
pub enum KeyCommand {
    /// Create a new key in the encrypted keystore
    Gen {
        /// Name to file it under
        #[arg(long, short = 'l', default_value = "default")]
        label: String,

        /// Back the key with a 24-word recovery phrase, so it can be typed back
        /// in if the keystore is lost
        #[arg(long)]
        from_phrase: bool,

        /// Print the recovery phrase. It is shown once and never stored
        #[arg(long, requires = "from_phrase")]
        show_phrase: bool,
    },

    /// Import an existing key, read from a prompt rather than the command line
    Import {
        /// Name to file it under
        #[arg(long, short = 'l')]
        label: String,

        /// Read a 24-word recovery phrase instead of a WIF
        #[arg(long)]
        phrase: bool,
    },

    /// List stored keys
    List,

    /// Show one key's public details
    Show {
        /// Which key
        label: String,
    },

    /// Print a stored private key in the clear
    Export {
        /// Which key
        label: String,
    },

    /// Generate a recovery phrase and show what it maps to, storing nothing
    Phrase,
}

#[derive(Debug, Subcommand)]
pub enum WalletCommand {
    /// Spendable, immature and token balances
    Balance,
    /// The individual outputs behind the balance
    Utxos,
}

#[derive(Debug, Subcommand)]
pub enum TxCommand {
    /// Say what every output in a transaction actually is
    Explain,
}

#[derive(Debug, Subcommand)]
pub enum PlanCommand {
    /// Plan a payment from a watch-only address
    Send,
}

#[derive(Debug, Subcommand)]
pub enum IdCommand {
    /// Read an identity off the chain
    Show,
    /// Register a new VerusID
    Register,
    /// Republish an identity with changes
    Update,
    /// Revoke an identity
    Revoke,
    /// Recover a revoked identity
    Recover,
    /// Prove control of an identity by signing a challenge
    Login,
    /// Publish VDXF data under an identity
    Publish,
    /// Read VDXF data from an identity
    Read,
}
