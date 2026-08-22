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
        "498c396eb2016d2e010ac78eae1839c858455c59"
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

    /// Currencies, which are defined by identities
    Currency {
        #[command(subcommand)]
        command: CurrencyCommand,
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

    /// Pay out of funds a VerusID holds, rather than out of the key's own
    /// address. `--from` then names a key that is one of its primary
    /// addresses: the identity owns the money, the key proves the authority
    #[arg(long, value_name = "NAME@|i-ADDRESS", conflicts_with = "currency")]
    pub from_identity: Option<String>,
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
    /// Every transaction that touched an address, oldest first
    History(HistoryArgs),
}

#[derive(Debug, Clone, Args)]
pub struct HistoryArgs {
    #[command(flatten)]
    pub target: WalletTarget,

    /// Start at this height. Without a range the whole chain is searched,
    /// which on a busy address is a very large reply
    #[arg(long, value_name = "HEIGHT")]
    pub from_height: Option<u32>,

    /// Stop at this height
    #[arg(long, value_name = "HEIGHT")]
    pub to_height: Option<u32>,

    /// Show at most this many, most recent last
    #[arg(long, short = 'n', value_name = "COUNT", default_value_t = 25)]
    pub limit: usize,
}

/// Which address to look at. Read-only commands take an address, never a key —
/// watching a balance needs no secret.
#[derive(Debug, Clone, Args)]
#[group(multiple = false)]
pub struct WalletTarget {
    /// Look at this address, or a VerusID name like `bob@`, which is resolved
    #[arg(long, short = 'a', value_name = "R…|NAME@")]
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
    /// Discard a saved reservation and claim the name from scratch
    #[arg(long)]
    pub restart: bool,
    /// Stop after each step instead of waiting for the commitment to confirm
    #[arg(long)]
    pub no_wait: bool,

    /// How long to wait for the commitment, in minutes
    #[arg(long, value_name = "MINUTES", default_value_t = 20)]
    pub timeout: u64,
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
    Update(IdUpdateArgs),
    /// Revoke an identity, using its revocation authority
    Revoke(IdAuthorityArgs),
    /// Bring a revoked identity back, using its recovery authority
    Recover(IdRecoverArgs),
    /// Start the countdown on a delay-locked identity
    Unlock(IdUnlockArgs),
    /// Prove control of an identity by signing a challenge
    Login,
    /// Publish VDXF data under an identity
    Publish,
    /// Read VDXF data from an identity
    Read,
}

/// Changing an identity. Every field left unnamed is carried through untouched.
#[derive(Debug, Clone, Args)]
pub struct IdUpdateArgs {
    /// The identity to change. Prefer its i-address for anything destructive:
    /// a name is only checked against what the node itself reported
    #[arg(value_name = "NAME@|i-ADDRESS")]
    pub name: String,

    /// Which stored key signs and pays the fee
    #[arg(long, short = 'f', value_name = "LABEL")]
    pub from: Option<String>,

    /// Replace the addresses that control the identity. Repeat for several
    #[arg(long, value_name = "ADDRESS")]
    pub primary: Vec<String>,

    /// How many of those must sign
    #[arg(long, value_name = "N")]
    pub min_sigs: Option<u32>,

    /// Point revocation at another VerusID. One-way: these keys cannot take it
    /// back afterwards
    #[arg(long, value_name = "NAME@|i-ADDRESS")]
    pub revocation: Option<String>,

    /// Point recovery at another VerusID. One-way, and what makes an identity
    /// revocable in the first place
    #[arg(long, value_name = "NAME@|i-ADDRESS")]
    pub recovery: Option<String>,

    /// Lock the identity until an absolute block height. The countdown runs
    /// from when this is mined and cannot be paused
    #[arg(long, value_name = "HEIGHT", conflicts_with_all = ["unlock_delay", "clear_timelock"])]
    pub lock_until: Option<u32>,

    /// Lock the identity indefinitely, unlocking this many blocks after an
    /// unlock is asked for. Nothing starts counting until then
    #[arg(long, value_name = "BLOCKS", conflicts_with = "clear_timelock")]
    pub unlock_delay: Option<u32>,

    /// Remove the timelock entirely
    #[arg(long)]
    pub clear_timelock: bool,

    /// Permit changing who controls the identity. Required for --primary,
    /// --min-sigs, --revocation and --recovery, because publishing a threshold
    /// nobody can meet is the one mistake with no remedy
    #[arg(long)]
    pub allow_authority_change: bool,
}

/// Revoking. Takes no changes: revocation sets a flag and nothing else.
#[derive(Debug, Clone, Args)]
pub struct IdAuthorityArgs {
    /// The identity to revoke. Prefer its i-address
    #[arg(value_name = "NAME@|i-ADDRESS")]
    pub name: String,

    /// Which stored key signs and pays the fee. It must be a primary address
    /// of the identity's revocation authority, which is often another VerusID
    #[arg(long, short = 'f', value_name = "LABEL")]
    pub from: Option<String>,
}

/// Recovering, which may also hand the identity to new keys.
#[derive(Debug, Clone, Args)]
pub struct IdRecoverArgs {
    /// The revoked identity to bring back. Prefer its i-address
    #[arg(value_name = "NAME@|i-ADDRESS")]
    pub name: String,

    /// Which stored key signs and pays the fee. It must be a primary address
    /// of the identity's recovery authority
    #[arg(long, short = 'f', value_name = "LABEL")]
    pub from: Option<String>,

    /// Hand the recovered identity to these addresses. Without it the identity
    /// comes back under whatever keys it had when it was revoked
    #[arg(long, value_name = "ADDRESS")]
    pub primary: Vec<String>,

    /// How many of those must sign
    #[arg(long, value_name = "N", requires = "primary")]
    pub min_sigs: Option<u32>,
}

/// Asking a delay-locked identity to start counting down.
///
/// Its own command because the height cannot be worked out by hand: consensus
/// measures the countdown from the transaction's own expiry rather than from
/// the tip, and the expiry belongs to the transaction the flow is building.
#[derive(Debug, Clone, Args)]
pub struct IdUnlockArgs {
    /// The identity to start unlocking. Prefer its i-address
    #[arg(value_name = "NAME@|i-ADDRESS")]
    pub name: String,

    /// Which stored key signs and pays the fee
    #[arg(long, short = 'f', value_name = "LABEL")]
    pub from: Option<String>,

    /// Wait this many blocks beyond the earliest consensus allows. The floor is
    /// the delay plus the transaction's expiry; this adds to it
    #[arg(long, value_name = "BLOCKS", default_value_t = 0)]
    pub extra_blocks: u32,
}

#[derive(Debug, Subcommand)]
pub enum CurrencyCommand {
    /// Read a currency definition off the chain
    Show {
        /// The currency: a name like `bridge@`, or an i-address
        #[arg(value_name = "NAME@|i-ADDRESS")]
        name: String,
    },
    /// Define a currency under an identity you control
    ///
    /// Boxed because a full basket definition carries far more than a name, and
    /// an enum is as large as its largest variant.
    Launch(Box<CurrencyLaunchArgs>),
    /// Create new supply of a centralized currency you control
    Mint(CurrencyMintArgs),
    /// Buy into a currency before it launches, at the launch price
    Preconvert(CurrencyPreconvertArgs),
    /// Convert between a basket and its reserves, or between two of its reserves
    Convert(CurrencyConvertArgs),
}

#[derive(Debug, Args)]
pub struct CurrencyConvertArgs {
    /// What you want out: a name like `mybasket@`, or an i-address
    #[arg(value_name = "NAME@|i-ADDRESS")]
    pub currency: String,

    /// How much to spend
    #[arg(long, value_name = "COINS")]
    pub amount: String,

    /// What to spend. Defaults to the chain's own currency
    #[arg(long, value_name = "NAME@|i-ADDRESS")]
    pub spend: Option<String>,

    /// The basket to route through, when converting one reserve into another
    #[arg(long, value_name = "NAME@|i-ADDRESS")]
    pub via: Option<String>,

    /// Which stored key signs and pays
    #[arg(short = 'f', long, value_name = "LABEL")]
    pub from: Option<String>,

    /// Who receives it. Defaults to the paying key
    #[arg(long, value_name = "R-ADDRESS")]
    pub to: Option<String>,

    /// Refuse if the node's estimate is below this
    #[arg(long, value_name = "COINS")]
    pub min_out: Option<String>,

    /// The reserve transfer fee, in native coins
    #[arg(long, value_name = "COINS", default_value = "0.0002")]
    pub fee: String,
}

#[derive(Debug, Args)]
pub struct CurrencyPreconvertArgs {
    /// The launching currency to buy: a name like `mybasket@`, or an i-address
    #[arg(value_name = "NAME@|i-ADDRESS")]
    pub currency: String,

    /// How much to spend
    #[arg(long, value_name = "COINS")]
    pub amount: String,

    /// What to spend. Defaults to the chain's own currency
    #[arg(long, value_name = "NAME@|i-ADDRESS")]
    pub spend: Option<String>,

    /// Which stored key signs and pays
    #[arg(short = 'f', long, value_name = "LABEL")]
    pub from: Option<String>,

    /// Who receives the new currency at launch. Defaults to the paying key
    #[arg(long, value_name = "R-ADDRESS")]
    pub to: Option<String>,

    /// The reserve transfer fee, in native coins
    #[arg(long, value_name = "COINS", default_value = "0.0002")]
    pub fee: String,
}

#[derive(Debug, Args)]
pub struct CurrencyMintArgs {
    /// The currency to mint: a name like `mytoken@`, or an i-address
    #[arg(value_name = "NAME@|i-ADDRESS")]
    pub currency: String,

    /// Who receives the new supply — a transparent R-address
    #[arg(long, value_name = "R-ADDRESS")]
    pub to: String,

    /// How much new supply to create
    #[arg(long, value_name = "COINS")]
    pub amount: String,

    /// Which stored key signs. Must be one of the defining identity's primaries
    #[arg(short = 'f', long, value_name = "LABEL")]
    pub from: Option<String>,

    /// The reserve transfer fee, in native coins
    #[arg(long, value_name = "COINS", default_value = "0.0002")]
    pub fee: String,
}

/// Launching a token.
///
/// A fractional basket is deliberately not here: reserves, weights, conversion
/// rates and preconversion limits are six vectors indexed by the same list, and
/// choosing them is a design exercise rather than a set of flags.
#[derive(Debug, Clone, Args)]
pub struct CurrencyLaunchArgs {
    /// The identity that will define it. Its i-address becomes the currency's
    /// id, and it can never define another
    #[arg(value_name = "NAME@|i-ADDRESS")]
    pub name: String,

    /// Which stored key signs and pays. Must be a primary address of that
    /// identity
    #[arg(long, short = 'f', value_name = "LABEL")]
    pub from: Option<String>,

    /// How much exists at launch.
    ///
    /// For a token this is preallocated to the defining identity, because a
    /// token's supply *is* the sum of its preallocations. For a basket it is
    /// the initial supply the reserves price against, which is a different
    /// field entirely — the panel says which one it used
    #[arg(long, value_name = "COINS")]
    pub supply: Option<String>,

    /// Give some of the supply to an identity, as `i-ADDRESS:COINS`. Repeat for
    /// several
    #[arg(long, value_name = "i-ADDRESS:COINS")]
    pub preallocate: Vec<String>,

    /// Back it with this reserve, as `NAME@:PERCENT`. Repeat for each one;
    /// the percentages must total exactly 100. Giving any makes it a
    /// fractional basket rather than a plain token
    #[arg(long, value_name = "NAME@|i-ADDRESS:PERCENT")]
    pub reserve: Vec<String>,

    /// Let the defining identity mint more later. Off by default: a fixed
    /// supply is the thing holders can verify, and this cannot be undone
    #[arg(long, conflicts_with = "reserve")]
    pub mintable: bool,

    /// Refused: the SDK's launch builder funds no contribution, so this would
    /// declare reserve backing the basket does not hold. `pecu currency
    /// preconvert` is what actually seeds a reserve, and it spends
    #[arg(long, value_name = "NAME@:COINS", requires = "reserve")]
    pub contribute: Vec<String>,

    /// Least anyone may preconvert into a reserve, as `NAME@:COINS`
    #[arg(long, value_name = "NAME@:COINS", requires = "reserve")]
    pub min_preconvert: Vec<String>,

    /// Most anyone may preconvert into a reserve, as `NAME@:COINS`
    #[arg(long, value_name = "NAME@:COINS", requires = "reserve")]
    pub max_preconvert: Vec<String>,

    /// Pre-launch conversion rate for a reserve, as `NAME@:RATE`
    #[arg(long, value_name = "NAME@:RATE", requires = "reserve")]
    pub conversion: Vec<String>,

    /// Discount for converting before launch, as a percentage
    #[arg(long, value_name = "PERCENT", requires = "reserve")]
    pub prelaunch_discount: Option<String>,

    /// Share of the launch carved out to the defining identity, as a percentage
    #[arg(long, value_name = "PERCENT", requires = "reserve")]
    pub prelaunch_carveout: Option<String>,

    /// What registering a sub-identity under this currency costs
    #[arg(long, value_name = "COINS")]
    pub id_registration_fee: Option<String>,

    /// How many referral levels a sub-identity registration pays. Sets the
    /// referral option bit on its own
    #[arg(long, value_name = "N")]
    pub id_referral_levels: Option<u32>,

    /// What importing an identity into this currency costs
    #[arg(long, value_name = "COINS")]
    pub id_import_fee: Option<String>,

    /// A referral is mandatory for sub-identity registration
    #[arg(long, requires = "id_referral_levels")]
    pub id_referral_required: bool,

    /// An NFT: exactly one indivisible unit, held by the defining identity.
    ///
    /// Sets the supply itself — one satoshi, which is what makes it
    /// non-fungible — so it cannot be combined with --supply
    #[arg(long, conflicts_with_all = ["reserve", "supply", "preallocate", "mintable"])]
    pub nft: bool,

    /// Only identities may hold it
    #[arg(long)]
    pub id_restricted: bool,

    /// Identities may stake it
    #[arg(long)]
    pub id_staking: bool,

    /// No identities may be registered under it
    #[arg(long, conflicts_with_all = ["id_registration_fee", "id_referral_levels", "id_import_fee"])]
    pub no_ids: bool,

    /// The block it stops at. Zero, the default, means never
    #[arg(long, value_name = "HEIGHT")]
    pub end_block: Option<u32>,

    /// The block conversions open at. Defaults to the tip plus --start-in
    #[arg(long, value_name = "HEIGHT", conflicts_with = "start_in")]
    pub start_block: Option<u32>,

    /// How many blocks ahead to start, when --start-block is not given
    #[arg(long, value_name = "BLOCKS", default_value_t = 20)]
    pub start_in: u32,
    /// Register the defining identity first if it does not exist yet. A dry run
    /// stops there rather than registering: with no identity on chain there is
    /// nothing for the launch to be defined by and nothing to preview
    #[arg(long)]
    pub register: bool,

    /// How long to wait for that registration, in minutes
    #[arg(long, value_name = "MINUTES", default_value_t = 20)]
    pub register_timeout: u64,
}
