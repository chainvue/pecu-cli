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
use crate::cmd::tx;
use crate::cmd::uncertain_broadcast_advice;
use crate::config::Settings;
use crate::keystore::{self, Envelope, Keystore};
use crate::node::{self, Node};
use crate::ui::{fmt, Panel, Text, Ui};

/// How much of an identity name is ever printed on the review panel.
const IDENTITY_BUDGET: usize = 40;

/// How much of the `to` row is ever printed. It is the one row carrying two
/// names — what was typed and what the node answered for it — so it gets both
/// budgets plus the ` (` and `)` that join them. Capping the pair at one name's
/// worth would elide the middle of an ordinary `bob@ (bob.VRSCTEST@)` once the
/// base name passed thirteen characters, on the panel that asks for `yes`.
const RECIPIENT_BUDGET: usize = IDENTITY_BUDGET * 2 + 3;

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

    #[error("`{name}` did not resolve to a currency on this chain")]
    #[diagnostic(
        code(pecu::unknown_currency),
        help("--currency wants the name with its trailing @, as in `{suggested}`, or the currency's i-address. `pecu wallet balance` prints the currencies an address holds in the form to copy")
    )]
    UnknownCurrency { name: String, suggested: String },

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
        Some(name) => Some(Token {
            id: resolve_currency(ui, &node, &profile.node, name)?,
            shown: name.clone(),
        }),
        None => None,
    };

    // Unlocked before building because signing needs it, and after every check
    // that can fail without it — a passphrase prompt for a send that was never
    // going to work is a waste of the one interaction this command demands.
    let secret = keystore::passphrase(&format!("passphrase for `{}`", envelope.label), false)?;
    let key = envelope.unlock(&secret)?;

    let unsent = match (&args.from_identity, &currency) {
        (Some(identity), _) => build_from_identity(ui, &node, &key, identity, &recipient, amount)?,
        (None, None) => build_native(ui, &node, &key, &recipient, amount)?,
        (None, Some(currency)) => build_token(ui, &node, &key, &recipient, amount, currency.id)?,
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
        currency.as_ref(),
    );

    if !ui.is_json() {
        ui.panel(&review(
            ui,
            settings,
            &envelope,
            args.from_identity.as_deref(),
            &recipient,
            amount,
            currency.as_ref(),
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
    ///
    /// Untrusted on both halves — pasted argument and node answer — so `review`
    /// runs it through `fmt::untrusted` before it goes inside a frame.
    shown: String,
}

/// Which token is moving, when one is. A native send has nothing to name beyond
/// the profile's own ticker, so this is `None` there.
///
/// Name and id travel together on purpose: the name is untrusted display text a
/// registrant chose, and the id is the part that identifies anything. Two
/// parallel `Option`s threaded through two signatures could silently disagree.
struct Token {
    /// What the SDK is handed — always a currency id.
    id: CurrencyId,
    /// What to show. The name as typed, so the confirmation answers the command
    /// that was given; the `currency id` row is what the node resolved it to.
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

/// Which currency `--currency` names, as the id the SDK moves.
///
/// A currency id *is* the defining identity's 160-bit hash, so an i-address is
/// the answer already and a name has to be looked up as an identity to become
/// one. What that lookup refuses is a *currency* problem: reporting it as
/// `unknown_recipient` sent readers off to check a `--to` that was never
/// involved.
fn resolve_currency(
    ui: &Ui,
    node: &Node,
    url: &str,
    name: &str,
) -> Result<CurrencyId, miette::Report> {
    if let Ok(address) = name.parse::<Address>() {
        if address.kind() == AddressKind::Identity {
            return Ok(CurrencyId::from_bytes(address.hash()));
        }
    }

    // The `@` form of what was typed is the remedy the help offers, and a name
    // that already carries one gets it back unchanged rather than doubled.
    let unknown = || SendError::UnknownCurrency {
        name: name.to_string(),
        suggested: format!("{}@", name.trim_end_matches('@')),
    };

    ui.sdk(format!("node.identity({name:?})"));
    let record = match node.identity(name) {
        Ok(record) => record,
        // `-5` is no such name and `-8` is not a usable reference at all —
        // which is exactly what a currency name missing its `@` gets back.
        // Both are the daemon answering. Anything else is it failing to, and
        // calling that "did not resolve to a currency" would deny the
        // existence of a currency nobody asked about. The same distinction
        // `wallet balance` and `id show` already draw.
        Err(RpcError::Node { code: -5 | -8, .. }) => return Err(unknown().into()),
        Err(other) => {
            return Err(node::NodeError::request("looking up the currency", url, other).into())
        }
    };
    ui.sdk_result(format!("identity_address: {}", record.identity_address));
    let address: Address = record.identity_address.parse().map_err(|_| unknown())?;
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
    token: Option<&Token>,
    unsent: &Unsent<verus_sdk::network::Sent>,
    decoded: &Option<TxV4>,
) -> Panel {
    let palette = ui.theme.palette;
    let glyphs = ui.theme.glyphs;
    let currency = &settings.profile.currency;

    // The label on the one row anybody actually checks. `--currency` moves a
    // token while the miner is still paid in the chain's own coins, so this is
    // the only row that follows what is being sent: labelling all four with the
    // profile's ticker asserted a native transfer on every token send, on a
    // transaction that moves no native value at all.
    let moving = match token {
        // A currency name is an identity name, so it gets the same treatment
        // `--from-identity` gets below: it is text somebody registered, going
        // inside a box frame.
        Some(token) => fmt::untrusted(&token.shown, IDENTITY_BUDGET, glyphs.ellipsis),
        None => currency.to_string(),
    };

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
    // The `to` row is untrusted on both halves: `--to` is text somebody pasted,
    // and the name in parentheses is the node repeating what a registrant chose.
    // Same treatment as the two rows above — an escape run here can repaint the
    // terminal or forge a row, and this row sits directly above OUTPUTS AS BUILT,
    // the only place a substituted address would be exposed.
    panel = panel
        .row(
            "to",
            Text::of(
                fmt::untrusted(&to.shown, RECIPIENT_BUDGET, glyphs.ellipsis),
                palette.accent,
            ),
        )
        .row(
            "amount",
            Text::of(fmt::amount(amount), palette.accent)
                .space()
                .push(&moving, palette.muted),
        );

    // What the name resolved to. `--currency mytoken@` is a name lookup, and a
    // panel that only repeats the name back proves nothing about which currency
    // the node actually named — while `describe()` prints the reserve output's
    // currency truncated, so there was nothing full to check it against. The
    // same pair of rows the mint panel carries, for the same reason.
    if let Some(token) = token {
        panel = panel.row(
            "currency id",
            Text::of(currency_address(token.id), palette.value),
        );
    }

    panel = panel
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
            .push(tx::show(&destination), palette.value)
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
    // The node answered, or the connection broke — either way `pecu doctor`
    // blames a node that is not the problem, and the retry it invites is how
    // the payment gets made twice.
    } else if let FlowError::BroadcastUncertain { txid, hex, .. } = &source {
        uncertain_broadcast_advice(txid, hex)
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
    token: Option<&Token>,
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
        // Which asset moved. Null when native coins are moving, the way
        // `from_identity` is null on an ordinary spend — a ticker read out of
        // this field could otherwise be either. `fee` and `change` above are
        // native satoshis on every path including this one, because a token
        // moves as a reserve output while the miner is paid in the chain's own
        // currency, so they are not denominated in `currency`.
        "currency": token.map(|token| token.shown.as_str()),
        "currency_id": token.map(|token| currency_address(token.id)),
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
///   answered with an outright rejection.
/// * `null` — unknown. Either the request did not complete, or the node
///   answered without settling the outcome: a `-25` says a check failed, not
///   that the transaction was refused, so the SDK reports it as
///   `BroadcastUncertain` and it lands here rather than under `rejected`. A
///   broadcast that timed out may still be sitting in the mempool just the
///   same. Saying `false` on either would be a guess about money; `outcome`
///   and `hex` are what let you go and find out.
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
            // A daemon that answered with an outright rejection has read the
            // transaction and refused it. Anything else — a timeout, a dropped
            // connection, a reply this build could not parse, or a `-25` the
            // SDK hands back as `BroadcastUncertain` because it does not say
            // the transaction was refused — leaves the mempool's contents
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
    use verus_sdk::decode::Destination;
    use verus_sdk::network::Sent;
    use verus_sdk::verus_tx::cc::reserve_output_script_to;
    use verus_sdk::verus_wire::TxOut;

    use super::*;
    use crate::cli::Theme as ThemeFlag;
    use crate::config::Paths;
    use crate::keystore::{Cipher, Kdf, ENVELOPE_VERSION};
    use crate::ui::theme::{Skin, Theme};

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

    fn unsent() -> Unsent<Sent> {
        Unsent {
            hex: String::new(),
            txid: "2aada7…".into(),
            outcome: Sent {
                txid: "2aada7…".into(),
                fee: Amount::from_sat(10_000),
                change: Amount::from_sat(489_990_000),
                hex: String::new(),
            },
        }
    }

    /// The i-address behind the token the panel tests move. A currency id is
    /// an identity's 160-bit hash, which is what `--currency` resolves to.
    fn token_id() -> CurrencyId {
        CurrencyId::from_bytes(
            "iK2k8YH1jfR7RLmEZ3zac2Mkx5rxSgbMqg"
                .parse::<Address>()
                .expect("an i-address")
                .hash(),
        )
    }

    fn rendered_for(identity: Option<&str>) -> String {
        rendered_with(identity, None)
    }

    /// The address the panel tests pay when the recipient is not what is being
    /// tested.
    const RECIPIENT: &str = "RJ7gsKDjUjPS8XZzENqmQMmWJRLuTnw5hp";

    fn rendered_with(identity: Option<&str>, token: Option<&Token>) -> String {
        rendered_paying(identity, token, RECIPIENT)
    }

    fn rendered_paying(identity: Option<&str>, token: Option<&Token>, shown: &str) -> String {
        let ui = Ui::new(ThemeFlag::Phosphor, false, false);
        let settings =
            Settings::resolve_in(Paths::at("/nonexistent"), None, None).expect("builtin");
        let transaction = fixture_transaction();
        let recipient = Recipient {
            address: RECIPIENT.into(),
            shown: shown.into(),
        };
        let panel = review(
            &ui,
            &settings,
            &envelope(),
            identity,
            &recipient,
            Amount::from_sat(10_000_000),
            token,
            &unsent(),
            &Some(transaction),
        );
        panel.render(&ui.theme)
    }

    /// The panel as a token send renders it, with the escapes stripped so the
    /// assertions can look at one row at a time.
    fn rendered_token(shown: &str) -> String {
        crate::ui::text::strip_ansi(&rendered_with(
            None,
            Some(&Token {
                id: token_id(),
                shown: shown.into(),
            }),
        ))
    }

    /// The panel over the outputs a token send really builds. The checked-in
    /// fixture is a native identity spend and carries no reserve output, which
    /// is why every assertion here passed while the recipient row printed
    /// twenty numbers — so this replaces the outputs with a script from the
    /// SDK's own encoder, the one `build_token_send` writes.
    fn rendered_holding(destination: Destination) -> String {
        let mut ui = Ui::new(ThemeFlag::Phosphor, false, false);
        // Pin the width, as the integration tests pin `PECU_WIDTH`, so the
        // assertions do not depend on whoever's terminal ran them: the output
        // row wraps between the address and the amount below about sixty
        // columns, and the panel is drawn as narrow as forty-eight.
        ui.theme = Theme::with_skin(Skin::Phosphor, 84);
        let settings =
            Settings::resolve_in(Paths::at("/nonexistent"), None, None).expect("builtin");
        let mut transaction = fixture_transaction();
        transaction.outputs = vec![TxOut {
            // Zero, as the builder writes it: the value is the token in the
            // payload, not the satoshis on the output.
            value: 0,
            script_pubkey: reserve_output_script_to(destination, token_id(), 500_000_000)
                .expect("the SDK encodes its own reserve output"),
        }];
        let token = Token {
            id: token_id(),
            shown: "mytoken@".into(),
        };
        let recipient = Recipient {
            address: RECIPIENT.into(),
            shown: RECIPIENT.into(),
        };
        let panel = review(
            &ui,
            &settings,
            &envelope(),
            None,
            &recipient,
            Amount::from_sat(500_000_000),
            Some(&token),
            &unsent(),
            &Some(transaction),
        );
        crate::ui::text::strip_ansi(&panel.render(&ui.theme))
    }

    /// The single rendered line carrying `label`. Whole-panel assertions cannot
    /// say anything useful here: a token send legitimately names the chain's own
    /// currency on other rows, and the point is which row says which.
    fn row<'a>(rendered: &'a str, label: &str) -> &'a str {
        let mut matching = rendered.lines().filter(|line| line.contains(label));
        let found = matching
            .next()
            .unwrap_or_else(|| panic!("no `{label}` row in:\n{rendered}"));
        assert!(
            matching.next().is_none(),
            "`{label}` matched more than one line in:\n{rendered}"
        );
        found
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
    fn a_token_send_labels_the_amount_with_the_token_it_moves() {
        // The whole issue. `--currency mytoken@` moves no native coins at all,
        // so labelling the headline row with the chain's ticker was an
        // affirmative falsehood on the one row that gets read before `yes`.
        let out = rendered_token("mytoken@");
        let amount = row(&out, "amount");
        assert!(amount.contains("mytoken@"), "{out}");
        assert!(
            !amount.contains("VRSCTEST"),
            "the amount row still claims native coins:\n{out}"
        );
    }

    #[test]
    fn a_native_send_still_labels_the_amount_with_the_chains_own_currency() {
        // The other half of the same row: nothing about a token send may leak
        // into the ordinary path, which really is denominated in the chain's
        // own coins.
        let out = crate::ui::text::strip_ansi(&rendered());
        assert!(row(&out, "amount").contains("VRSCTEST"), "{out}");
    }

    #[test]
    fn the_fee_and_the_change_stay_native_on_a_token_send() {
        // Deliberately *not* relabelled. A token moves as a reserve output
        // while the miner is paid in the chain's own currency, so both of these
        // figures are native satoshis; naming them after the token would
        // replace one false statement with two.
        let out = rendered_token("mytoken@");
        for label in ["fee", "change"] {
            let line = row(&out, label);
            assert!(line.contains("VRSCTEST"), "`{label}`:\n{out}");
            assert!(!line.contains("mytoken@"), "`{label}`:\n{out}");
        }
    }

    #[test]
    fn the_outputs_as_built_figures_stay_native_on_a_token_send() {
        // Also deliberately not relabelled. These are the wire `TxOut.value`,
        // and a reserve output's `0.00000000` is the truth about that output —
        // the token it carries is on the line beneath it. Stamping the token's
        // name here would read as "zero of your token reaches the recipient".
        let out = rendered_token("mytoken@");
        let outputs: Vec<&str> = out
            .lines()
            .filter(|line| line.contains("#0") || line.contains("#1"))
            .collect();
        assert!(!outputs.is_empty(), "no outputs were listed:\n{out}");
        for line in outputs {
            assert!(line.contains("VRSCTEST"), "{out}");
        }
    }

    #[test]
    fn a_token_send_names_the_currency_id_it_resolved_to() {
        // A name is a lookup, and repeating the name back proves nothing about
        // which currency the node named. The reserve output on the OUTPUTS AS
        // BUILT line shows its currency truncated, so this row is what it can
        // be checked against.
        let out = rendered_token("mytoken@");
        assert!(
            row(&out, "currency id").contains("iK2k8YH1jfR7RLmEZ3zac2Mkx5rxSgbMqg"),
            "{out}"
        );
    }

    #[test]
    fn a_token_output_names_the_address_it_pays_rather_than_its_bytes() {
        // The whole issue. The reserve arm formatted its destination with
        // `{:?}`, so the one row that says where the token goes read
        // `PubKeyHash([38, 176, …])` — twenty decimal numbers nobody can check
        // against the address they typed, on the panel that asks for `yes`.
        //
        // The address paid here is deliberately neither `RECIPIENT` nor the
        // envelope's, both of which already appear elsewhere on this panel: a
        // third address is the only way the assertion can prove it read *this*
        // row.
        let paid = "RComfCn4wHHsGR8vWBAU7T1r3tHHyxN9Hm";
        let hash = paid.parse::<Address>().expect("an R-address").hash();
        let out = rendered_holding(Destination::PubKeyHash(hash));
        assert!(out.contains(paid), "{out}");
        assert!(!out.contains("PubKeyHash("), "{out}");
        // The token half of the same line must survive the fix — a rendering
        // that named the address and dropped the amount would be no better.
        assert!(out.contains("holds 5.00000000"), "{out}");
    }

    #[test]
    fn a_token_output_held_for_an_identity_reads_as_an_i_address() {
        // The display twin of the upstream bug in `docs/status.md` (#115): an
        // identity destination written as a key hash pays a different owner
        // entirely, because the two share their 160 bits and differ only in the
        // kind. A renderer that guesses `PubKeyHash` shows the reader the wrong
        // address for the right output, so the `i` and the `R` are asserted
        // apart rather than together.
        //
        // Not reachable from the CLI today — `build_token_send` refuses a
        // non-key-hash recipient and `--from-identity` conflicts with
        // `--currency` — so this pins the rendering ahead of the path.
        let held = "i7r29bDQfrwjkTxjv4bcYD6B1ZV7WZ4kGo";
        let hash = held.parse::<Address>().expect("an i-address").hash();
        let twin = Address::new(AddressKind::PubKeyHash, hash).to_string();
        let out = rendered_holding(Destination::Identity(hash));
        assert!(out.contains(held), "{out}");
        assert!(!out.contains("Identity("), "{out}");
        assert!(!out.contains(&twin), "the key-hash twin was shown:\n{out}");
    }

    #[test]
    fn an_ordinary_send_names_no_currency_id_at_all() {
        let out = crate::ui::text::strip_ansi(&rendered());
        assert!(!out.contains("currency id"), "{out}");
    }

    #[test]
    fn a_hostile_currency_name_cannot_break_the_review_frame() {
        // A currency name is untrusted display text somebody registered, and it
        // now reaches the inside of a box frame. An escape that repaints the
        // terminal or a newline that forges an extra row would do it here.
        let hostile = format!("\u{1b}[31mev\nil\u{7f}{}@", "x".repeat(80));
        let out = rendered_token(&hostile);
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

    #[test]
    fn a_bidi_override_in_a_currency_name_cannot_reverse_the_review_row() {
        // None of these is `is_control()`, so the frame check above says
        // nothing about them: RLO reverses the display order of everything
        // after it and ZWSP has no glyph at all, which on a panel that asks for
        // `yes` is a name reading as one currency while being another. This is
        // the end-to-end proof that the row really routes through the
        // sanitiser rather than printing what the node answered.
        let hostile = "ev\u{202e}il\u{200b}coin@";
        let out = rendered_token(hostile);
        assert!(
            !out.contains('\u{202e}'),
            "the bidi override survived into the panel:\n{out}"
        );
        assert!(
            !out.contains('\u{200b}'),
            "the zero-width space survived into the panel:\n{out}"
        );
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

    #[test]
    fn a_hostile_recipient_name_cannot_break_the_review_frame() {
        // The `to` row carries what the node answered for `--to bob@`, and a
        // node hostile enough to answer with escapes is the whole reason this
        // panel gets checked before `yes`. Same class as the currency name
        // above, on the row directly over OUTPUTS AS BUILT.
        let hostile = format!("bob@ (\u{1b}[31mev\nil\u{7f}{}@)", "x".repeat(80));
        let raw = rendered_paying(None, None, &hostile);
        assert!(
            !raw.contains('\u{7f}'),
            "the delete character survived into the panel:\n{raw}"
        );
        let out = crate::ui::text::strip_ansi(&raw);
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

    #[test]
    fn the_to_row_names_both_what_was_typed_and_what_the_node_answered() {
        // The pair is budgeted as a pair. This name is forty-six characters
        // across both halves and nothing about it is exotic, so capping the row
        // at one name's worth of budget would put an ellipsis through the middle
        // of it — a `to` row with a hole in it, on the panel that asks for `yes`,
        // is worse than the escape run the cap exists to stop.
        let shown = "myawesomeproject@ (myawesomeproject.VRSCTEST@)";
        let out = crate::ui::text::strip_ansi(&rendered_paying(None, None, shown));
        assert!(out.contains(shown), "{out}");
    }

    #[test]
    fn the_plan_names_the_currency_being_sent() {
        // `--json` is the other consent surface, and a token send looked native
        // there too.
        let document = plan_json(
            &unsent(),
            &None,
            "RJ7gsKDjUjPS8XZzENqmQMmWJRLuTnw5hp",
            None,
            Some(&Token {
                id: token_id(),
                shown: "mytoken@".into(),
            }),
        );
        assert_eq!(document["currency"], "mytoken@");
        assert_eq!(
            document["currency_id"],
            "iK2k8YH1jfR7RLmEZ3zac2Mkx5rxSgbMqg"
        );
    }

    #[test]
    fn the_plan_leaves_the_currency_null_when_native_coins_move() {
        // Null rather than the chain's ticker, so a consumer can tell a token
        // send from a native one instead of reading a name that might be either.
        let document = plan_json(
            &unsent(),
            &None,
            "RJ7gsKDjUjPS8XZzENqmQMmWJRLuTnw5hp",
            None,
            None,
        );
        assert!(document["currency"].is_null(), "{document:#}");
        assert!(document["currency_id"].is_null(), "{document:#}");
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

    #[test]
    fn an_uncertain_send_broadcast_does_not_send_the_reader_to_the_doctor() {
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
        let SendError::Flow { advice, .. } = refused else {
            panic!("a flow refusal is a SendError::Flow");
        };
        assert!(advice.contains("tx explain 9c1d55"));
        assert!(!advice.contains("doctor"));
    }

    /// Arm ordering: the token-recipient refusal comes first and has to stay
    /// there, since being swallowed by the new branch is the only way this
    /// change could regress it.
    #[test]
    fn a_token_recipient_keeps_its_own_advice_ahead_of_the_uncertain_arm() {
        use verus_sdk::verus_tx::TxError;
        let refused = flow("building", FlowError::Tx(TxError::UnsupportedRecipient));
        let SendError::Flow { advice, .. } = refused else {
            panic!("a flow refusal is a SendError::Flow");
        };
        assert!(advice.contains("a token payment can only name an R-address"));
    }
}
