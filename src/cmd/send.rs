//! `pecu send` — the first command that moves money.
//!
//! The order matters and is deliberate: unlock the key, build and sign
//! **locally**, show you the finished transaction decoded output by output, and
//! only then offer to broadcast. Nothing leaves the machine until you have seen
//! what it says.
//!
//! The dry run is enforced by the SDK's types rather than by remembering not to
//! call something. [`prepare_send`] takes a `ChainReader` and no `Broadcaster`,
//! so the value it returns is *incapable* of being sent; broadcasting is a
//! second, explicit step on [`Unsent`].

use std::io::{IsTerminal, Write};

use miette::Diagnostic;
use thiserror::Error;
use verus_sdk::decode::{decode_output_script, OutputKind};
use verus_sdk::money::{Amount, Utxo};
use verus_sdk::network::{
    prepare_send, prepare_send_from_identity, prepare_send_token, spendable, ChainReader,
    FlowError, RpcError, Unsent,
};
use verus_sdk::send::CurrencyId;
use verus_sdk::verus_keys::{Address, AddressKind, PrivateKey};
use verus_sdk::verus_wire::TxV4;

use crate::cli::{Globals, SendArgs};
use crate::config::Settings;
use crate::keystore::{self, Envelope, Keystore};
use crate::node::{self, Node};
use crate::ui::{fmt, Panel, Text, Ui};

/// How much of an identity name is ever printed on the review panel.
const IDENTITY_BUDGET: usize = 40;

#[derive(Debug, Error, Diagnostic)]
pub enum SendError {
    #[error("the `{profile}` profile is not allowed to spend")]
    #[diagnostic(
        code(pecu::spending_disabled),
        help("set `allow_spend = true` under [profiles.{profile}] in config.toml. It ships off for mainnet on purpose: moving real coins out of an example app should be a deliberate act")
    )]
    SpendingDisabled { profile: String },

    #[error("`{amount}` is not an amount")]
    #[diagnostic(
        code(pecu::bad_amount),
        help("a decimal number of coins, at most eight places: 1, 0.5, 0.00000001")
    )]
    BadAmount { amount: String },

    #[error("no key to spend from")]
    #[diagnostic(
        code(pecu::no_key),
        help("pass --from <label>, or make a key with `pecu key gen`")
    )]
    NoKey,

    #[error("the keystore holds {count} keys, so there is no obvious one to spend from")]
    #[diagnostic(
        code(pecu::ambiguous_key),
        help("name one with --from <label>; `pecu key list` shows them")
    )]
    AmbiguousKey { count: usize },

    #[error("nothing on this chain is called `{name}`")]
    #[diagnostic(
        code(pecu::unknown_recipient),
        help("VerusID names end with @, as in `bob@`. An address works too")
    )]
    UnknownRecipient { name: String },

    #[error("`{name}` is revoked")]
    #[diagnostic(
        code(pecu::revoked_recipient),
        help("a revoked identity can still receive, but its keys no longer control it — paying it may be paying nobody")
    )]
    RevokedRecipient { name: String },

    #[error("not enough spendable funds at {address}")]
    #[diagnostic(code(pecu::insufficient_funds), help("{advice}"))]
    Insufficient { address: String, advice: String },

    #[error("{address} holds no {currency} to send")]
    #[diagnostic(
        code(pecu::no_token_outputs),
        help("`pecu wallet balance` lists the tokens this address holds")
    )]
    NoTokenOutputs { address: String, currency: String },

    #[error("cancelled")]
    #[diagnostic(code(pecu::cancelled), help("nothing was broadcast"))]
    Cancelled,

    #[error("cannot ask for confirmation")]
    #[diagnostic(
        code(pecu::no_tty),
        help("there is no terminal to confirm on — pass --yes to send without asking, or --dry-run to stop before broadcasting")
    )]
    CannotConfirm,

    #[error("--json will not broadcast without --yes")]
    #[diagnostic(
        code(pecu::needs_yes),
        help("--json is machine-readable output, not consent to spend: the confirmation prompt would go to the same stream you are parsing, and there is nobody to answer it. Add --yes to send, or --dry-run to stop at the signed bytes")
    )]
    NeedsYes,

    #[error("{what} failed")]
    #[diagnostic(code(pecu::flow_failed), help("{advice}"))]
    Flow {
        what: &'static str,
        advice: String,
        #[source]
        source: Box<FlowError>,
    },
}

pub fn run(ui: &Ui, settings: &Settings, globals: &Globals, args: &SendArgs) -> miette::Result<()> {
    let outcome = attempt(ui, settings, globals, args);
    // Printed on the way out whatever happened. `--explain` earns its keep most
    // when something went wrong, and swallowing the record on the error path
    // would hide the call that failed.
    if !ui.is_json() {
        ui.explain_panel();
    }
    outcome
}

fn attempt(ui: &Ui, settings: &Settings, globals: &Globals, args: &SendArgs) -> miette::Result<()> {
    let profile = &settings.profile;
    if !profile.allow_spend {
        return Err(SendError::SpendingDisabled {
            profile: profile.name.clone(),
        }
        .into());
    }

    let amount = Amount::from_coins_str(&args.amount).map_err(|_| SendError::BadAmount {
        amount: args.amount.clone(),
    })?;
    let store = Keystore::new(&settings.paths);
    let envelope = choose_key(&store, args.from.as_deref())?;
    let node = node::connect(profile)?;

    let recipient = resolve_recipient(ui, &node, &args.to)?;
    let currency = match &args.currency {
        Some(name) => Some(resolve_currency(ui, &node, name)?),
        None => None,
    };

    // Unlocked before building because signing needs it, and after every check
    // that can fail without it — a passphrase prompt for a send that was never
    // going to work is a waste of the one interaction this command demands.
    let secret = keystore::passphrase(&format!("passphrase for `{}`", envelope.label), false)?;
    let key = envelope.unlock(&secret)?;

    let unsent = match (&args.from_identity, currency) {
        (Some(identity), _) => build_from_identity(ui, &node, &key, identity, &recipient, amount)?,
        (None, None) => build_native(ui, &node, &key, &recipient, amount)?,
        (None, Some(currency)) => build_token(ui, &node, &key, &recipient, amount, currency)?,
    };

    let decoded =
        TxV4::deserialize(&hex::decode(&unsent.hex).expect("the SDK just produced this hex")).ok();

    // Built before the broadcast consumes `unsent`, and printed at exactly one
    // point on each path below. The signed hex is the one thing here that cannot
    // be recovered afterwards, so every exit carries it — including the failing
    // one, where it is what lets you find out what actually happened.
    let plan = plan_json(
        &unsent,
        &decoded,
        args.from_identity.as_deref().unwrap_or(&envelope.address),
        args.from_identity.as_deref(),
    );

    if !ui.is_json() {
        ui.panel(&review(
            ui,
            settings,
            &envelope,
            args.from_identity.as_deref(),
            &recipient,
            amount,
            &unsent,
            &decoded,
        ));
    }

    if globals.dry_run {
        if ui.is_json() {
            emit_json(plan, Delivery::Held);
            return Ok(());
        }
        // The bytes themselves, not just the reading of them. Without these
        // a dry run in a terminal has nothing to hand to anything else —
        // and they are what an air-gapped signer would carry across.
        ui.blank();
        ui.panel(
            &Panel::new("SIGNED TRANSACTION")
                .wrapped(0, Text::of(&unsent.hex, ui.theme.palette.value))
                .note(Text::of(
                    "nothing was broadcast. `pecu tx explain <hex>` reads this back, \
                     and `--json` gives it unwrapped",
                    ui.theme.palette.muted,
                )),
        );
        return Ok(());
    }

    if !globals.yes {
        // `--json` used to skip this silently, which made it a spending flag:
        // `pecu send --json` broadcast without asking anyone. The prompt cannot
        // run here — it writes to stdout, and there is nobody to read it — so
        // the consent has to arrive as `--yes` instead of being assumed.
        if ui.is_json() {
            return Err(SendError::NeedsYes.into());
        }
        confirm(ui)?;
    }

    ui.sdk("unsent.broadcast(&node)");
    let sent = match unsent.broadcast(&node) {
        Ok(sent) => sent,
        Err(source) => {
            ui.sdk_result(format!("Err({source})"));
            if ui.is_json() {
                emit_json(plan, Delivery::Failed(&source));
            }
            return Err(flow("broadcasting", source).into());
        }
    };
    ui.sdk_result(format!("Sent {{ txid: {} }}", sent.txid));

    if ui.is_json() {
        emit_json(plan, Delivery::Accepted(&sent));
    } else {
        ui.blank();
        ui.ok(format!("broadcast — txid {}", sent.txid));
        ui.note(format!(
            "{}/tx/{}",
            settings.profile.explorer.trim_end_matches('/'),
            sent.txid
        ));
    }
    Ok(())
}

/// Which key pays. Same rule as the read-only commands: name one, or have
/// exactly one. Guessing between several would spend from the wrong address.
fn choose_key(store: &Keystore, label: Option<&str>) -> Result<Envelope, miette::Report> {
    if let Some(label) = label {
        return Ok(store.load(label)?);
    }
    let keys = store.list()?;
    match keys.len() {
        0 => Err(SendError::NoKey.into()),
        1 => Ok(keys.into_iter().next().expect("just checked")),
        count => Err(SendError::AmbiguousKey { count }.into()),
    }
}

/// Where the money goes: an address as given, or a VerusID name looked up.
struct Recipient {
    /// What the SDK is handed — always an address.
    address: String,
    /// What to show. The name if there was one, so the confirmation says what
    /// you typed rather than what it resolved to.
    shown: String,
}

fn resolve_recipient(ui: &Ui, node: &Node, to: &str) -> Result<Recipient, miette::Report> {
    if to.parse::<Address>().is_ok() {
        return Ok(Recipient {
            address: to.to_string(),
            shown: to.to_string(),
        });
    }

    ui.sdk(format!("node.identity({to:?})"));
    let record = node.identity(to).map_err(|_| SendError::UnknownRecipient {
        name: to.to_string(),
    })?;
    ui.sdk_result(format!(
        "IdentityRecord {{ identity_address: {}, status: {} }}",
        record.identity_address, record.status
    ));

    // A revoked identity can still be paid, and the money is very likely gone:
    // whoever held the keys no longer controls it.
    if record.is_revoked() {
        return Err(SendError::RevokedRecipient {
            name: to.to_string(),
        }
        .into());
    }
    Ok(Recipient {
        address: record.identity_address,
        shown: format!("{to} ({})", record.fully_qualified_name),
    })
}

fn resolve_currency(ui: &Ui, node: &Node, name: &str) -> Result<CurrencyId, miette::Report> {
    if let Ok(address) = name.parse::<Address>() {
        if address.kind() == AddressKind::Identity {
            return Ok(CurrencyId::from_bytes(address.hash()));
        }
    }
    ui.sdk(format!("node.identity({name:?})"));
    let record = node
        .identity(name)
        .map_err(|_| SendError::UnknownRecipient {
            name: name.to_string(),
        })?;
    ui.sdk_result(format!("identity_address: {}", record.identity_address));
    let address: Address =
        record
            .identity_address
            .parse()
            .map_err(|_| SendError::UnknownRecipient {
                name: name.to_string(),
            })?;
    Ok(CurrencyId::from_bytes(address.hash()))
}

fn build_native(
    ui: &Ui,
    node: &Node,
    key: &PrivateKey,
    to: &Recipient,
    amount: Amount,
) -> Result<Unsent<verus_sdk::network::Sent>, miette::Report> {
    ui.sdk(format!(
        "verus_sdk::network::prepare_send(&node, &key, {:?}, Amount::from_coins_str({:?}))",
        to.address,
        amount.to_coins_string()
    ));
    let unsent = prepare_send(node, key, &to.address, amount)
        .map_err(|source| insufficient_or_flow("building the payment", source))?;
    ui.sdk_result(format!(
        "Unsent<Sent> {{ txid: {}, fee: {}, change: {} }}",
        unsent.outcome.txid, unsent.outcome.fee, unsent.outcome.change
    ));
    Ok(unsent)
}

/// Pay out of what a VerusID holds, rather than out of the key's own address.
///
/// A different signature from an ordinary spend: the inputs are the identity's
/// pay-to-identity outputs, each carrying a fulfillment rather than a
/// scriptSig, and the surplus returns to the identity rather than to the key.
/// This is the everyday shape of money on Verus — funds live under an identity
/// — and it is the other half of what `wallet balance` reports as HELD BY ID.
///
/// The SDK refuses ahead of time everything the chain would refuse later with a
/// message that names nothing: a revoked identity, a key the identity does not
/// list, or fewer distinct keys than its `minimumsignatures`.
fn build_from_identity(
    ui: &Ui,
    node: &Node,
    key: &PrivateKey,
    identity: &str,
    to: &Recipient,
    amount: Amount,
) -> Result<Unsent<verus_sdk::network::Sent>, miette::Report> {
    ui.sdk(format!(
        "verus_sdk::network::prepare_send_from_identity(&node, &[&key], {identity:?}, {:?}, {})",
        to.address,
        amount.to_coins_string()
    ));
    let unsent = prepare_send_from_identity(node, &[key], identity, &to.address, amount)
        .map_err(|source| identity_spend_error(identity, source))?;
    ui.sdk_result(format!(
        "Unsent<Sent> {{ txid: {}, fee: {}, change: {} }}",
        unsent.outcome.txid, unsent.outcome.fee, unsent.outcome.change
    ));
    Ok(unsent)
}

/// Turn the SDK's pre-flight refusals into something that says what to do.
fn identity_spend_error(identity: &str, source: FlowError) -> SendError {
    use verus_sdk::verus_tx::TxError;

    // `TxError::InsufficientFunds`, not `FlowError::InsufficientFunds`. The
    // identity path funds itself from the identity's own outputs and runs out
    // inside the builder, so the flow-level variant — the one the ordinary send
    // produces — is never reached here.
    //
    // It needs its own wording either way. The ordinary message ends "value
    // held by a VerusID cannot be moved by this key", which is exactly what
    // this command is doing, and repeating it would contradict the operation
    // being attempted.
    if let FlowError::Tx(verus_sdk::verus_tx::TxError::InsufficientFunds {
        required,
        available,
    }) = &source
    {
        return SendError::Insufficient {
            address: identity.to_string(),
            advice: format!(
                "{identity} itself holds {} and {} is needed including the fee. This spends what \
                 the identity holds, not what the signing key holds — pay the identity to fund \
                 it, and `pecu wallet balance` on its i-address is the figure that matters.",
                fmt::sats(*available),
                fmt::sats(*required),
            ),
        };
    }

    let advice = match &source {
        FlowError::NoSuchIdentity(_) => {
            "check the name — `pecu id show <name@>` reads it off the chain".to_string()
        }
        // Refused before the transaction is built, since verus-rust-sdk#109.
        // Consensus would have answered `mandatory-script-verify-flag-failed`,
        // naming neither the identity nor the height.
        FlowError::Tx(TxError::FundsTimelocked { unlock_at }) => match unlock_at {
            Some(height) => format!(
                "{identity} opens at block {} — `pecu id show {identity}` counts it down, and \
                 nothing can bring it forward",
                fmt::height((*height).into())
            ),
            None => format!(
                "{identity} carries an unlock delay and nobody has started it, so its funds \
                 have no unlock height yet. `pecu id unlock {identity}` starts the countdown"
            ),
        },
        FlowError::Tx(TxError::AlreadyRevoked) => {
            "a revoked identity cannot spend. Its recovery authority can restore it first"
                .to_string()
        }
        FlowError::Tx(TxError::NotAPrimaryAddress { address }) => format!(
            "{address} is not one of {identity}'s primary addresses — `pecu id show {identity}` \
             lists them, and `pecu key list` shows what you hold"
        ),
        // Worth spelling out rather than leaving as a count: this command takes
        // one key, so an m-of-n identity cannot be spent from here at all yet.
        FlowError::Tx(TxError::NotEnoughSigners { supplied, required }) => format!(
            "{identity} needs {required} signatures and this supplied {supplied}. \
             `pecu send` signs with one key, so a multi-signature identity needs the air-gap \
             path — `pecu plan send` and `pecu sign` — once that learns identity inputs"
        ),
        _ => "run `pecu doctor`, or point somewhere else with --node".to_string(),
    };
    SendError::Flow {
        what: "building the identity-funded payment",
        advice,
        source: Box::new(source),
    }
}

fn build_token(
    ui: &Ui,
    node: &Node,
    key: &PrivateKey,
    to: &Recipient,
    amount: Amount,
    currency: CurrencyId,
) -> Result<Unsent<verus_sdk::network::Sent>, miette::Report> {
    let from = key.address().to_string();

    // The token builder takes the outputs to spend rather than finding them:
    // which of an address's reserve outputs to consume is a wallet's decision,
    // not the SDK's. They are picked here, by decoding what is already in hand.
    ui.sdk(format!("verus_sdk::network::spendable(&node, {from:?})"));
    let funding =
        spendable(node, &from).map_err(|source| flow("reading the address's outputs", source))?;
    ui.sdk_result(format!(
        "Funding {{ utxos: {}, other: {} }}",
        funding.utxos.len(),
        funding.other.len()
    ));

    let token_utxos = carrying(&funding.other, currency);
    if token_utxos.is_empty() {
        return Err(SendError::NoTokenOutputs {
            address: from,
            currency: currency_address(currency),
        }
        .into());
    }

    ui.sdk(format!(
        "verus_sdk::network::prepare_send_token(&node, &key, {}, {:?}, {}, &[{} token utxos])",
        currency_address(currency),
        to.address,
        amount.to_coins_string(),
        token_utxos.len()
    ));
    let unsent = prepare_send_token(node, key, currency, &to.address, amount, &token_utxos)
        .map_err(|source| insufficient_or_flow("building the token payment", source))?;
    ui.sdk_result(format!(
        "Unsent<Sent> {{ txid: {}, fee: {} }}",
        unsent.outcome.txid, unsent.outcome.fee
    ));
    Ok(unsent)
}

/// The reserve outputs holding `currency`.
pub(crate) fn carrying(
    others: &[verus_sdk::network::AddressUtxo],
    currency: CurrencyId,
) -> Vec<Utxo> {
    others
        .iter()
        .filter(|held| {
            matches!(
                decode_output_script(&held.utxo.script_pubkey),
                Ok(OutputKind::ReserveOutput { ref tokens, .. })
                    if tokens.iter().any(|(id, _)| *id == currency)
            )
        })
        .map(|held| held.utxo.clone())
        .collect()
}

/// What you are about to authorise, decoded from the bytes that would go out.
///
/// Deliberately built from the *finished transaction*, not from the arguments:
/// the point of a confirmation is to show what was actually constructed, and
/// re-printing the input would confirm nothing.
#[allow(clippy::too_many_arguments)]
fn review(
    ui: &Ui,
    settings: &Settings,
    from: &Envelope,
    identity: Option<&str>,
    to: &Recipient,
    amount: Amount,
    unsent: &Unsent<verus_sdk::network::Sent>,
    decoded: &Option<TxV4>,
) -> Panel {
    let palette = ui.theme.palette;
    let glyphs = ui.theme.glyphs;
    let currency = &settings.profile.currency;

    // Who the money leaves, which is not always the key that signs for it.
    // Naming the signing key here while an identity paid would misstate the
    // payer on the one panel whose job is to be checked before a spend.
    let mut panel = Panel::new("REVIEW");
    panel = match identity {
        None => panel.row(
            "from",
            Text::of(&from.address, palette.value)
                .space()
                .push(format!("({})", from.label), palette.muted),
        ),
        Some(identity) => panel
            .row(
                "from",
                Text::of(
                    fmt::untrusted(identity, IDENTITY_BUDGET, glyphs.ellipsis),
                    palette.accent,
                )
                .space()
                .push("(the identity's own funds)", palette.muted),
            )
            .row(
                "signed by",
                Text::of(&from.address, palette.value)
                    .space()
                    .push(format!("({})", from.label), palette.muted),
            ),
    };
    panel = panel
        .row("to", Text::of(&to.shown, palette.accent))
        .row(
            "amount",
            Text::of(fmt::amount(amount), palette.accent)
                .space()
                .push(currency, palette.muted),
        )
        .row(
            "fee",
            Text::of(fmt::amount(unsent.outcome.fee), palette.value)
                .space()
                .push(currency, palette.muted),
        )
        .row(
            "change",
            Text::of(fmt::amount(unsent.outcome.change), palette.value)
                .space()
                .push(currency, palette.muted)
                .push(
                    if unsent.outcome.change.is_zero() {
                        "  (none — it would have been dust)"
                    } else {
                        ""
                    },
                    palette.muted,
                ),
        )
        .row("txid", Text::of(&unsent.outcome.txid, palette.value));

    // Before the outputs, not after them: an expiry height is a fact about the
    // transaction, and tacking it onto the end of the output list read as if it
    // belonged to the last one.
    if let Some(transaction) = decoded {
        panel = panel.row("expiry", expiry(ui, transaction));
    }

    match decoded {
        Some(transaction) => {
            panel = panel.section("OUTPUTS AS BUILT");
            for (index, output) in transaction.outputs.iter().enumerate() {
                panel = panel
                    .line(
                        Text::of(format!("#{index}"), palette.muted)
                            .space()
                            .push(fmt::sats(output.value), palette.accent)
                            .space()
                            .push(currency, palette.muted),
                    )
                    .wrapped(5, describe(ui, &output.script_pubkey));
            }
        }
        // Should not happen — these are bytes the SDK just built — but silently
        // skipping the decoded view would turn the confirmation into a summary
        // of what was asked for rather than of what was made.
        None => {
            panel = panel.section("OUTPUTS AS BUILT").line(
                Text::of(glyphs.warn, palette.warn).space().push(
                    "the signed bytes did not decode — do not send this",
                    palette.danger,
                ),
            );
        }
    }

    panel
}

fn expiry(ui: &Ui, transaction: &TxV4) -> Text {
    let palette = ui.theme.palette;
    match transaction.expiry_height {
        0 => Text::of(ui.theme.glyphs.warn, palette.warn)
            .space()
            .push("never — this stays minable forever", palette.warn),
        height => Text::of(
            format!("height {}", fmt::height(height.into())),
            palette.value,
        ),
    }
}

fn describe(ui: &Ui, script: &[u8]) -> Text {
    let palette = ui.theme.palette;
    match decode_output_script(script) {
        Ok(OutputKind::PubKeyHash { hash }) => {
            Text::of(ui.theme.glyphs.arrow, palette.muted).space().push(
                Address::new(AddressKind::PubKeyHash, hash).to_string(),
                palette.value,
            )
        }
        Ok(OutputKind::IdentityPayment { identity }) => {
            Text::of(ui.theme.glyphs.arrow, palette.muted)
                .space()
                .push(
                    Address::new(AddressKind::Identity, identity).to_string(),
                    palette.value,
                )
                .push("  held for a VerusID, not a key", palette.muted)
        }
        Ok(OutputKind::ReserveOutput {
            destination,
            tokens,
        }) => Text::of(ui.theme.glyphs.arrow, palette.muted)
            .space()
            .push(format!("{destination:?}"), palette.value)
            .push(
                format!(
                    " holds {}",
                    tokens
                        .iter()
                        .map(|(id, amount)| format!(
                            "{} {}",
                            fmt::sats(*amount),
                            fmt::address(&currency_address(*id), ui.theme.glyphs.ellipsis)
                        ))
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
                palette.accent,
            ),
        Ok(other) => Text::of(format!("{other:?}"), palette.muted),
        Err(error) => Text::of(format!("undecodable: {error}"), palette.danger),
    }
}

/// Require the word, not a keystroke. This is the last thing between a signed
/// transaction and the network.
fn confirm(ui: &Ui) -> Result<(), miette::Report> {
    if !std::io::stdin().is_terminal() {
        return Err(SendError::CannotConfirm.into());
    }
    ui.blank();
    print!("  type `yes` to broadcast: ");
    let _ = std::io::stdout().flush();

    let mut answer = String::new();
    std::io::stdin()
        .read_line(&mut answer)
        .map_err(|_| SendError::CannotConfirm)?;
    if answer.trim() != "yes" {
        return Err(SendError::Cancelled.into());
    }
    Ok(())
}

fn currency_address(id: CurrencyId) -> String {
    Address::new(AddressKind::Identity, id.to_bytes()).to_string()
}

fn flow(what: &'static str, source: FlowError) -> SendError {
    use verus_sdk::verus_tx::TxError;
    // A VerusID recipient is the one case here that is not the node's fault, and
    // it is a likely thing to try: `--to name@` works for native coins, so the
    // same command with `--currency` reads as though it should. It does not, and
    // sending the reader to `pecu doctor` for a node that is answering perfectly
    // wastes their time on the wrong question.
    let advice = if matches!(&source, FlowError::Tx(TxError::UnsupportedRecipient)) {
        "a token payment can only name an R-address. Native coins can be sent to a \
         VerusID — `pecu send --to <name@>` without --currency — but the SDK writes a token \
         recipient as a plain key hash, so there is no way to address an identity's own token \
         holdings yet. Paying one of its primary addresses instead is not the same thing: those \
         coins belong to whoever holds that key, not to the identity"
            .to_string()
    } else {
        "run `pecu doctor`, or point somewhere else with --node".to_string()
    };
    SendError::Flow {
        what,
        advice,
        source: Box::new(source),
    }
}

/// Insufficient funds deserves its own message: the numbers are in the error,
/// and the likeliest confusion — value the address holds but this key cannot
/// move — is worth naming before someone goes looking for a bug.
fn insufficient_or_flow(what: &'static str, source: FlowError) -> SendError {
    if let FlowError::InsufficientFunds {
        needed,
        available,
        address,
        utxos,
    } = &source
    {
        return SendError::Insufficient {
            address: address.clone(),
            advice: format!(
                "{available} spendable across {utxos} output(s), {needed} needed including fee. \
                 `pecu wallet balance` shows the rest: value that is withheld, or held by a \
                 VerusID, cannot be moved by this key.",
            ),
        };
    }
    flow(what, source)
}

/// What happened to the signed transaction after it was built.
enum Delivery<'a> {
    /// Built and signed, deliberately not sent. A dry run.
    Held,
    /// The node took it.
    Accepted(&'a verus_sdk::network::Sent),
    /// The broadcast did not come back cleanly. Whether the node has it anyway
    /// is a separate question, answered in [`emit_json`].
    Failed(&'a FlowError),
}

/// The signed transaction, before anything is known about delivering it.
fn plan_json(
    unsent: &Unsent<verus_sdk::network::Sent>,
    decoded: &Option<TxV4>,
    from: &str,
    identity: Option<&str>,
) -> serde_json::Value {
    serde_json::json!({
        // Who the money leaves. For an identity-funded payment that is the
        // identity, not the key that signed for it — a consumer reconciling
        // balances against `from` would otherwise debit the wrong address.
        "from": from,
        "from_identity": identity,
        "txid": unsent.txid,
        "fee": unsent.outcome.fee.to_sat(),
        "change": unsent.outcome.change.to_sat(),
        "hex": unsent.hex,
        "outputs": decoded.as_ref().map(|tx| tx.outputs.len()),
    })
}

/// Print the one and only JSON document this command produces.
///
/// It used to print two — the plan before broadcasting and the result after —
/// so `pecu send --json | jq` failed on the very invocation that spent money,
/// and the first document announced `"broadcast": true` before anything had been
/// sent. One document, written once the answer is known, on every path
/// including the failing one.
///
/// `broadcast` is deliberately a tri-state:
///
/// * `true` — the node accepted it.
/// * `false` — it definitely was not accepted: a dry run, or a daemon that
///   answered with a rejection.
/// * `null` — unknown. The request did not complete, and a transaction whose
///   broadcast timed out may still be sitting in the mempool. Saying `false`
///   there would be a guess about money; `outcome` and `hex` are what let you
///   go and find out.
fn emit_json(plan: serde_json::Value, delivery: Delivery<'_>) {
    println!(
        "{}",
        serde_json::to_string_pretty(&delivery_json(plan, delivery)).expect("plain data")
    );
}

/// The document [`emit_json`] prints, so every state can be asserted on without
/// a node, a key or a transaction.
fn delivery_json(mut plan: serde_json::Value, delivery: Delivery<'_>) -> serde_json::Value {
    let document = plan.as_object_mut().expect("plan_json builds an object");

    let (broadcast, outcome) = match delivery {
        Delivery::Held => (Some(false), "not_broadcast"),
        Delivery::Accepted(sent) => {
            // The node's figures win over the builder's estimate.
            document.insert("txid".into(), serde_json::json!(sent.txid));
            document.insert("fee".into(), serde_json::json!(sent.fee.to_sat()));
            document.insert("change".into(), serde_json::json!(sent.change.to_sat()));
            (Some(true), "accepted")
        }
        Delivery::Failed(error) => {
            document.insert("error".into(), serde_json::json!(error.to_string()));
            // A daemon that answered with an error has read the transaction and
            // refused it. Anything else — a timeout, a dropped connection, a
            // reply this build could not parse — leaves the mempool's contents
            // genuinely unknown.
            match error {
                FlowError::Rpc(RpcError::Node { .. }) => (Some(false), "rejected"),
                _ => (None, "unknown"),
            }
        }
    };
    document.insert("broadcast".into(), serde_json::json!(broadcast));
    document.insert("outcome".into(), serde_json::json!(outcome));
    plan
}

#[cfg(test)]
mod tests {
    use unicode_width::UnicodeWidthStr;
    use verus_sdk::network::Sent;

    use super::*;
    use crate::cli::Theme as ThemeFlag;
    use crate::config::Paths;
    use crate::keystore::{Cipher, Kdf, ENVELOPE_VERSION};

    /// A real VRSCTEST transaction, so the review panel is exercised against
    /// bytes the daemon actually produced rather than something hand-rolled.
    fn fixture_transaction() -> TxV4 {
        let hex = include_str!("../../fixtures/identity-spend.hex");
        TxV4::deserialize(&hex::decode(hex.trim()).expect("valid fixture")).expect("a transaction")
    }

    fn envelope() -> Envelope {
        Envelope {
            version: ENVELOPE_VERSION,
            label: "paper".into(),
            address: "RJ7gsKDjUjPS8XZzENqmQMmWJRLuTnw5hp".into(),
            compressed: true,
            created: 0,
            kdf: Kdf {
                algorithm: "argon2id".into(),
                salt: String::new(),
                memory_kib: 1,
                iterations: 1,
                parallelism: 1,
            },
            cipher: Cipher {
                algorithm: "chacha20poly1305".into(),
                nonce: String::new(),
            },
            ciphertext: String::new(),
        }
    }

    fn rendered_for(identity: Option<&str>) -> String {
        let ui = Ui::new(ThemeFlag::Phosphor, false, false);
        let settings =
            Settings::resolve_in(Paths::at("/nonexistent"), None, None).expect("builtin");
        let transaction = fixture_transaction();
        let unsent = Unsent {
            hex: String::new(),
            txid: "2aada7…".into(),
            outcome: Sent {
                txid: "2aada7…".into(),
                fee: Amount::from_sat(10_000),
                change: Amount::from_sat(489_990_000),
                hex: String::new(),
            },
        };
        let recipient = Recipient {
            address: "RJ7gsKDjUjPS8XZzENqmQMmWJRLuTnw5hp".into(),
            shown: "RJ7gsKDjUjPS8XZzENqmQMmWJRLuTnw5hp".into(),
        };
        let panel = review(
            &ui,
            &settings,
            &envelope(),
            identity,
            &recipient,
            Amount::from_sat(10_000_000),
            &unsent,
            &Some(transaction),
        );
        panel.render(&ui.theme)
    }

    fn rendered() -> String {
        rendered_for(None)
    }

    #[test]
    fn expiry_is_a_transaction_fact_not_a_trailing_output() {
        let out = rendered();
        let expiry = out.find("expiry").expect("an expiry row");
        let outputs = out.find("OUTPUTS AS BUILT").expect("the outputs section");
        assert!(
            expiry < outputs,
            "expiry rendered after the output list, where it reads as belonging \
             to the last output:\n{out}"
        );
    }

    #[test]
    fn the_review_names_every_figure_that_matters() {
        let out = rendered();
        for wanted in [
            "from",
            "to",
            "amount",
            "fee",
            "change",
            "txid",
            "expiry",
            "0.10000000", // amount
            "0.00010000", // fee
            "4.89990000", // change
            "paper",      // which key pays
        ] {
            assert!(out.contains(wanted), "`{wanted}` missing from:\n{out}");
        }
    }

    #[test]
    fn the_review_frame_stays_rectangular() {
        let out = rendered();
        let widths: Vec<usize> = out
            .lines()
            .map(crate::ui::text::strip_ansi)
            .filter(|line| line.starts_with(['┌', '│', '├', '└']))
            .map(|line| UnicodeWidthStr::width(line.as_str()))
            .collect();
        assert!(!widths.is_empty(), "nothing was framed:\n{out}");
        assert!(
            widths.windows(2).all(|pair| pair[0] == pair[1]),
            "ragged frame {widths:?}:\n{out}"
        );
    }

    fn plan() -> serde_json::Value {
        serde_json::json!({
            "from": "RComfCn4wHHsGR8vWBAU7T1r3tHHyxN9Hm",
            "txid": "1111111111111111111111111111111111111111111111111111111111111111",
            "fee": 10_000,
            "change": 500,
            "hex": "0400008085202f89",
            "outputs": 2,
        })
    }

    #[test]
    fn a_dry_run_says_it_was_not_sent_rather_than_that_it_failed() {
        let document = delivery_json(plan(), Delivery::Held);
        assert_eq!(document["broadcast"], false);
        assert_eq!(document["outcome"], "not_broadcast");
        assert!(document["error"].is_null(), "nothing went wrong");
        assert_eq!(document["hex"], "0400008085202f89");
    }

    #[test]
    fn an_accepted_broadcast_carries_the_nodes_figures_not_the_builders() {
        let sent = Sent {
            txid: "2222222222222222222222222222222222222222222222222222222222222222".into(),
            fee: Amount::from_sat(12_345),
            change: Amount::from_sat(678),
            hex: "0400008085202f89".into(),
        };
        let document = delivery_json(plan(), Delivery::Accepted(&sent));
        assert_eq!(document["broadcast"], true);
        assert_eq!(document["outcome"], "accepted");
        assert_eq!(document["txid"], sent.txid);
        assert_eq!(document["fee"], 12_345);
        assert_eq!(document["change"], 678);
    }

    #[test]
    fn a_daemon_that_refused_it_is_a_knowable_no() {
        // The daemon read the transaction and said no, so it is not in any
        // mempool and the document can say so outright.
        let error = FlowError::Rpc(RpcError::Node {
            code: -26,
            message: "16: bad-txns-inputs-spent".into(),
        });
        let document = delivery_json(plan(), Delivery::Failed(&error));
        assert_eq!(document["broadcast"], false);
        assert_eq!(document["outcome"], "rejected");
        assert!(
            document["error"]
                .as_str()
                .unwrap_or_default()
                .contains("bad-txns-inputs-spent"),
            "{document:#}"
        );
        // The one field that cannot be reconstructed afterwards.
        assert_eq!(document["hex"], "0400008085202f89");
    }

    #[test]
    fn a_broadcast_that_did_not_come_back_is_unknown_not_false() {
        // A timed-out request may still have reached the mempool. `false` here
        // would tell someone their money is safe when it may already be moving.
        let error = FlowError::Rpc(RpcError::Transport("timed out".into()));
        let document = delivery_json(plan(), Delivery::Failed(&error));
        assert!(
            document["broadcast"].is_null(),
            "a timeout is not a no:\n{document:#}"
        );
        assert_eq!(document["outcome"], "unknown");
        assert_eq!(document["hex"], "0400008085202f89");
    }

    #[test]
    fn an_identity_funded_review_names_the_identity_as_the_payer() {
        let out = crate::ui::text::strip_ansi(&rendered_for(Some("pecucli7@")));
        // The money leaves the identity; the key only proves the authority.
        // Naming the key as `from` would misstate whose balance drops, on the
        // one panel whose entire job is to be read before a spend.
        assert!(out.contains("from"), "{out}");
        assert!(out.contains("pecucli7@"), "{out}");
        assert!(out.contains("the identity's own funds"), "{out}");
        assert!(out.contains("signed by"), "{out}");
    }

    #[test]
    fn an_ordinary_review_does_not_claim_an_identity_paid() {
        let out = crate::ui::text::strip_ansi(&rendered());
        assert!(!out.contains("the identity's own funds"), "{out}");
        assert!(!out.contains("signed by"), "{out}");
    }

    #[test]
    fn the_identity_funded_review_frame_stays_rectangular() {
        let out = crate::ui::text::strip_ansi(&rendered_for(Some("pecucli7@")));
        let widths: Vec<usize> = out
            .lines()
            .filter(|line| line.starts_with(['\u{250c}', '\u{2502}', '\u{251c}', '\u{2514}']))
            .map(UnicodeWidthStr::width)
            .collect();
        assert!(!widths.is_empty(), "nothing was framed:\n{out}");
        assert!(
            widths.windows(2).all(|pair| pair[0] == pair[1]),
            "ragged frame {widths:?}:\n{out}"
        );
    }
}
