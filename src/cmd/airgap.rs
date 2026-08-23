//! `pecu plan send` · `pecu sign` · `pecu broadcast` — the air-gap ceremony.
//!
//! Three commands because there are three machines' worth of trust:
//!
//! 1. **`plan send`** runs where the node is. It holds no key and cannot sign;
//!    it chooses coins and places outputs, and hands over a partial transaction.
//! 2. **`sign`** runs where the key is. It opens no socket — it does not need
//!    one, because everything it must check is inside the bytes it was given.
//! 3. **`broadcast`** runs where the node is again, and only carries finished
//!    bytes.
//!
//! # What the signer has to check, and why it can
//!
//! Whoever planned the transaction chose the outputs, and a signature is the
//! irreversible step. So `sign` shows the whole thing before it will sign
//! anything, and it checks the sighash types: outputs are only binding on your
//! input if your input commits to them. Under `SIGHASH_NONE` they are not
//! covered at all and whoever holds the partial can redirect the money after
//! you sign. That check is [`Summary::commits_to_all_outputs`], and this refuses
//! to sign without `--yes` when it is false.
//!
//! The planning and signing halves go through the same `plan_transparent_send`
//! the one-shot `pecu send` uses, so the transaction that comes out the far end
//! is byte-for-byte the one `send` would have made.

use std::io::{IsTerminal, Write};

use miette::Diagnostic;
use thiserror::Error;
use verus_sdk::cosign::{PartialTransaction, Summary};
use verus_sdk::money::Amount;
use verus_sdk::network::{broadcast as submit, prepare_unsigned_send, FlowError};
use verus_sdk::verus_keys::{Address, AddressKind};

use crate::cli::{BroadcastArgs, Globals, PlanSendArgs, QrOut, SignArgs};
use crate::cmd::tx;
use crate::cmd::wallet;
use crate::cmd::{estimated_native_fee, uncertain_broadcast_advice, what_the_fee_leaves, Fee};
use crate::config::Settings;
use crate::keystore::{self, Keystore};
use crate::node;
use crate::payload;
use crate::ui::{fmt, qr, Panel, Text, Ui};

#[derive(Debug, Error, Diagnostic)]
pub enum AirgapError {
    #[error("the `{profile}` profile is not allowed to spend")]
    #[diagnostic(
        code(pecu::spending_disabled),
        help("the air gap is still a spend: planning one picks real coins and broadcasting one is irreversible. Set `allow_spend = true` under [profiles.{profile}] in config.toml")
    )]
    SpendingDisabled { profile: String },

    #[error("`{amount}` is not an amount")]
    #[diagnostic(
        code(pecu::bad_amount),
        help("a decimal number of coins, at most eight places")
    )]
    BadAmount { amount: String },

    /// Fieldless, with the help inline, because every word of it is the same
    /// on every run — matching the `--contribute` and `--conversion` refusals
    /// rather than the `{detail}` variants around it, which carry a value.
    ///
    /// The layer matters in the wording: nothing about the partial *format* is
    /// in the way, so "not supported" would send someone looking for a flag
    /// that turns it on. `prepare_unsigned_send` is the only builder in the SDK
    /// that hands back a `PartialTransaction`; the token builders hand back an
    /// `Unsent<Sent>` at the earliest, which is already signed, because each
    /// one signs as it builds.
    #[error("--currency plans a token payment, and there is no unsigned form of one")]
    #[diagnostic(
        code(pecu::plan_has_no_token_path),
        help(
            "this is an SDK gap, not a mistake in what you asked for. The partial format is not \
             the blocker — it is started with whatever outputs it is handed. What is missing is a \
             builder: the SDK's token builders each sign as they build, so there is no unsigned \
             token payment to carry to the offline machine. `pecu send --currency NAME@ --amount \
             N --to ADDRESS --from LABEL` moves one, but it signs on the machine that talks to \
             the node, which is the thing the air gap exists to avoid — a real limit, not a \
             longer way round. Plan without --currency"
        )
    )]
    CannotPlanAToken,

    #[error("--from-identity spends what {identity} holds, and a plan cannot reach it")]
    #[diagnostic(code(pecu::plan_has_no_identity_path), help("{advice}"))]
    CannotPlanFromIdentity { identity: String, advice: String },

    #[error("that is not a partial transaction")]
    #[diagnostic(code(pecu::bad_plan), help("{detail}"))]
    BadPlan { detail: String },

    #[error("`{address}` is not a Verus address")]
    #[diagnostic(code(pecu::bad_address), help("transparent addresses start with R"))]
    BadAddress { address: String },

    #[error("no key to sign with")]
    #[diagnostic(
        code(pecu::no_key),
        help("pass --key <label>, or make one with `pecu key gen`")
    )]
    NoKey,

    #[error("the keystore holds {count} keys, so there is no obvious one to sign with")]
    #[diagnostic(code(pecu::ambiguous_key), help("name one with --key <label>"))]
    AmbiguousKey { count: usize },

    #[error("this key signed nothing — it does not unlock any of these inputs")]
    #[diagnostic(
        code(pecu::wrong_key),
        help("the plan spends outputs belonging to a different address. `pecu key list` shows what you hold")
    )]
    WrongKey,

    #[error("the inputs do not commit to the outputs")]
    #[diagnostic(
        code(pecu::partial_sighash),
        help("at least one input is signed under something other than SIGHASH_ALL, so the outputs shown are not what your signature protects — whoever holds this can redirect the money after you sign. Pass --yes only if you meant this")
    )]
    NotCommitted,

    #[error("still unsigned: {missing} of {total} input(s) have no signature")]
    #[diagnostic(
        code(pecu::incomplete),
        help("this needs another signer. Hand the partial on rather than broadcasting it")
    )]
    Incomplete { missing: usize, total: usize },

    #[error("cancelled")]
    #[diagnostic(code(pecu::cancelled), help("nothing was signed"))]
    Cancelled,

    #[error("cannot ask for confirmation")]
    #[diagnostic(
        code(pecu::no_tty),
        help("there is no terminal to confirm on — pass --yes once you have read the summary")
    )]
    CannotConfirm,

    #[error("--json will not broadcast without --yes")]
    #[diagnostic(
        code(pecu::needs_yes),
        help("--json is machine-readable output, not consent to spend: the confirmation prompt would go to the same stream you are parsing, and there is nobody to answer it. Add --yes to send these bytes, or --dry-run to stop at the decoded summary")
    )]
    NeedsYes,

    #[error("not enough spendable funds at {address}")]
    #[diagnostic(code(pecu::insufficient_funds), help("{advice}"))]
    Insufficient { address: String, advice: String },

    #[error("{what} failed")]
    #[diagnostic(code(pecu::flow_failed), help("{advice}"))]
    Flow {
        what: &'static str,
        advice: String,
        #[source]
        source: Box<FlowError>,
    },

    #[error("the transaction could not be assembled")]
    #[diagnostic(code(pecu::finalize_failed), help("{detail}"))]
    Finalize { detail: String },
}

fn flow(what: &'static str, source: FlowError) -> AirgapError {
    let advice = match &source {
        // `pecu broadcast` is the command someone reaches for to resend, so
        // sending them to `pecu doctor` here argues directly for the second
        // broadcast this variant exists to prevent.
        FlowError::BroadcastUncertain { txid, hex, .. } => uncertain_broadcast_advice(txid, hex),
        _ => "run `pecu doctor`, or point somewhere else with --node".to_string(),
    };
    AirgapError::Flow {
        what,
        advice,
        source: Box::new(source),
    }
}

/// `pecu send`'s closing sentence ends "cannot be moved by this key". Planning
/// is watch-only and holds no key at all, so this one blames nothing but the
/// address — and says only what is true of a *watch-only* shortfall: value the
/// node withholds. It deliberately does not repeat `send`'s "or held by a
/// VerusID", which reads as a contradiction here, since resolving a VerusID
/// name is exactly what `--address` does. What is true of that case is worth a
/// message of its own, below.
fn rest_is_elsewhere(address: &str) -> String {
    format!(
        "`pecu wallet balance --address {address}` shows what is there: value the node reports \
         as withheld — coinbase still maturing, or already spent by a transaction that has not \
         confirmed — cannot be planned."
    )
}

/// Planning from a VerusID is not a shortfall that funding it would fix.
///
/// `plan send --address bob@` resolves the name, so the request looks like it
/// worked and then reports nothing to spend. The reason is structural:
/// `prepare_unsigned_send` funds through `funding::spendable`, which keeps only
/// P2PKH outputs, and what an identity holds is a pay-to-identity output. So
/// the figure is always zero, and "add funds" is the one remedy that cannot
/// help.
fn a_verusid_holds_it_differently(address: &str) -> String {
    format!(
        "{address} is a VerusID, and {WHAT_AN_IDENTITY_HOLDS}, so planning finds none of it, \
         however much the identity is worth — funding it would not change that. `pecu send \
         --from-identity` moves those funds instead, signing with one of the identity's primary \
         keys; `pecu wallet balance --address {address}` shows what it holds."
    )
}

/// The one fact both identity-shaped refusals here turn on, written once
/// because they are the same limit reached from two directions: `--address
/// bob@` walks into it as a shortfall after the node has been asked, and
/// `--from-identity` asks for it outright. `prepare_unsigned_send` funds
/// through `funding::spendable`, which separates out every output that is not
/// plain P2PKH before selection ever sees it.
const WHAT_AN_IDENTITY_HOLDS: &str = "what a VerusID holds sits in pay-to-identity outputs, and \
                                      an unsigned plan can spend only the plain P2PKH kind";

/// The identity to name in the refusal, if there is one worth naming.
///
/// An empty or all-blank `--from-identity` is refused for exactly the same
/// reason as any other value, but it cannot be quoted back: it would leave a
/// pair of empty backticks in the sentence, and a remedy built around it —
/// `pecu wallet balance --address` with nothing after it — is a command that
/// does not run, which is the one thing every refusal here has to avoid.
/// Nothing else validates the value: a flag that is always refused has nothing
/// to validate it against, so this is only about the sentence staying true.
fn identity_worth_naming(identity: &str) -> Option<&str> {
    Some(identity.trim()).filter(|name| !name.is_empty())
}

/// Why `--from-identity` cannot be planned. Shares its middle sentence with the
/// shortfall a VerusID `--address` reports, because it is the same limit.
fn an_identity_spend_has_no_unsigned_form(identity: Option<&str>) -> String {
    // Named, these two are commands the reader can paste. Unnamed, they would
    // be commands missing an argument, so the shapes go instead.
    let (moves_them, shows_them) = match identity {
        Some(name) => (
            format!("`pecu send --from-identity {name} --amount N --to ADDRESS --from LABEL`"),
            format!("; `pecu wallet balance --address {name}` shows what the identity holds"),
        ),
        None => (
            "`pecu send --from-identity NAME@ --amount N --to ADDRESS --from LABEL`".to_string(),
            String::new(),
        ),
    };
    format!(
        "this is an SDK gap, not a mistake in what you asked for: {WHAT_AN_IDENTITY_HOLDS}, and \
         those inputs are unlocked by a fulfillment rather than by a signature and public key. \
         The builders that reach them each sign as they build. {moves_them} moves those funds, \
         signing on the machine that talks to the node{shows_them}. Plan without --from-identity, \
         from an address that holds its own coins"
    )
}

/// The two flags `pecu send` takes that no partial transaction can carry.
///
/// They are declared and parsed rather than left unknown on purpose: clap's
/// vocabulary can only say `unexpected argument '--currency'`, which reads as a
/// misspelling and sends the reader hunting for the right spelling of something
/// that does not exist. What they cost is nothing — refused here, ahead of the
/// node, for the reason the launch guards give: a flag that can never be
/// honoured should cost neither a prompt nor a round trip.
fn refuse_what_no_partial_can_carry(args: &PlanSendArgs) -> Result<(), AirgapError> {
    // Order shows only when both are passed, and either sentence is true then.
    // `--currency` goes first because it is the likelier thing to have copied
    // across from a working `pecu send`.
    if args.currency.is_some() {
        return Err(AirgapError::CannotPlanAToken);
    }
    if let Some(identity) = &args.from_identity {
        let named = identity_worth_naming(identity);
        return Err(AirgapError::CannotPlanFromIdentity {
            identity: named.map_or_else(|| "a VerusID".to_string(), |name| format!("`{name}`")),
            advice: an_identity_spend_has_no_unsigned_form(named),
        });
    }
    Ok(())
}

/// A shortfall is not a node problem, and `pecu plan send` used to report every
/// one of them as one.
///
/// The same two variants and the same two readings as `pecu send`'s
/// [`insufficient_or_flow`](crate::cmd::send) — planning goes through the same
/// `funding::require` and the same coin selection, so the pre-flight `needed`
/// excludes the fee here too, and selection's `required` includes it. There is
/// no token path to plan yet, so `TxError::InsufficientTokens` is deliberately
/// not diverted: it cannot arrive, and if it ever did there would be no name to
/// print.
fn insufficient_or_flow(
    what: &'static str,
    from: &Address,
    amount: Amount,
    source: FlowError,
) -> AirgapError {
    use verus_sdk::verus_tx::TxError;

    if let FlowError::InsufficientFunds {
        needed,
        available,
        address,
        utxos,
    } = &source
    {
        if from.kind() == AddressKind::Identity {
            return AirgapError::Insufficient {
                address: address.clone(),
                advice: a_verusid_holds_it_differently(address),
            };
        }
        return AirgapError::Insufficient {
            address: address.clone(),
            advice: format!(
                "{} spendable across {utxos} output(s), and this plans a payment of {}. {} {}",
                fmt::amount(*available),
                fmt::amount(*needed),
                what_the_fee_leaves(
                    available.to_sat(),
                    Fee::Estimated(estimated_native_fee(*utxos))
                ),
                rest_is_elsewhere(address),
            ),
        };
    }

    if let FlowError::Tx(TxError::InsufficientFunds {
        required,
        available,
    }) = &source
    {
        let fee = required.saturating_sub(amount.to_sat());
        let address = from.to_string();
        return AirgapError::Insufficient {
            advice: format!(
                "{} spendable, and {} plus a {} fee comes to {}. {} {}",
                fmt::sats(*available),
                fmt::amount(amount),
                fmt::sats(fee),
                fmt::sats(*required),
                // Selection priced the transaction it refused, so what is left
                // after that fee is the exact ceiling and may be named as one.
                what_the_fee_leaves(*available, Fee::Charged(fee)),
                rest_is_elsewhere(&address),
            ),
            address,
        };
    }

    flow(what, source)
}

// ── plan ────────────────────────────────────────────────────────────────────

/// Build an unsigned transaction. Holds no key and cannot sign.
pub fn plan_send(
    ui: &Ui,
    settings: &Settings,
    globals: &Globals,
    args: &PlanSendArgs,
) -> miette::Result<()> {
    let outcome = plan_send_inner(ui, settings, globals, args);
    // On stderr under `--json`; see `pecu send`.
    ui.explain_panel();
    outcome
}

fn plan_send_inner(
    ui: &Ui,
    settings: &Settings,
    _globals: &Globals,
    args: &PlanSendArgs,
) -> miette::Result<()> {
    // Ahead of the spending gate, because this refusal is true on every profile
    // and the other is true only on this one: a reader told to edit
    // `allow_spend` first, and told the flag can never work afterwards, has
    // been sent on an errand for nothing.
    refuse_what_no_partial_can_carry(args)?;

    // Planning is where a mainnet spend is chosen, even though it is broadcast
    // two machines later. Every other command gates before it builds, and a
    // plan that may never legally be broadcast is not worth the round trip.
    if !settings.profile.allow_spend {
        return Err(AirgapError::SpendingDisabled {
            profile: settings.profile.name.clone(),
        }
        .into());
    }

    let amount = Amount::from_coins_str(&args.amount).map_err(|_| AirgapError::BadAmount {
        amount: args.amount.clone(),
    })?;

    let node = node::connect(&settings.profile)?;

    // The same resolution the read-only commands use: this step is watch-only,
    // so a stored key contributes its address and nothing else. A VerusID name
    // resolves here too, which is what makes planning a spend *from* an
    // identity possible without looking its i-address up by hand.
    let from = wallet::resolve_address(
        ui,
        &node,
        settings,
        args.target.address.as_deref(),
        args.target.key.as_deref(),
    )?;
    let from = from.address;
    let from: Address = from.parse().map_err(|_| AirgapError::BadAddress {
        address: from.clone(),
    })?;
    ui.sdk(format!(
        "verus_sdk::network::prepare_unsigned_send(&node, &{from}, {:?}, Amount::from_coins_str({:?}))",
        args.to,
        amount.to_coins_string()
    ));
    let partial = prepare_unsigned_send(&node, &from, &args.to, amount)
        .map_err(|source| insufficient_or_flow("planning the payment", &from, amount, source))?;
    ui.sdk(format!(
        "PartialTransaction with {} input(s), {} output(s)",
        partial.inputs.len(),
        partial.outputs.len()
    ));

    let bytes = partial.to_bytes().map_err(|error| AirgapError::BadPlan {
        detail: error.to_string(),
    })?;
    let encoded = hex::encode(&bytes);

    if let Some(path) = &args.out {
        payload::write_hex(path, &encoded)?;
    }

    hand_over(ui, &args.qr, &encoded, "unsigned plan")?;

    if ui.is_json() {
        emit(&serde_json::json!({
            "kind": "plan",
            "from": from.to_string(),
            "to": args.to,
            "plan": encoded,
            "inputs": partial.inputs.len(),
            "outputs": partial.outputs.len(),
            "bytes": bytes.len(),
            "written_to": args.out,
        }));
        return Ok(());
    }

    let palette = ui.theme.palette;
    ui.panel(&summary_panel(
        ui,
        settings,
        "PLAN",
        &partial,
        Some(&from.to_string()),
    )?);
    ui.blank();
    let mut carried = Panel::new("UNSIGNED PLAN")
        .wrapped(0, Text::of(&encoded, palette.value))
        .note(Text::of(
            "no key was used and none is needed here — this cannot be broadcast until it is signed",
            palette.muted,
        ));
    if let Some(path) = &args.out {
        carried = carried.path("written to", path);
    }
    ui.panel(&carried);
    Ok(())
}

// ── sign ────────────────────────────────────────────────────────────────────

/// Sign a plan. Opens no socket.
pub fn sign(
    ui: &Ui,
    settings: &Settings,
    globals: &Globals,
    args: &SignArgs,
) -> miette::Result<()> {
    let bytes = if args.qr_in.is_empty() {
        payload::read_hex(args.input.as_deref())?
    } else {
        from_qr(&args.qr_in)?
    };
    let mut partial =
        PartialTransaction::from_bytes(&bytes).map_err(|error| AirgapError::BadPlan {
            detail: error.to_string(),
        })?;

    let summary = partial.summary().map_err(|error| AirgapError::BadPlan {
        detail: error.to_string(),
    })?;

    if !ui.is_json() {
        ui.panel(&summary_panel(
            ui,
            settings,
            "ABOUT TO SIGN",
            &partial,
            None,
        )?);
    }

    // The one check a co-signer cannot make by eye. Outputs are binding only if
    // every input commits to them; anything else and the money can be
    // redirected after the signature exists.
    if !summary.commits_to_all_outputs() && !globals.yes {
        return Err(AirgapError::NotCommitted.into());
    }
    if !globals.yes && !ui.is_json() {
        confirm(ui)?;
    }

    let store = Keystore::new(&settings.paths);
    let envelope = match args.key.as_deref() {
        Some(label) => store.load(label)?,
        None => {
            let keys = store.list()?;
            match keys.len() {
                0 => return Err(AirgapError::NoKey.into()),
                1 => keys.into_iter().next().expect("just checked"),
                count => return Err(AirgapError::AmbiguousKey { count }.into()),
            }
        }
    };
    let secret = keystore::passphrase(&format!("passphrase for `{}`", envelope.label), false)?;
    let key = envelope.unlock(&secret)?;

    ui.sdk("partial.sign(&key)");
    let signed_count = partial.sign(&key).map_err(|error| AirgapError::BadPlan {
        detail: error.to_string(),
    })?;
    ui.sdk_result(format!("{signed_count} input(s) signed"));
    if signed_count == 0 {
        return Err(AirgapError::WrongKey.into());
    }

    // Not complete means another signer is still needed — hand on the partial,
    // not a transaction that cannot be mined.
    if !partial.is_complete() {
        let unsigned = partial
            .summary()
            .map(|s| s.signatures_per_input.iter().filter(|n| **n == 0).count())
            .unwrap_or(0);
        let encoded = hex::encode(partial.to_bytes().map_err(|error| AirgapError::BadPlan {
            detail: error.to_string(),
        })?);
        if let Some(path) = &args.out {
            payload::write_hex(path, &encoded)?;
        }
        if ui.is_json() {
            emit(&serde_json::json!({
                "kind": "partially_signed",
                "complete": false,
                "signed_inputs": signed_count,
                "partial": encoded,
            }));
        } else {
            ui.blank();
            ui.panel(
                &Panel::new("PARTIALLY SIGNED")
                    .wrapped(0, Text::of(&encoded, ui.theme.palette.value))
                    .note(Text::of(
                        "another signature is still needed — pass this on, do not broadcast it",
                        ui.theme.palette.warn,
                    )),
            );
        }
        return Err(AirgapError::Incomplete {
            missing: unsigned,
            total: partial.inputs.len(),
        }
        .into());
    }

    ui.sdk("partial.finalize()");
    let finished = partial.finalize().map_err(|error| AirgapError::Finalize {
        detail: error.to_string(),
    })?;
    ui.sdk_result(format!("SignedTransaction {{ txid: {} }}", finished.txid));

    if let Some(path) = &args.out {
        payload::write_hex(path, &finished.hex)?;
    }

    hand_over(ui, &args.qr, &finished.hex, "signed transaction")?;

    if ui.is_json() {
        emit(&serde_json::json!({
            "kind": "signed",
            "complete": true,
            "txid": finished.txid,
            "hex": finished.hex,
            "fee": finished.fee.to_sat(),
            "written_to": args.out,
        }));
        return Ok(());
    }

    let palette = ui.theme.palette;
    ui.blank();
    let mut done = Panel::new("SIGNED TRANSACTION")
        .row("txid", Text::of(&finished.txid, palette.accent))
        .rule()
        .wrapped(0, Text::of(&finished.hex, palette.value))
        .note(Text::of(
            "carry this back to a machine with a node and run `pecu broadcast`",
            palette.muted,
        ));
    if let Some(path) = &args.out {
        done = done.path("written to", path);
    }
    ui.panel(&done);
    ui.explain_panel();
    Ok(())
}

// ── broadcast ───────────────────────────────────────────────────────────────

/// Hand finished bytes to a node. Carries no key.
pub fn broadcast(
    ui: &Ui,
    settings: &Settings,
    globals: &Globals,
    args: &BroadcastArgs,
) -> miette::Result<()> {
    // Before the payload is even read. Everything else this command refuses is
    // a property of the bytes; this is a property of the profile, and a profile
    // that may not spend may not hand finished bytes to a node either.
    if !settings.profile.allow_spend {
        return Err(AirgapError::SpendingDisabled {
            profile: settings.profile.name.clone(),
        }
        .into());
    }

    let bytes = if args.qr_in.is_empty() {
        payload::read_hex(args.input.as_deref())?
    } else {
        from_qr(&args.qr_in)?
    };
    let encoded = hex::encode(&bytes);

    // Decoded locally first, so the last thing before the network is still a
    // description of what is about to be sent rather than an opaque blob.
    let transaction =
        verus_sdk::verus_wire::TxV4::deserialize(&bytes).map_err(|error| AirgapError::BadPlan {
            detail: format!("not a finished transaction: {error}"),
        })?;
    let txid = transaction
        .txid()
        .map(|mut id| {
            id.reverse();
            hex::encode(id)
        })
        .map_err(|error| AirgapError::BadPlan {
            detail: error.to_string(),
        })?;

    if !ui.is_json() {
        let palette = ui.theme.palette;
        ui.panel(
            &Panel::new(if globals.dry_run {
                "WOULD BROADCAST"
            } else {
                "ABOUT TO BROADCAST"
            })
            .row("txid", Text::of(&txid, palette.accent))
            .row(
                "outputs",
                Text::of(
                    fmt::plural(transaction.outputs.len(), "output", "outputs"),
                    palette.value,
                ),
            )
            .row(
                "value",
                Text::of(
                    fmt::total(tx::total_output_value(&transaction)),
                    palette.value,
                )
                .space()
                .push(&settings.profile.currency, palette.muted),
            ),
        );
    }

    // `--dry-run` is documented as "never broadcast" (src/cli.rs, README) and
    // was a silent no-op here: this command went to the wire with the flag set.
    // Decoded, described, and stopped.
    if globals.dry_run {
        if ui.is_json() {
            emit(&serde_json::json!({ "kind": "broadcast", "txid": txid, "broadcast": false }));
            return Ok(());
        }
        ui.blank();
        ui.note("nothing was sent. Drop --dry-run to broadcast it");
        ui.explain_panel();
        return Ok(());
    }

    if !globals.yes {
        // The consent used to live inside the `!ui.is_json()` block above,
        // which made `--json` a spending flag — the same bug `pecu send` fixed
        // in src/cmd/send.rs. The prompt writes to the stream being parsed and
        // nobody is there to answer it, so consent has to be passed in.
        if ui.is_json() {
            return Err(AirgapError::NeedsYes.into());
        }
        confirm(ui)?;
    }

    let node = node::connect(&settings.profile)?;
    ui.sdk(format!(
        "verus_sdk::network::broadcast(&node, <{} bytes>, {txid:?})",
        bytes.len()
    ));
    let accepted = submit(&node, &encoded, &txid).map_err(|source| flow("broadcasting", source))?;
    ui.sdk_result(format!("txid {accepted}"));

    if ui.is_json() {
        emit(&serde_json::json!({ "kind": "broadcast", "txid": accepted, "broadcast": true }));
        return Ok(());
    }
    ui.blank();
    ui.ok(format!("broadcast — txid {accepted}"));
    ui.note(format!(
        "{}/tx/{accepted}",
        settings.profile.explorer.trim_end_matches('/')
    ));
    ui.explain_panel();
    Ok(())
}

// ── shared ──────────────────────────────────────────────────────────────────

/// What this transaction does, as the SDK's own [`Summary`] describes it.
fn summary_panel(
    ui: &Ui,
    settings: &Settings,
    title: &str,
    partial: &PartialTransaction,
    from: Option<&str>,
) -> Result<Panel, AirgapError> {
    let summary = partial.summary().map_err(|error| AirgapError::BadPlan {
        detail: error.to_string(),
    })?;
    let palette = ui.theme.palette;
    let glyphs = ui.theme.glyphs;
    let currency = &settings.profile.currency;

    let mut panel = Panel::new(title);
    if let Some(from) = from {
        panel = panel.row("from", Text::of(from, palette.value));
    }
    panel = panel
        .row(
            "spending",
            Text::of(fmt::amount(summary.total_in), palette.value)
                .space()
                .push(currency, palette.muted)
                .space()
                .push(
                    format!(
                        "across {}",
                        fmt::plural(partial.inputs.len(), "input", "inputs")
                    ),
                    palette.muted,
                ),
        )
        .row(
            "paying out",
            Text::of(fmt::amount(summary.total_out), palette.accent)
                .space()
                .push(currency, palette.muted),
        )
        .row(
            "fee and burn",
            Text::of(fmt::amount(summary.fee_and_burn), palette.value)
                .space()
                .push(currency, palette.muted),
        )
        .row("expiry", expiry_text(ui, partial));

    panel = panel.row("commits", commitment(ui, &summary));

    panel = panel.section("OUTPUTS");
    for (index, (amount, address)) in summary.outputs.iter().enumerate() {
        // Built as spans, never as one Text's `render()` pushed into another:
        // the escapes in a rendered string are counted as visible width, and the
        // frame comes out ragged. The kit exists to make that impossible; going
        // around it puts it straight back.
        let (destination, style) = match address {
            Some(address) => (address.to_string(), palette.value),
            // `Summary` decodes plain key-hash outputs only. Anything else is a
            // CryptoCondition it will not guess at, and neither will this.
            None => (
                "a CryptoCondition — read the script before signing".to_string(),
                palette.warn,
            ),
        };
        panel = panel
            .line(
                Text::of(format!("#{index}"), palette.muted)
                    .space()
                    .push(fmt::amount(*amount), palette.accent)
                    .space()
                    .push(currency, palette.muted),
            )
            .wrapped(
                5,
                Text::of(glyphs.arrow, palette.muted)
                    .space()
                    .push(destination, style),
            );
    }
    Ok(panel)
}

/// The sighash check, spelled out. This is the difference between "these are the
/// outputs" and "these are the outputs your signature protects".
fn commitment(ui: &Ui, summary: &Summary) -> Text {
    let palette = ui.theme.palette;
    let glyphs = ui.theme.glyphs;
    if summary.commits_to_all_outputs() {
        Text::of(glyphs.ok, palette.ok).space().push(
            "every input covers every output (SIGHASH_ALL)",
            palette.value,
        )
    } else {
        Text::of(glyphs.danger, palette.danger).space().push(
            "NOT every input covers every output — the money can be redirected after you sign",
            palette.danger,
        )
    }
}

fn expiry_text(ui: &Ui, partial: &PartialTransaction) -> Text {
    let palette = ui.theme.palette;
    match partial.expiry.to_height() {
        0 => Text::of(ui.theme.glyphs.warn, palette.warn)
            .space()
            .push("never — this stays minable forever", palette.warn),
        height => Text::of(
            format!("height {}", fmt::height(height.into())),
            palette.value,
        ),
    }
}

fn confirm(ui: &Ui) -> Result<(), AirgapError> {
    if !std::io::stdin().is_terminal() {
        return Err(AirgapError::CannotConfirm);
    }
    ui.blank();
    print!("  type `yes` to continue: ");
    let _ = std::io::stdout().flush();
    let mut answer = String::new();
    std::io::stdin()
        .read_line(&mut answer)
        .map_err(|_| AirgapError::CannotConfirm)?;
    if answer.trim() != "yes" {
        return Err(AirgapError::Cancelled);
    }
    Ok(())
}

/// Draw and/or write the payload as QR codes, if asked.
///
/// Chunked and numbered, so several frames reassemble in any order — see
/// [`crate::ui::qr`]. Nothing happens unless a flag asked for it: a wall of
/// block characters is not what most runs want.
fn hand_over(ui: &Ui, options: &QrOut, hex: &str, what: &str) -> Result<(), miette::Report> {
    if !options.qr && options.qr_out.is_none() {
        return Ok(());
    }
    let frames = qr::frames(hex);
    let palette = ui.theme.palette;

    // Drawing is display and belongs to the rendered path only; writing a PNG
    // is a side effect the caller asked for and happens either way. `--json
    // --qr-out` silently producing no file was a bug.
    if options.qr && !ui.is_json() {
        for (index, frame) in frames.iter().enumerate() {
            ui.blank();
            ui.line(Text::of(
                format!("{what} — frame {} of {}", index + 1, frames.len()),
                palette.label,
            ));
            print!("{}", qr::render(frame)?);
        }
    }

    if let Some(stem) = &options.qr_out {
        let mut written = Vec::new();
        for (index, frame) in frames.iter().enumerate() {
            let path = numbered(stem, index + 1);
            qr::write_png(&path, frame)?;
            written.push(path);
        }
        if !ui.is_json() {
            ui.blank();
            let mut panel = Panel::new("QR FRAMES");
            for path in &written {
                panel = panel.path("wrote", path);
            }
            ui.panel(&panel.note(Text::of(
                "every frame is needed; they reassemble in any order",
                palette.muted,
            )));
        }
    }
    Ok(())
}

/// `plan.png` with frame 2 of 3 becomes `plan-2.png`.
fn numbered(stem: &std::path::Path, index: usize) -> std::path::PathBuf {
    let extension = stem
        .extension()
        .map(|ext| ext.to_string_lossy().to_string())
        .unwrap_or_else(|| "png".to_string());
    stem.with_extension("").with_file_name(format!(
        "{}-{index}.{extension}",
        stem.with_extension("")
            .file_name()
            .map(|name| name.to_string_lossy().to_string())
            .unwrap_or_else(|| "frame".to_string())
    ))
}

/// Read a payload from QR codes in one or more PNGs.
fn from_qr(paths: &[std::path::PathBuf]) -> Result<Vec<u8>, miette::Report> {
    let mut frames = Vec::new();
    for path in paths {
        frames.extend(qr::read_png(path)?);
    }
    let hex = qr::reassemble(&frames)?;
    Ok(crate::payload::read_hex(Some(&hex))?)
}

fn emit(value: &serde_json::Value) {
    crate::failure::document(value);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `pecu broadcast` is the command someone reaches for to resend, so wrong
    /// advice here argues directly for the double broadcast this variant exists
    /// to prevent.
    #[test]
    fn an_uncertain_broadcast_does_not_blame_the_node() {
        // The advice saves the signed bytes, so it needs somewhere that is not the
        // real keystore root to save them into.
        let _unsent = crate::cmd::UnsentRoot::temporary();
        let refused = flow(
            "broadcasting",
            FlowError::BroadcastUncertain {
                txid: "9c1d55".into(),
                hex: "0400008085202f89".into(),
                reason: "node returned error -25: bad-txns-failed-precheck".into(),
            },
        );
        let AirgapError::Flow { advice, .. } = refused else {
            panic!("a flow refusal is a AirgapError::Flow");
        };
        assert!(advice.contains("tx explain 9c1d55"));
        assert!(!advice.contains("doctor"));
    }

    /// The address a plan pays from, parsed. Every shortfall message is
    /// written for one, and the identity case says something different.
    fn paying(address: &str) -> Address {
        address.parse().expect("a valid address")
    }

    /// `plan send` had no shortfall message at all: every one of them, at
    /// either level, was reported as a failure of a node that was answering
    /// perfectly well.
    #[test]
    fn a_plan_that_cannot_be_funded_does_not_blame_the_node() {
        let refused = insufficient_or_flow(
            "planning the payment",
            &paying("RXQXEHraYWzrNy65Ni1fiEpd7bY1bqDmWt"),
            Amount::from_sat(100_000_000),
            FlowError::InsufficientFunds {
                needed: Amount::from_sat(100_000_000),
                available: Amount::from_sat(0),
                address: "RXQXEHraYWzrNy65Ni1fiEpd7bY1bqDmWt".into(),
                utxos: 0,
            },
        );
        let AirgapError::Insufficient { address, advice } = refused else {
            panic!("a shortfall is not a flow failure: {refused:?}");
        };
        assert_eq!(address, "RXQXEHraYWzrNy65Ni1fiEpd7bY1bqDmWt");
        assert!(!advice.contains("doctor"), "{advice}");
        assert!(!advice.contains("including fee"), "{advice}");
        assert!(
            advice.contains("There is nothing at this address to pay with"),
            "{advice}"
        );
        // Where the fee sits explains nothing when there is nothing at all.
        assert!(!advice.contains("charged on top"), "{advice}");
        // Planning holds no key, so it cannot be the key's fault.
        assert!(!advice.contains("this key"), "{advice}");
    }

    /// The remedy has to be runnable as printed. A bare `pecu wallet balance`
    /// refuses as soon as the keystore holds more than one key, and a planned
    /// address is watch-only and often no stored key at all.
    #[test]
    fn the_remedy_names_the_address_it_wants_looked_up() {
        let refused = insufficient_or_flow(
            "planning the payment",
            &paying("RXQXEHraYWzrNy65Ni1fiEpd7bY1bqDmWt"),
            Amount::from_sat(100_000_000),
            FlowError::InsufficientFunds {
                needed: Amount::from_sat(100_000_000),
                available: Amount::from_sat(10_000_000),
                address: "RXQXEHraYWzrNy65Ni1fiEpd7bY1bqDmWt".into(),
                utxos: 1,
            },
        );
        let AirgapError::Insufficient { advice, .. } = refused else {
            panic!("a shortfall is not a flow failure: {refused:?}");
        };
        assert!(
            advice.contains("`pecu wallet balance --address RXQXEHraYWzrNy65Ni1fiEpd7bY1bqDmWt`"),
            "{advice}"
        );
    }

    /// The pre-flight figure is an estimate, priced high, so the sentence built
    /// on it must not claim to name a maximum — that is the "including fee"
    /// mistake in a different sentence.
    #[test]
    fn an_estimated_fee_does_not_claim_to_name_the_ceiling() {
        let refused = insufficient_or_flow(
            "planning the payment",
            &paying("RXQXEHraYWzrNy65Ni1fiEpd7bY1bqDmWt"),
            Amount::from_sat(100_000_000),
            FlowError::InsufficientFunds {
                needed: Amount::from_sat(100_000_000),
                available: Amount::from_sat(50_000_000),
                address: "RXQXEHraYWzrNy65Ni1fiEpd7bY1bqDmWt".into(),
                utxos: 2,
            },
        );
        let AirgapError::Insufficient { advice, .. } = refused else {
            panic!("a shortfall is not a flow failure: {refused:?}");
        };
        // Two inputs, so the estimate sits on the 10,000-satoshi floor.
        assert!(
            advice.contains("a payment of 0.49990000 will go through"),
            "{advice}"
        );
        assert!(!advice.contains("the most that can move"), "{advice}");
    }

    /// Planning from a VerusID resolves, then finds nothing — and the old
    /// closing sentence told the reader that VerusID-held value cannot be
    /// planned from this address, which is the thing they had just asked for.
    #[test]
    fn planning_from_a_verusid_says_why_it_is_empty_and_what_moves_it() {
        let identity = Address::new(AddressKind::Identity, [0x3f; 20]);
        let refused = insufficient_or_flow(
            "planning the payment",
            &identity,
            Amount::from_sat(100_000_000),
            FlowError::InsufficientFunds {
                needed: Amount::from_sat(100_000_000),
                available: Amount::from_sat(0),
                address: identity.to_string(),
                utxos: 0,
            },
        );
        let AirgapError::Insufficient { address, advice } = refused else {
            panic!("a shortfall is not a flow failure: {refused:?}");
        };
        assert_eq!(address, identity.to_string());
        assert!(!advice.contains("doctor"), "{advice}");
        assert!(advice.contains("pay-to-identity outputs"), "{advice}");
        assert!(advice.contains("--from-identity"), "{advice}");
        // Adding coins to an i-address does not make a plan possible, so the
        // message must not read as "top it up and try again".
        assert!(
            advice.contains("funding it would not change that"),
            "{advice}"
        );
    }

    #[test]
    fn a_plan_the_fee_tips_over_names_the_amount_that_works() {
        let refused = insufficient_or_flow(
            "planning the payment",
            &paying("RQC1EG3GhZ9pvT9YgCp3YvxyYBsdb4FYfH"),
            Amount::from_sat(39_999_979_600),
            FlowError::Tx(verus_sdk::verus_tx::TxError::InsufficientFunds {
                required: 39_999_989_600,
                available: 39_999_979_600,
            }),
        );
        let AirgapError::Insufficient { address, advice } = refused else {
            panic!("a shortfall is not a flow failure: {refused:?}");
        };
        assert_eq!(address, "RQC1EG3GhZ9pvT9YgCp3YvxyYBsdb4FYfH");
        assert!(!advice.contains("doctor"), "{advice}");
        assert!(advice.contains("0.00010000 fee"), "{advice}");
        // The fee here priced the refused transaction, so this one is exact.
        assert!(
            advice.contains("the most that can move from here is 399.99969600"),
            "{advice}"
        );
    }

    /// Widening far enough to swallow a genuine failure would be the same
    /// mistake as the one being fixed, pointing the other way.
    #[test]
    fn a_genuine_planning_failure_still_reaches_the_node_remedy() {
        use verus_sdk::verus_tx::TxError;
        let failures = [
            FlowError::Tx(TxError::ValueOverflow),
            FlowError::Tx(TxError::UnsupportedRecipient),
            FlowError::Stalled("the node stopped answering".into()),
        ];
        for source in failures {
            let described = source.to_string();
            let refused = insufficient_or_flow(
                "planning the payment",
                &paying("RQC1EG3GhZ9pvT9YgCp3YvxyYBsdb4FYfH"),
                Amount::from_sat(1),
                source,
            );
            let AirgapError::Flow { advice, .. } = refused else {
                panic!("{described} is not a shortfall, but was reported as one");
            };
            assert!(advice.contains("doctor"), "{described}: {advice}");
        }
    }
}
