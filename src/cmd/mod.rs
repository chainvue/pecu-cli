//! Command dispatch.
//!
//! Commands land one milestone at a time. Everything not yet implemented
//! answers with [`NotYet`] rather than a panic or a silent no-op, so
//! `pecu --help` is an honest map of what this build can do.

pub mod airgap;
pub mod currency;
pub mod dev;
pub mod doctor;
pub mod id;
pub mod key;
pub mod lifecycle;
pub mod send;
pub mod tx;
pub mod wallet;

use std::path::{Path, PathBuf};

use clap::CommandFactory;
use miette::Diagnostic;
use thiserror::Error;
use verus_sdk::money::DEFAULT_FEE_PER_KB;
use verus_sdk::verus_tx::estimate_fee;
use verus_sdk::verus_tx::fee::DUST_THRESHOLD;

use crate::cli::{
    Cli, Command, CurrencyCommand, DevCommand, IdCommand, PlanCommand, TxCommand, WalletCommand,
};
use crate::config::{Paths, Settings};
use crate::ui::{fmt, Ui};

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

/// `-v` names a capability this binary has never had.
///
/// It was scaffolded in M0, wired to nothing, and has been advertised on every
/// help screen since: `pecu wallet balance -v --json` exited `0` with output
/// byte-identical to the same run without it, on stdout *and* stderr. There is
/// no logging framework linked to turn up, and `--explain` already occupies the
/// diagnostic niche.
///
/// # Why this is refused rather than deleted
///
/// Deleting the flag hands the question to clap, whose whole vocabulary here is
/// `unexpected argument`. Measured, that is worse than it sounds: bare `-v`
/// gets no suggestion at all, and `--verbose` gets `tip: a similar argument
/// exists: '--version'` — a flag that parses, prints a version string and exits
/// `0` without doing the work, offered as the fix on a wallet that spends. It
/// is the same misspelling-shaped answer the `plan send --currency` refusal
/// exists to avoid, and it drops the run from exit `0` to exit `2` with an
/// empty stdout, so a `--json` consumer gets nothing to parse rather than an
/// error document.
///
/// # Why it is refused rather than quietly ignored
///
/// Accepting it and doing nothing is the shape a run that asked for detail and
/// exited `0` with none already has, and hiding it would make that both
/// undiscoverable and still untrue. Nobody depends on this flag's *behaviour* —
/// there is none — only on its acceptance, and acceptance is exactly the thing
/// that was lying.
///
/// So the flag stays declared, leaves every help screen and every completion
/// script, and is answered by name with a `pecu::…` code and a document. The
/// status moves from `0` to `1`, which is a change to the published contract
/// and is written down in `docs/configuration.md`.
///
/// # Where this gate does not run
///
/// It is a `dispatch` gate, so clap gets there first in two cases, and both
/// keep their old status rather than reaching the refusal. `--help` and
/// `--version` are resolved inside `get_matches`, so `pecu key list -v --help`
/// still exits `0` with `-v` swallowed. A command line that is invalid for some
/// other reason is answered by that reason: `pecu key show -v` is clap's exit
/// `2` for the missing `<LABEL>`, not this. Closing either would need an `argv`
/// scan ahead of clap, which cannot tell a `-v` flag from a `-v` value — a worse
/// bug than the one it fixes. Both are recorded in `docs/configuration.md`
/// rather than quietly left for someone to find.
#[derive(Debug, Error, Diagnostic)]
// Both spellings, because the message is fixed and the command line is not: a
// reader who typed `--verbose` should not be told to drop something they did
// not type. Which one clap matched is not in `ArgMatches` to ask.
#[error("-v/--verbose turns up logging this build does not have")]
#[diagnostic(
    code(pecu::verbose_does_nothing),
    help(
        "nothing in `pecu` logs. No logging framework is linked, and -v, -vv and -vvv all \
         produced output byte-identical to no flag at all, on stdout and on stderr — the flag was \
         scaffolded with the command tree and never wired to anything. It is refused rather than \
         swallowed so that you are told, instead of a run that asked for detail exiting 0 with \
         none of it. `--explain` is the diagnostic that works: pass it to any command to see each \
         verus-sdk call this build makes and what it decoded back. Drop -v/--verbose"
    )
)]
pub struct VerboseDoesNothing;

/// What to do about a broadcast whose outcome the node did not settle.
///
/// Shared by every command that broadcasts, because the answer does not depend
/// on what was being sent. Two causes arrive as one variant: a connection that
/// broke after the bytes went out, and a node answering `-25` — a generic
/// verify failure that, unlike `-26`, does not say the transaction was
/// *refused*. The SDK classifies both as `FlowError::BroadcastUncertain` for
/// the same reason: resending blindly risks a second broadcast of something
/// already propagating.
///
/// So the catch-all "run `pecu doctor`" is wrong twice here — the node is not
/// the problem, and the retry it invites is how one launch fee gets burned
/// twice.
///
/// The signed bytes come with the variant and are saved before the sentence is
/// written, because `pecu tx explain <txid>` only answers while a node still
/// holds the transaction — and the case that most often produces a `-25` is the
/// one where no node ever did. The bytes are then the only thing left that can
/// settle whether what was built was wrong or merely unwelcome.
pub(crate) fn uncertain_broadcast_advice(txid: &str, hex: &str) -> String {
    let saved = unsent_paths().and_then(|paths| save_unsent(&paths, txid, hex).ok());
    uncertain_broadcast_text(txid, saved.as_deref())
}

/// Where the bytes go: the real configuration root.
#[cfg(not(test))]
fn unsent_paths() -> Option<Paths> {
    Paths::resolve().ok()
}

/// Where the bytes go under `cargo test`: nowhere, unless a test has said
/// where.
///
/// `Paths::resolve()` falls back to `$XDG_CONFIG_HOME`/`~/.config/verus-pecu`
/// when `$PECU_HOME` is unset, which is the live keystore root — so the unit
/// tests that reach this through a command's error mapper would write files
/// next to `keys/` and `pending/` on every developer machine. Setting
/// `$PECU_HOME` from inside a test is not the fix: it is process-global and the
/// unit tests share one process and run in parallel, so it would race with
/// every other test that resolves `Paths`. A thread-local is per-test by
/// construction.
///
/// Unset means no directory rather than the real one, so a test that forgets
/// [`UnsentRoot::temporary`] gets the "could not be written anywhere" sentence
/// instead of quietly touching a wallet.
#[cfg(test)]
fn unsent_paths() -> Option<Paths> {
    UNSENT_ROOT.with(|root| root.borrow().as_ref().map(|dir| Paths::at(dir.path())))
}

#[cfg(test)]
thread_local! {
    static UNSENT_ROOT: std::cell::RefCell<Option<tempfile::TempDir>> =
        const { std::cell::RefCell::new(None) };
}

/// Points [`uncertain_broadcast_advice`] at a temporary directory for as long
/// as it is alive.
///
/// Held by every unit test that reaches the advice through an error mapper
/// rather than calling [`uncertain_broadcast_text`] directly. Dropping it puts
/// the thread back to writing nowhere, so one test cannot leave the seam open
/// for the next one that runs on the same thread.
#[cfg(test)]
#[must_use = "the temporary root is only in place while the guard is alive"]
pub(crate) struct UnsentRoot;

#[cfg(test)]
impl UnsentRoot {
    pub(crate) fn temporary() -> Self {
        let directory = tempfile::tempdir().expect("a temporary directory");
        UNSENT_ROOT.with(|root| *root.borrow_mut() = Some(directory));
        Self
    }
}

#[cfg(test)]
impl Drop for UnsentRoot {
    fn drop(&mut self) {
        UNSENT_ROOT.with(|root| *root.borrow_mut() = None);
    }
}

/// Write the signed bytes where a later command can pick them up.
///
/// Split out from the sentence so both halves can be tested without touching
/// `$PECU_HOME`: this one takes the directory, the other takes the result.
fn save_unsent(paths: &Paths, txid: &str, hex: &str) -> std::io::Result<PathBuf> {
    let directory = paths.unsent_dir();
    std::fs::create_dir_all(&directory)?;
    // The txid is computed locally from the bytes, so it is already hex — but it
    // reaches here as a `String` from another crate and is about to become a
    // path, and a filename is not the place to find out that assumption was
    // wrong.
    let stem: String = txid
        .chars()
        .filter(char::is_ascii_hexdigit)
        .take(64)
        .collect();
    let path = directory.join(format!(
        "{}.hex",
        if stem.is_empty() { "unsent" } else { &stem }
    ));
    std::fs::write(&path, hex)?;
    Ok(path)
}

/// The sentence itself, given whether the bytes were saved.
fn uncertain_broadcast_text(txid: &str, saved: Option<&Path>) -> String {
    let bytes = match saved {
        // `@file` and `-` are what these two commands already accept, so the
        // path is usable as it stands. `tx explain` on the bytes needs no node
        // at all, which is the point: it answers even when nothing ever
        // accepted the transaction.
        Some(path) => {
            let path = path.display();
            format!(
                " The signed bytes are at `{path}`, so nothing has to be rebuilt: \
                 `pecu tx explain - < {path}` reads them with no node involved, and \
                 `pecu broadcast @{path}` resends exactly those — but only once the check \
                 above says nothing landed"
            )
        }
        None => " The signed bytes could not be written anywhere, so they go with this \
                 process: rebuilding is the only way back to them"
            .to_string(),
    };
    format!(
        "the node did not say no. Either it answered without settling the outcome or it \
         stopped answering after the bytes went out, so this may already be propagating — \
         and sending it again would broadcast the same operation twice. Check first: \
         `pecu tx explain {txid}` decodes it if the node has it, and says it knows no such \
         transaction if nothing was ever accepted.{bytes}"
    )
}

/// The fee a shortfall message has in hand, and how sure of it that message may
/// sound.
///
/// The distinction is the whole point. A fee taken out of the builder's own
/// refusal priced the exact transaction that was refused, so what is left after
/// it is a ceiling: retrying at that figure selects the same inputs, pays the
/// same fee and leaves no change. A fee worked out *before* selection ran is
/// deliberately high — see [`estimated_native_fee`] — so the same subtraction
/// is a floor and nothing more. Calling that a maximum would be a smaller
/// version of the "needed including fee" wording this replaced: a sentence
/// asserting more than the code behind it knows.
pub(crate) enum Fee {
    /// What the builder charged, read back out of its refusal.
    Charged(u64),
    /// What a transaction of this shape would cost at most, sized before the
    /// inputs were chosen.
    Estimated(u64),
}

/// What the fee leaves for a payment, said in a sentence.
///
/// Shared by `pecu send` and `pecu plan send` because the arithmetic is the
/// same on both: the fee is charged on top of the amount, never taken out of
/// it, so the number worth printing is what is left after it — not the number
/// that was just refused.
pub(crate) fn what_the_fee_leaves(available: u64, fee: Fee) -> String {
    const ON_TOP: &str = "The fee is charged on top of the amount rather than taken out of it";

    // With nothing to spend, where the fee sits explains nothing: the payment
    // fails identically either way, and the clause is filler in front of a
    // reader who has just been told the address is empty.
    if available == 0 {
        return "There is nothing at this address to pay with.".to_string();
    }

    let (fee, exact) = match fee {
        Fee::Charged(fee) => (fee, true),
        Fee::Estimated(fee) => (fee, false),
    };
    match available.checked_sub(fee) {
        None | Some(0) => {
            format!("{ON_TOP}, and what is here does not cover a payment and a fee as well.")
        }
        // An output this small is dust, which a node is entitled to refuse
        // whatever the arithmetic says. Naming it as an amount that works
        // would be the same overclaim as naming an amount the fee eats.
        Some(most) if most <= DUST_THRESHOLD => format!(
            "{ON_TOP}, and the {} it would leave is small enough that a node may refuse the \
             output as dust.",
            fmt::sats(most)
        ),
        Some(most) if exact => {
            format!(
                "{ON_TOP}, so the most that can move from here is {}.",
                fmt::sats(most)
            )
        }
        Some(most) => format!(
            "{ON_TOP}, so a payment of {} will go through — perhaps a little more, since the \
             fee is not settled until the inputs are chosen and the recipient's output is \
             sized.",
            fmt::sats(most)
        ),
    }
}

/// What a one-recipient native send of this shape would pay in fees, erring
/// high.
///
/// Sized as though every output were a CryptoCondition, which is true when the
/// recipient is a VerusID and not when it is an R-address, where the builder
/// prices 34 bytes rather than 200 — a flat 3,320 satoshis of overshoot from
/// five inputs up. It is also priced over *every* output the address holds,
/// where selection stops as soon as it has enough. Both err the same way, and
/// [`Fee::Estimated`] is what keeps the sentence built on it from claiming
/// otherwise: overstating the fee understates the payment, so the figure named
/// is one that goes through rather than one that is refused a second time —
/// which is the whole complaint this exists to answer. Below four inputs both
/// output shapes land on the same 10,000-satoshi floor in any case.
///
/// `u64::MAX` on overflow rather than some fallback figure: an input count that
/// overflows the size arithmetic is not a transaction anybody can pay for, and
/// saying nothing works beats naming an amount that does not.
pub(crate) fn estimated_native_fee(utxos: usize) -> u64 {
    // One recipient and the change output, which is the shape `prepare_send`
    // builds and the count `select_utxos` prices its fee at.
    estimate_fee(utxos as u64, 2, DEFAULT_FEE_PER_KB, true).unwrap_or(u64::MAX)
}

pub fn dispatch(cli: Cli) -> miette::Result<()> {
    // Ahead of everything, including `Ui`: a flag that can never be honoured
    // should cost neither a rendered line nor a round trip, and this one is
    // global, so there is no command it could be honoured by.
    if cli.globals.verbose > 0 {
        return Err(VerboseDoesNothing.into());
    }

    let ui = Ui::new(cli.globals.theme, cli.globals.json, cli.globals.explain);

    match &cli.command {
        Command::Dev { command } => match command {
            DevCommand::Ui => {
                // The one command that ignores `--json`. Everywhere else the
                // flag means stdout carries a document and nothing else, which
                // `Ui` now enforces for every rendered thing; here the rendered
                // thing *is* the output, and there is no machine-readable form
                // of how something looks. So the gallery is drawn anyway and
                // the note about it goes to stderr, which is not the stream
                // anybody would be parsing.
                if cli.globals.json {
                    eprintln!("`dev ui` has no --json form; showing the rendered gallery");
                }
                dev::gallery(&Ui::new(cli.globals.theme, false, cli.globals.explain));
                Ok(())
            }
        },

        Command::Currency { command } => {
            let settings = Settings::resolve(&cli.globals)?;
            match command {
                CurrencyCommand::Show { name } => currency::show(&ui, &settings, name),
                CurrencyCommand::Launch(args) => {
                    currency::launch(&ui, &settings, &cli.globals, args)
                }
                CurrencyCommand::Mint(args) => currency::mint(&ui, &settings, &cli.globals, args),
                CurrencyCommand::Preconvert(args) => {
                    currency::preconvert(&ui, &settings, &cli.globals, args)
                }
                CurrencyCommand::Convert(args) => {
                    currency::convert(&ui, &settings, &cli.globals, args)
                }
            }
        }

        Command::Completions { shell } => {
            // The other command that ignores `--json`: a completion script is
            // not a document, and wrapping one in JSON would only mean nobody
            // could `source` it. Said on stderr, the way `dev ui` does, so the
            // script on stdout stays usable either way.
            if cli.globals.json {
                eprintln!("`completions` has no --json form; printing the shell script");
            }
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
                WalletCommand::History(args) => wallet::history_command(&ui, &settings, args),
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
            command: IdCommand::List { target },
        } => id::list(&ui, &Settings::resolve(&cli.globals)?, target),

        Command::Id {
            command: IdCommand::Register(args),
        } => id::register(&ui, &Settings::resolve(&cli.globals)?, &cli.globals, args),

        Command::Id {
            command: IdCommand::Update(args),
        } => lifecycle::update(&ui, &Settings::resolve(&cli.globals)?, &cli.globals, args),

        Command::Id {
            command: IdCommand::Revoke(args),
        } => lifecycle::revoke(&ui, &Settings::resolve(&cli.globals)?, &cli.globals, args),

        Command::Id {
            command: IdCommand::Recover(args),
        } => lifecycle::recover(&ui, &Settings::resolve(&cli.globals)?, &cli.globals, args),

        Command::Id {
            command: IdCommand::Unlock(args),
        } => lifecycle::unlock(&ui, &Settings::resolve(&cli.globals)?, &cli.globals, args),

        Command::Id { command } => Err(match command {
            IdCommand::Show { .. }
            | IdCommand::List { .. }
            | IdCommand::Register(_)
            | IdCommand::Update(_)
            | IdCommand::Revoke(_)
            | IdCommand::Recover(_)
            | IdCommand::Unlock(_) => unreachable!("handled above"),
            IdCommand::Login => NotYet::at("id login", "M8"),
            IdCommand::Publish => NotYet::at("id publish", "M8"),
            IdCommand::Read => NotYet::at("id read", "M8"),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Paths;

    /// The bytes are what settle a `-25` later, and `pecu tx explain <txid>`
    /// cannot: a transaction refused before the mempool is one no node holds,
    /// so the txid alone is a dead end at exactly the moment this advice is
    /// printed. Written to a file rather than into the sentence because a
    /// launch is kilobytes of hex and every renderer wraps it.
    #[test]
    fn the_signed_bytes_of_an_unsettled_broadcast_survive_the_error() {
        let home = tempfile::tempdir().expect("a temporary directory");
        let paths = Paths::at(home.path());
        let hex = "0400008085202f89".repeat(64);

        let saved = save_unsent(&paths, "7fed7b98", &hex).expect("the bytes were written");

        assert_eq!(
            std::fs::read_to_string(&saved).expect("the file is there"),
            hex,
            "the signed bytes did not survive into the file the advice names"
        );
        let advice = uncertain_broadcast_text("7fed7b98", Some(&saved));
        assert!(advice.contains(&saved.display().to_string()));
        assert!(advice.contains("pecu tx explain - <"));
        assert!(!advice.contains("doctor"));
    }

    /// A txid reaches here as a `String` from another crate and leaves as a
    /// path. Anything that is not hex is dropped rather than trusted.
    #[test]
    fn a_txid_that_is_not_hex_cannot_steer_where_the_bytes_are_written() {
        let home = tempfile::tempdir().expect("a temporary directory");
        let paths = Paths::at(home.path());

        let saved = save_unsent(&paths, "../../etc/passwd", "00").expect("the bytes were written");

        assert_eq!(
            saved.parent(),
            Some(paths.unsent_dir().as_path()),
            "a txid escaped the directory it was meant to name a file in"
        );
        assert_eq!(saved.file_name().and_then(|n| n.to_str()), Some("ecad.hex"));
    }

    /// Failing to write is not a reason to invent a file. The sentence has to
    /// stay true either way, and the txid half of the check still works.
    #[test]
    fn advice_with_nowhere_to_put_the_bytes_says_so_rather_than_naming_a_path() {
        let advice = uncertain_broadcast_text("7fed7b98", None);

        assert!(advice.contains("tx explain 7fed7b98"));
        assert!(advice.contains("could not be written anywhere"));
        assert!(!advice.contains("pecu broadcast @"));
    }
}
