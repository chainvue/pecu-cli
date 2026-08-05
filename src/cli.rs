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
        "4044fb1ed47c35c33918921b94ec792286599357"
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
    Send(SendArgs),

    /// Build an unsigned transaction without touching a key
    Plan {
        #[command(subcommand)]
        command: PlanCommand,
    },

    /// Sign a plan offline. Opens no socket
    Sign(SignArgs),

    /// Hand finished bytes to the node
    Broadcast(BroadcastArgs),

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

/// A transparent payment.
#[derive(Debug, Clone, Args)]
pub struct SendArgs {
    /// Who gets paid: an address, or a VerusID name like `bob@`
    #[arg(long, short = 't', value_name = "ADDRESS|NAME@")]
    pub to: String,

    /// How much, in coins. At most eight decimal places, and parsed exactly —
    /// no float is constructed at any point
    #[arg(long, short = 'm', value_name = "COINS")]
    pub amount: String,

    /// Which stored key pays. Defaults to the only one, if there is only one
    #[arg(long, short = 'f', value_name = "LABEL")]
    pub from: Option<String>,

    /// Send a token instead of the chain's own currency
    #[arg(long, short = 'c', value_name = "NAME@|i-ADDRESS")]
    pub currency: Option<String>,
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
    Balance {
        #[command(flatten)]
        target: WalletTarget,
    },
    /// The individual outputs behind the balance
    Utxos {
        #[command(flatten)]
        target: WalletTarget,
    },
}

/// Which address to look at. Read-only commands take an address, never a key —
/// watching a balance needs no secret.
#[derive(Debug, Clone, Args)]
#[group(multiple = false)]
pub struct WalletTarget {
    /// Look at this address
    #[arg(long, short = 'a', value_name = "R…")]
    pub address: Option<String>,

    /// Look at the address of a stored key
    #[arg(long, short = 'k', value_name = "LABEL")]
    pub key: Option<String>,
}

#[derive(Debug, Subcommand)]
pub enum TxCommand {
    /// Say what every output in a transaction actually is
    ///
    /// Takes a txid, a raw transaction as hex, or a bare output script as hex.
    /// Only a txid needs a node; the decoding is offline either way.
    Explain {
        /// txid, raw hex, or `-` to read hex on stdin
        #[arg(value_name = "TXID|HEX")]
        input: Option<String>,
    },
}

#[derive(Debug, Subcommand)]
pub enum PlanCommand {
    /// Plan a payment from a watch-only address
    Send(PlanSendArgs),
}

/// A payment planned without a key.
#[derive(Debug, Clone, Args)]
pub struct PlanSendArgs {
    /// Who gets paid
    #[arg(long, short = 't', value_name = "ADDRESS")]
    pub to: String,

    /// How much, in coins
    #[arg(long, short = 'm', value_name = "COINS")]
    pub amount: String,

    /// Which address pays. A stored key contributes its address only — this
    /// step never unlocks anything
    #[command(flatten)]
    pub target: WalletTarget,

    /// Also write the plan here
    #[arg(long, short = 'o', value_name = "FILE")]
    pub out: Option<std::path::PathBuf>,

    #[command(flatten)]
    pub qr: QrOut,
}

#[derive(Debug, Clone, Args)]
pub struct SignArgs {
    /// The plan: hex, `@file`, or `-` for stdin
    #[arg(value_name = "HEX|@FILE")]
    pub input: Option<String>,

    /// Read the plan from QR codes in a PNG instead. Repeat for several frames
    #[arg(long, value_name = "PNG")]
    pub qr_in: Vec<std::path::PathBuf>,

    /// Which stored key signs. Defaults to the only one, if there is only one
    #[arg(long, short = 'k', value_name = "LABEL")]
    pub key: Option<String>,

    /// Also write the result here
    #[arg(long, short = 'o', value_name = "FILE")]
    pub out: Option<std::path::PathBuf>,

    #[command(flatten)]
    pub qr: QrOut,
}

#[derive(Debug, Clone, Args)]
pub struct BroadcastArgs {
    /// The signed transaction: hex, `@file`, or `-` for stdin
    #[arg(value_name = "HEX|@FILE")]
    pub input: Option<String>,

    /// Read it from QR codes in a PNG instead. Repeat for several frames
    #[arg(long, value_name = "PNG")]
    pub qr_in: Vec<std::path::PathBuf>,
}

/// How to hand a payload across the gap as QR codes.
#[derive(Debug, Clone, Args)]
pub struct QrOut {
    /// Draw the payload as QR codes in the terminal
    #[arg(long)]
    pub qr: bool,

    /// Write the payload as QR codes to `<STEM>-1.png`, `<STEM>-2.png`, …
    #[arg(long, value_name = "STEM")]
    pub qr_out: Option<std::path::PathBuf>,
}

/// A VerusID registration.
///
/// Two transactions with a confirmation between them. Running this again after
/// the first resumes rather than starting over.
#[derive(Debug, Clone, Args)]
pub struct IdRegisterArgs {
    /// The name to claim, without a parent: `alice` or `alice@`
    #[arg(value_name = "NAME")]
    pub name: String,

    /// Which stored key pays. Defaults to the only one, if there is only one
    #[arg(long, short = 'f', value_name = "LABEL")]
    pub from: Option<String>,

    /// Addresses that will control the identity. Defaults to the paying key's
    #[arg(long, value_name = "ADDRESS")]
    pub primary: Vec<String>,

    /// How many of those must sign. Defaults to 1
    #[arg(long, value_name = "N")]
    pub min_sigs: Option<u32>,

    /// A referrer, which reduces the fee
    #[arg(long, value_name = "NAME@")]
    pub referral: Option<String>,
}

#[derive(Debug, Subcommand)]
pub enum IdCommand {
    /// Read an identity off the chain
    Show {
        /// The identity: a name like `bob@`, or an i-address
        #[arg(value_name = "NAME@|i-ADDRESS")]
        name: String,
    },
    /// Register a new VerusID. Run it again to carry on where it left off
    Register(IdRegisterArgs),
    /// Republish an identity with changes
    Update,
    /// Revoke an identity
    Revoke,
    /// Recover a revoked identity
    Recover,
    /// Prove control of an identity by signing a challenge
    Login {
        #[command(subcommand)]
        command: LoginCommand,
    },
    /// Publish VDXF data under an identity
    Publish(IdPublishArgs),
    /// Read VDXF data from an identity
    Read(IdReadArgs),
}

/// Signing in with a VerusID, split across the three parties that do it: the
/// site that asks, the holder who signs, and the site again when it checks.
#[derive(Debug, Subcommand)]
pub enum LoginCommand {
    /// Issue a single-use challenge for someone to sign
    Challenge(LoginChallengeArgs),
    /// Sign a challenge as an identity you hold a key for
    Sign(LoginSignArgs),
    /// Check a signature against the identity as it stood when it was made
    Verify(LoginVerifyArgs),
}

#[derive(Debug, Clone, Args)]
pub struct LoginChallengeArgs {
    /// Who is asking. It goes into the signed message, so a signature made for
    /// one site cannot be presented at another
    #[arg(long, short = 'a', value_name = "TEXT")]
    pub audience: String,
}

#[derive(Debug, Clone, Args)]
pub struct LoginSignArgs {
    /// The identity to sign as
    #[arg(value_name = "NAME@|i-ADDRESS")]
    pub name: String,

    /// Who asked. Must match what the verifier will check against
    #[arg(long, short = 'a', value_name = "TEXT")]
    pub audience: String,

    /// The challenge they issued
    #[arg(long, short = 'c', value_name = "HEX")]
    pub challenge: String,

    /// Which stored key signs. Must be one of the identity's primary addresses
    #[arg(long, short = 'f', value_name = "LABEL")]
    pub from: Option<String>,
}

#[derive(Debug, Clone, Args)]
pub struct LoginVerifyArgs {
    /// The identity that claims to have signed
    #[arg(value_name = "NAME@|i-ADDRESS")]
    pub name: String,

    /// Who asked
    #[arg(long, short = 'a', value_name = "TEXT")]
    pub audience: String,

    /// The challenge that was issued
    #[arg(long, short = 'c', value_name = "HEX")]
    pub challenge: String,

    /// The signature to check, base64 as every Verus tool exchanges it
    #[arg(long, short = 's', value_name = "BASE64")]
    pub signature: String,

    /// How old the signature may be, in blocks. Roughly a block a minute, so
    /// the default of 60 is an hour
    #[arg(long, value_name = "BLOCKS")]
    pub max_age: Option<u32>,

    /// Check the signature alone, without requiring that this machine issued
    /// the challenge. Replay is then nobody's job — for challenges that came
    /// from somewhere else and are tracked there
    #[arg(long)]
    pub stateless: bool,
}

#[derive(Debug, Clone, Args)]
pub struct IdPublishArgs {
    /// The identity to write to. You must hold enough of its primary keys
    #[arg(value_name = "NAME@|i-ADDRESS")]
    pub name: String,

    /// The VDXF key name, as `pecu id read` takes it back
    #[arg(value_name = "KEY")]
    pub key: String,

    /// The value: text, `@file` to read a file, or `-` for stdin
    #[arg(value_name = "VALUE")]
    pub value: Option<String>,

    /// Which stored key signs and pays the fee. Must be a primary address
    #[arg(long, short = 'f', value_name = "LABEL")]
    pub from: Option<String>,

    /// Whose namespace the key hangs under. Defaults to the identity itself
    #[arg(long, value_name = "NAME@|i-ADDRESS")]
    pub namespace: Option<String>,

    /// Delete the key instead of writing to it
    #[arg(long, conflicts_with = "value")]
    pub remove: bool,
}

#[derive(Debug, Clone, Args)]
pub struct IdReadArgs {
    /// The identity to read
    #[arg(value_name = "NAME@|i-ADDRESS")]
    pub name: String,

    /// The VDXF key name. Without one, every key the identity holds
    #[arg(value_name = "KEY")]
    pub key: Option<String>,

    /// Whose namespace the key hangs under. Defaults to the identity itself
    #[arg(long, value_name = "NAME@|i-ADDRESS")]
    pub namespace: Option<String>,

    /// Every value ever published under this key, oldest first, rather than
    /// what stands there now
    #[arg(long, requires = "key")]
    pub history: bool,
}
