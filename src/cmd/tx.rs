//! `pecu tx explain` — what is actually in this transaction?
//!
//! On Bitcoin an output is a script and a number of satoshis, and the number is
//! the value. On Verus that is true only for the plain ones. A token lives in
//! the *payload* of a CryptoCondition output whose satoshi field is zero; an
//! identity is an output; a conversion in flight is an output; a name
//! commitment is an output. Reading the satoshi column of a Verus transaction
//! and calling it the value is how a wallet reports that an address holds
//! nothing while it holds a fortune in tokens.
//!
//! So this says what each output *is*, and where it cannot tell, it says so —
//! including whether the thing it cannot read is *able* to hold money. That
//! last distinction is the one worth having: an undecodable output that
//! provably cannot carry currency is safe to ignore, and one that can is not.
//!
//! Offline for hex. A txid is the one input that needs a node, and only to
//! fetch the bytes; the decoding is the same either way. A node in hand is also
//! a node that can say what a currency is *called*, so the txid path names them
//! and the offline paths print the ids alone — the id is what identifies a
//! currency in either case, and it is never traded away for a name.

use std::collections::{BTreeMap, BTreeSet};
use std::io::Read;
use std::time::Duration;

use miette::Diagnostic;
use thiserror::Error;
use verus_sdk::decode::{decode_output_script, Destination, OutputKind};
use verus_sdk::money::Amount;
use verus_sdk::network::ChainReader;
use verus_sdk::send::CurrencyId;
use verus_sdk::verus_keys::{Address, AddressKind};
use verus_sdk::verus_wire::TxV4;

use crate::config::Settings;
use crate::currency_name::{look_up_names, name_budget, name_json, name_result, CurrencyName};
use crate::node::{self, Node, NodeError};
use crate::ui::{fmt, Panel, Text, Ui};

/// A node-supplied name is display text and nothing more; this is how much of
/// one is ever printed.
const NAME_BUDGET: usize = 32;

/// What was learned about the currencies in these bytes.
///
/// `None` is not "no names came back" — it is that nothing was asked, which is
/// the state of every path here that has no node, and a different thing from a
/// lookup that failed. They are kept apart because a panel that prints
/// `(name unknown)` beside an id nobody went looking up reports a failure that
/// never happened.
///
/// A currency missing from the map inside says the same thing about itself: the
/// naming step is bounded as a whole, so a run against a slow node can come back
/// having asked about some of the set and not the rest.
type Names<'a> = Option<&'a BTreeMap<CurrencyId, CurrencyName>>;

#[derive(Debug, Error, Diagnostic)]
pub enum TxError {
    #[error("nothing to decode")]
    #[diagnostic(
        code(pecu::no_input),
        help("give a txid, a raw transaction as hex, or `-` to read hex on stdin")
    )]
    Empty,

    #[error("that is not hex")]
    #[diagnostic(code(pecu::not_hex), help("{detail}"))]
    NotHex { detail: String },

    #[error("cannot read stdin")]
    #[diagnostic(code(pecu::stdin))]
    Stdin {
        #[source]
        source: std::io::Error,
    },

    #[error("the node knows no transaction {txid}")]
    #[diagnostic(
        code(pecu::unknown_txid),
        help("check the id, or pass the raw hex instead — decoding needs no node")
    )]
    UnknownTxid { txid: String },

    #[error("the node answered without the transaction's bytes")]
    #[diagnostic(
        code(pecu::no_raw_hex),
        help("`getrawtransaction` did not include a `hex` field; pass the raw hex instead")
    )]
    NoRawHex,

    #[error("these bytes are not a transaction, and not an output script either")]
    #[diagnostic(code(pecu::undecodable), help("{detail}"))]
    Undecodable { detail: String },
}

pub fn explain(ui: &Ui, settings: &Settings, input: Option<&str>) -> miette::Result<()> {
    let outcome = attempt(ui, settings, input);
    // Printed on the way out whatever happened, the way `pecu send` does it.
    // `--explain` earns its keep most when something went wrong — a node that
    // refused the txid, a name lookup that ran out of time — and printing the
    // record only on the two success arms hid the call that failed in exactly
    // the case a reader came for it.
    ui.explain_panel();
    outcome
}

fn attempt(ui: &Ui, settings: &Settings, input: Option<&str>) -> miette::Result<()> {
    let source = read_input(input)?;
    // The node is built on the txid arm and nowhere else, and what the rest of
    // this function gets is an `Option<&Node>`. `pecu tx explain <hex>` is
    // documented as needing no network — it is what still answers after a
    // broadcast the node was unsure about, when the node is the thing that just
    // failed — so the promise is kept by there being nothing to ask rather than
    // by a rule somewhere further down remembering not to.
    let (bytes, node) = match &source {
        Source::Txid(txid) => {
            let node = node::connect(&settings.profile)?;
            let bytes = fetch(ui, &node, &settings.profile.node, txid)?;
            (bytes, Some(node))
        }
        Source::Hex(hex) => (
            hex::decode(hex).map_err(|error| TxError::NotHex {
                detail: error.to_string(),
            })?,
            None,
        ),
    };

    // A transaction first, because that is what a caller almost always has. A
    // bare script is the useful fallback: "what does this scriptPubKey do" comes
    // up while debugging a builder, and at that point there is no transaction to
    // put it in yet.
    match TxV4::deserialize(&bytes) {
        Ok(transaction) => {
            let names = name_currencies(
                ui,
                node.as_ref(),
                &transaction_currencies(&transaction),
                name_budget(&settings.profile),
            );
            if ui.is_json() {
                emit_transaction_json(&transaction, names.as_ref());
            } else {
                render_transaction(ui, &transaction, names.as_ref());
            }
            Ok(())
        }
        Err(transaction_error) => match decode_output_script(&bytes) {
            Ok(kind) => {
                let mut wanted = BTreeSet::new();
                currencies_in(&kind, &mut wanted);
                let names =
                    name_currencies(ui, node.as_ref(), &wanted, name_budget(&settings.profile));
                if ui.is_json() {
                    emit_json(&serde_json::json!({
                        "kind": "output_script",
                        "output": output_json(&kind, None, names.as_ref()),
                    }));
                } else {
                    render_script(ui, &kind, names.as_ref());
                }
                Ok(())
            }
            // Report the transaction error, not the script one: the input was
            // far more likely meant to be a transaction, and "expected 4 bytes
            // of version" is a more useful sentence than "not a script".
            Err(_) => Err(TxError::Undecodable {
                detail: transaction_error.to_string(),
            }
            .into()),
        },
    }
}

/// Every currency any output in this transaction names.
///
/// Decoded again rather than threaded through from the render: decoding a
/// script is pure and cheap, and one pass that collects before anything is
/// printed is what lets the whole lookup happen ahead of the single `--json`
/// document.
fn transaction_currencies(transaction: &TxV4) -> BTreeSet<CurrencyId> {
    let mut found = BTreeSet::new();
    for output in &transaction.outputs {
        if let Ok(kind) = decode_output_script(&output.script_pubkey) {
            currencies_in(&kind, &mut found);
        }
    }
    found
}

/// The currencies one output names, which is exactly the set [`describe`] and
/// [`output_json`] will print ids for.
fn currencies_in(kind: &OutputKind, found: &mut BTreeSet<CurrencyId>) {
    match kind {
        OutputKind::ReserveOutput { tokens, .. }
        | OutputKind::IdentityCommitment { tokens, .. } => {
            found.extend(tokens.iter().map(|(id, _)| *id));
        }
        OutputKind::ReserveDeposit {
            controlling_currency,
            tokens,
            ..
        } => {
            found.insert(*controlling_currency);
            found.extend(tokens.iter().map(|(id, _)| *id));
        }
        OutputKind::ReserveTransfer { transfer, .. } => {
            found.insert(transfer.destination_currency);
            found.insert(transfer.fee_currency);
        }
        _ => {}
    }
}

/// Ask the node what these currencies are called, if there is a node to ask.
///
/// Infallible on purpose, twice over. `None` for want of a node is not a
/// failure — it is the offline path working as documented. And `look_up_names`
/// returns a verdict per currency rather than one `Result` for the set, so a
/// node that times out mid-way costs this transaction some names and not the
/// answer the caller came for.
///
/// The `budget` covers the whole step, which matters more here than anywhere
/// else that asks: `wanted` is everything the *counterparty's* bytes named, so
/// nothing about the input bounds how many lookups this would otherwise be.
fn name_currencies(
    ui: &Ui,
    node: Option<&Node>,
    wanted: &BTreeSet<CurrencyId>,
    budget: Duration,
) -> Option<BTreeMap<CurrencyId, CurrencyName>> {
    let node = node?;
    if wanted.is_empty() {
        return Some(BTreeMap::new());
    }
    ui.sdk(format!(
        "node.currency_definition(…) for {}",
        fmt::plural(wanted.len(), "currency", "currencies")
    ));
    let named = look_up_names(node, wanted, budget);
    ui.sdk_result(name_result(&named, wanted));
    Some(named)
}

enum Source {
    Txid(String),
    Hex(String),
}

/// Work out what we were given. A 64-character hex string is a txid — nothing
/// else is that length — and anything else hex is raw bytes.
fn read_input(input: Option<&str>) -> Result<Source, TxError> {
    let raw = match input {
        None | Some("-") => {
            let mut buffer = String::new();
            std::io::stdin()
                .read_to_string(&mut buffer)
                .map_err(|source| TxError::Stdin { source })?;
            buffer
        }
        Some(argument) => argument.to_string(),
    };
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(TxError::Empty);
    }
    if trimmed.len() == 64 && trimmed.chars().all(|c| c.is_ascii_hexdigit()) {
        return Ok(Source::Txid(trimmed.to_ascii_lowercase()));
    }
    Ok(Source::Hex(trimmed.to_string()))
}

/// The bytes behind a txid.
///
/// Takes the node rather than building one, so the caller keeps it: the same
/// connection then answers what the currencies in those bytes are called,
/// which is the only reason this path can name anything at all.
fn fetch(ui: &Ui, node: &Node, url: &str, txid: &str) -> Result<Vec<u8>, miette::Report> {
    ui.sdk(format!("node.raw_transaction({txid})"));
    let transaction = node.raw_transaction(txid).map_err(|source| {
        match &source {
            // The node has never seen it. That is an answer, not a failure.
            verus_sdk::network::RpcError::Node { code: -5, .. } => TxError::UnknownTxid {
                txid: txid.to_string(),
            }
            .into(),
            _ => miette::Report::from(NodeError::request("fetching the transaction", url, source)),
        }
    })?;
    let hex = transaction
        .get("hex")
        .and_then(serde_json::Value::as_str)
        .ok_or(TxError::NoRawHex)?;
    // Decoded here rather than trusting the node's own decode: the whole point
    // of the command is what this SDK makes of the bytes.
    Ok(hex::decode(hex).map_err(|error| TxError::NotHex {
        detail: error.to_string(),
    })?)
}

fn render_transaction(ui: &Ui, transaction: &TxV4, names: Names) {
    let palette = ui.theme.palette;
    let glyphs = ui.theme.glyphs;
    let total_out = total_output_value(transaction);

    let expiry = match transaction.expiry_height {
        // Worth naming rather than printing as 0. An expiry of "never" is a
        // payment that can still be mined months later, against coins the sender
        // has since spent elsewhere.
        0 => Text::of(glyphs.warn, palette.warn).space().push(
            "never — this transaction stays minable forever",
            palette.warn,
        ),
        height => Text::of(
            format!("height {}", fmt::height(height.into())),
            palette.value,
        ),
    };

    let mut header = Panel::new("TRANSACTION")
        .row(
            "txid",
            Text::of(
                transaction
                    .txid()
                    .map_or_else(|_| "unknown".to_string(), hex_reversed),
                palette.accent,
            ),
        )
        .row("expiry", expiry);

    if transaction.is_shielded() {
        header = header.row(
            "shielded",
            Text::of(
                format!(
                    "{}, {}, valueBalance {}",
                    fmt::plural(transaction.shielded_spends.len(), "spend", "spends"),
                    fmt::plural(transaction.shielded_outputs.len(), "output", "outputs"),
                    fmt::sats(transaction.value_balance.unsigned_abs()),
                ),
                palette.value,
            ),
        );
    }

    header = header.section("INPUTS");
    if transaction.inputs.is_empty() {
        header = header.line(Text::of("none", palette.muted));
    }
    for (index, input) in transaction.inputs.iter().enumerate() {
        let mut txid = input.txid_internal;
        txid.reverse();
        let mut line = Text::of(format!("#{index}"), palette.muted)
            .space()
            .push(
                fmt::hash(&hex::encode(txid), glyphs.ellipsis),
                palette.value,
            )
            .push(format!(":{}", input.vout), palette.muted);
        if input.script_sig.is_empty() {
            line = line.push("  (unsigned)", palette.warn);
        }
        header = header.line(line);
    }

    header = header.section("OUTPUTS").line(Text::of(
        format!(
            "{} — {} in native satoshis",
            fmt::plural(transaction.outputs.len(), "output", "outputs"),
            fmt::total(total_out)
        ),
        palette.muted,
    ));

    for (index, output) in transaction.outputs.iter().enumerate() {
        header = header.line(
            Text::of(format!("#{index}"), palette.muted)
                .space()
                .push(fmt::sats(output.value), value_style(ui, output.value)),
        );
        header = header.wrapped(
            OUTPUT_INDENT,
            describe_script(ui, &output.script_pubkey, names),
        );
    }

    ui.panel(&header);
}

fn render_script(ui: &Ui, kind: &OutputKind, names: Names) {
    let palette = ui.theme.palette;
    ui.panel(
        &Panel::new("OUTPUT SCRIPT")
            .line(Text::of(
                "not a transaction — read as a single output script",
                palette.muted,
            ))
            .rule()
            .wrapped(0, describe(ui, kind, names)),
    );
}

/// How far an output's description is indented under its `#n` line.
///
/// Named because it is also the difference between the panel's width and the
/// room a description has, and that difference is what makes a whole currency
/// id safe to print here: the narrowest frame this tool draws is 48 columns, so
/// a description always has at least 43 — nine more than the 34 an i-address
/// takes. See [`push_currency`].
const OUTPUT_INDENT: usize = 5;

/// The satoshi value of every output added up, or `None` when they do not fit.
///
/// The bytes come from whoever handed them over — `tx explain` takes free-form
/// hex or stdin, `broadcast` takes hex or a QR — and `TxV4::deserialize` reads
/// each value as a raw `u64` with no range check, so nothing upstream rules out
/// two outputs of `u64::MAX`. A bare `sum::<u64>()` panicked in a debug build
/// and wrapped in a release one, printing a total *smaller than one of the
/// outputs it totalled*. No such transaction can be mined — the sum has to pass
/// ~184 billion coins to wrap, four orders of magnitude beyond supply — so the
/// honest answer is not an error, it is that there is no number to print.
pub fn total_output_value(transaction: &TxV4) -> Option<Amount> {
    Amount::checked_sum(
        transaction
            .outputs
            .iter()
            .map(|out| Amount::from_sat(out.value)),
    )
}

/// Zero satoshis on a Verus output is normal and meaningful, so it is muted
/// rather than shouted: the value is in the payload.
fn value_style(ui: &Ui, satoshis: u64) -> anstyle::Style {
    if satoshis == 0 {
        ui.theme.palette.muted
    } else {
        ui.theme.palette.accent
    }
}

/// The same, straight from a script.
///
/// An undecodable output is not fatal: it sits beside ones that decoded fine,
/// and refusing the whole transaction over it would throw away the answer the
/// caller came for.
fn describe_script(ui: &Ui, script: &[u8], names: Names) -> Text {
    match decode_output_script(script) {
        Ok(kind) => describe(ui, &kind, names),
        Err(error) => Text::of(format!("undecodable: {error}"), ui.theme.palette.danger),
    }
}

/// One line per output kind.
///
/// The satoshi value is printed by the caller; what this adds is everything the
/// satoshi value does not say. Every branch is `decode_output_script` telling
/// you something, and the variants of `OutputKind` are a compact map of what a
/// Verus output can be.
fn describe(ui: &Ui, kind: &OutputKind, names: Names) -> Text {
    let palette = ui.theme.palette;
    let glyphs = ui.theme.glyphs;

    match kind {
        OutputKind::PubKeyHash { hash } => Text::of(glyphs.arrow, palette.muted)
            .space()
            .push(address(AddressKind::PubKeyHash, *hash), palette.value),

        OutputKind::PubKey { pubkey, hash } => Text::of(glyphs.arrow, palette.muted)
            .space()
            .push(address(AddressKind::PubKeyHash, *hash), palette.value)
            .push(
                format!(
                    "  pays a bare public key, {} bytes — a mined coinbase",
                    pubkey.len()
                ),
                palette.muted,
            ),

        OutputKind::IdentityPayment { identity } => Text::of(glyphs.arrow, palette.muted)
            .space()
            .push(address(AddressKind::Identity, *identity), palette.value)
            .push("  held for a VerusID, not a key", palette.muted),

        OutputKind::ReserveOutput {
            destination,
            tokens,
        } => push_currencies(
            Text::of(glyphs.arrow, palette.muted)
                .space()
                .push(show(destination), palette.value)
                .push(" holds ", palette.muted),
            ui,
            tokens,
            names,
        ),

        OutputKind::IdentityPrimary { identity } => {
            let mut line = Text::of("the VerusID ", palette.muted)
                .push(
                    format!(
                        "{}@",
                        fmt::untrusted(&identity.name, NAME_BUDGET, glyphs.ellipsis)
                    ),
                    palette.accent,
                )
                .push(
                    format!(
                        " — {}-of-{}",
                        identity.min_sigs,
                        identity.primary_addresses.len()
                    ),
                    palette.value,
                );
            for (what, authority) in [
                ("revocation", identity.revocation_authority),
                ("recovery", identity.recovery_authority),
            ] {
                line = line.push(
                    format!(
                        ", {what} {}",
                        fmt::address(&address(AddressKind::Identity, authority), glyphs.ellipsis)
                    ),
                    palette.muted,
                );
            }
            let published = identity.content_multimap.len() + identity.content_map.len();
            if published > 0 {
                line = line.push(
                    format!(
                        ", {}",
                        fmt::plural(published, "content key", "content keys")
                    ),
                    palette.muted,
                );
            }
            line
        }

        OutputKind::IdentityCommitment {
            destination,
            commitment,
            tokens,
        } => {
            let mut line = Text::of("a name commitment", palette.value)
                .push(" redeemable by ", palette.muted)
                .push(show(destination), palette.value)
                .push(
                    format!(
                        ", hash {}",
                        fmt::hash(&hex::encode(commitment), glyphs.ellipsis)
                    ),
                    palette.muted,
                );
            if !tokens.is_empty() {
                line = push_currencies(line.push(", carrying ", palette.muted), ui, tokens, names);
            }
            line
        }

        OutputKind::ReserveDeposit {
            controlling_currency,
            tokens,
            ..
        } => {
            let line = push_currency(
                Text::of("reserves held for ", palette.muted),
                ui,
                controlling_currency,
                names,
            )
            .push(": ", palette.muted);
            push_currencies(line, ui, tokens, names)
        }

        OutputKind::ReserveTransfer { transfer, .. } => {
            let line = Text::of("value in flight ", palette.muted)
                .push(glyphs.arrow, palette.muted)
                .space()
                .push(show(&transfer.destination.recipient), palette.value)
                .push(" as ", palette.muted);
            let line = push_currency(line, ui, &transfer.destination_currency, names);
            let line = line.push(
                format!(", {} fee in ", fmt::sats(transfer.fees)),
                palette.muted,
            );
            push_currency(line, ui, &transfer.fee_currency, names)
                .push(format!(", flags {:#x}", transfer.flags), palette.muted)
        }

        // The honest answer, and the one worth reading carefully. `false` means
        // the output is undecodable *and* provably holds no currency, so
        // ignoring it costs nothing. `true` means something may be in there.
        OutputKind::UnsupportedCryptoCondition {
            eval_code,
            may_carry_currency,
        } => {
            let line = Text::of(
                format!("a CryptoCondition this SDK does not decode (eval {eval_code})"),
                palette.value,
            );
            if *may_carry_currency {
                line.push(" — ", palette.muted)
                    .push(glyphs.warn, palette.danger)
                    .space()
                    .push(
                        "IT MAY HOLD CURRENCY; do not treat this output as empty",
                        palette.danger,
                    )
            } else {
                line.push(" — it cannot hold currency", palette.muted)
            }
        }

        // `OutputKind` is `#[non_exhaustive]`: a future variant should print
        // something honest here rather than fail to compile a consumer.
        other => Text::of(format!("{other:?}"), palette.muted),
    }
}

/// A destination as the address it is, for every kind an output can name.
///
/// Shared with the `send` confirmation panel, which decodes the same reserve
/// outputs this reads back. While that panel had a rendering of its own it
/// printed `PubKeyHash([38, 176, …])` where the recipient belongs — twenty
/// numbers nobody can compare by eye to the address they typed, on the one row
/// that says where the token goes.
pub fn show(destination: &Destination) -> String {
    match destination {
        Destination::PubKeyHash(hash) => address(AddressKind::PubKeyHash, *hash),
        Destination::Identity(hash) => address(AddressKind::Identity, *hash),
        Destination::ScriptHash(hash) => address(AddressKind::ScriptHash, *hash),
        Destination::PubKey(key) => format!("public key {}", hex::encode(key)),
    }
}

fn address(kind: AddressKind, hash: [u8; 20]) -> String {
    Address::new(kind, hash).to_string()
}

/// A currency id as the `i` address it is.
///
/// `CurrencyId: Display` prints the raw 20 bytes as hex, which is correct and
/// unreadable — every explorer, every RPC reply and every other line of this
/// program says `iJhCe…`.
fn currency(id: &CurrencyId) -> String {
    Address::new(AddressKind::Identity, id.to_bytes()).to_string()
}

/// One currency, appended to the line being built: what the node calls it when
/// something asked, and always the id.
///
/// The name and the id are not alternatives. A name is display text a
/// registrant chose and a node repeated, so a lookalike costs nothing to
/// register; the id is the part that identifies the currency, and it stays on
/// the line whether or not a name turned up beside it. The `send` panel carries
/// a full `currency id` row for the same reason, and its comment names *this*
/// renderer as why it had to.
fn push_currency(text: Text, ui: &Ui, id: &CurrencyId, names: Names) -> Text {
    let palette = ui.theme.palette;
    let glyphs = ui.theme.glyphs;
    let address = currency(id);
    // Two levels of `Option`, and they mean different things. The outer one is
    // whether anything was asked at all; the inner one is what came back.
    let text = match names.map(|names| names.get(id)) {
        // Nothing was asked, so nothing is said. The wallet's renderer prints
        // `(name unknown)` for a missing verdict and is right to — there a
        // lookup really was attempted — but here it would report a failure on a
        // path that was never going to ask.
        //
        // A currency in a set the deadline cut short is missing from the map
        // for the same reason and renders the same way: it was never asked
        // about either. It must not borrow `(name unknown)` from the arm below
        // — that reports a question the node was asked and did not answer.
        None | Some(None) => text,
        Some(Some(CurrencyName::Known(name))) => text.push(
            format!("{}@ ", fmt::untrusted(name, NAME_BUDGET, glyphs.ellipsis)),
            palette.accent,
        ),
        // The node did not say this currency is nameless; it said it has no such
        // currency at all, which is a statement about the currency and not about
        // its name — and this output is holding a balance in it.
        Some(Some(CurrencyName::Absent)) => text.push("(no such currency) ", palette.muted),
        // A lookup that failed says nothing whatever about the currency, so this
        // must not read as a confident nothing. In the warning colour, so a
        // reader can tell it apart from something the node actually answered.
        Some(Some(CurrencyName::Failed(_))) => text.push("(name unknown) ", palette.warn),
    };
    text.push(fmt::id(&address, glyphs.ellipsis), palette.value)
}

/// `(currency, amount)` pairs, which is where a token's value actually lives.
fn push_currencies(text: Text, ui: &Ui, tokens: &[(CurrencyId, u64)], names: Names) -> Text {
    let palette = ui.theme.palette;
    if tokens.is_empty() {
        return text.push("no currency", palette.muted);
    }
    let mut text = text;
    for (index, (id, amount)) in tokens.iter().enumerate() {
        if index > 0 {
            text = text.push(", ", palette.muted);
        }
        text = push_currency(
            text.push(fmt::sats(*amount), palette.accent).space(),
            ui,
            id,
            names,
        );
    }
    text
}

/// A txid the way RPC prints it — the reverse of how it is serialized.
fn hex_reversed(mut bytes: [u8; 32]) -> String {
    bytes.reverse();
    hex::encode(bytes)
}

fn emit_transaction_json(transaction: &TxV4, names: Names) {
    let outputs: Vec<serde_json::Value> = transaction
        .outputs
        .iter()
        .enumerate()
        .map(|(index, output)| {
            let mut value = match decode_output_script(&output.script_pubkey) {
                Ok(kind) => output_json(&kind, Some(output.value), names),
                Err(error) => serde_json::json!({
                    "kind": "undecodable",
                    "error": error.to_string(),
                    "satoshis": output.value,
                }),
            };
            value
                .as_object_mut()
                .expect("output_json always builds an object")
                .insert("index".into(), serde_json::json!(index));
            value
        })
        .collect();

    emit_json(&serde_json::json!({
        "kind": "transaction",
        "txid": transaction.txid().map(hex_reversed).ok(),
        "expiry_height": transaction.expiry_height,
        "shielded": transaction.is_shielded(),
        "inputs": transaction.inputs.iter().map(|input| {
            let mut txid = input.txid_internal;
            txid.reverse();
            serde_json::json!({
                "txid": hex::encode(txid),
                "vout": input.vout,
                "signed": !input.script_sig.is_empty(),
            })
        }).collect::<Vec<_>>(),
        "outputs": outputs,
        "total_satoshis": total_output_value(transaction).map(Amount::to_sat),
    }));
}

/// The machine-readable form of one output. Deliberately flatter than the
/// rendered line: a consumer wants the discriminant and the fields, not prose.
fn output_json(kind: &OutputKind, satoshis: Option<u64>, names: Names) -> serde_json::Value {
    let mut value = match kind {
        OutputKind::PubKeyHash { hash } => serde_json::json!({
            "kind": "pubkey_hash",
            "address": address(AddressKind::PubKeyHash, *hash),
        }),
        OutputKind::PubKey { pubkey, hash } => serde_json::json!({
            "kind": "pubkey",
            "address": address(AddressKind::PubKeyHash, *hash),
            "pubkey_bytes": pubkey.len(),
        }),
        OutputKind::IdentityPayment { identity } => serde_json::json!({
            "kind": "identity_payment",
            "identity": address(AddressKind::Identity, *identity),
        }),
        OutputKind::ReserveOutput {
            destination,
            tokens,
        } => serde_json::json!({
            "kind": "reserve_output",
            "destination": show(destination),
            "tokens": tokens_json(tokens, names),
        }),
        OutputKind::IdentityPrimary { identity } => serde_json::json!({
            "kind": "identity_primary",
            "name": identity.name,
            "min_sigs": identity.min_sigs,
            "primary_addresses": identity.primary_addresses.len(),
            "revocation": address(AddressKind::Identity, identity.revocation_authority),
            "recovery": address(AddressKind::Identity, identity.recovery_authority),
            "content_keys": identity.content_multimap.len() + identity.content_map.len(),
        }),
        OutputKind::IdentityCommitment {
            destination,
            commitment,
            tokens,
        } => serde_json::json!({
            "kind": "identity_commitment",
            "destination": show(destination),
            "commitment": hex::encode(commitment),
            "tokens": tokens_json(tokens, names),
        }),
        OutputKind::ReserveDeposit {
            controlling_currency,
            tokens,
            ..
        } => serde_json::json!({
            "kind": "reserve_deposit",
            "controlling_currency": currency(controlling_currency),
            "controlling_currency_name": name_json(verdict(names, controlling_currency)),
            "tokens": tokens_json(tokens, names),
        }),
        OutputKind::ReserveTransfer { transfer, .. } => serde_json::json!({
            "kind": "reserve_transfer",
            "recipient": show(&transfer.destination.recipient),
            "destination_currency": currency(&transfer.destination_currency),
            "destination_currency_name": name_json(verdict(names, &transfer.destination_currency)),
            "fees": transfer.fees,
            "fee_currency": currency(&transfer.fee_currency),
            "fee_currency_name": name_json(verdict(names, &transfer.fee_currency)),
            "flags": transfer.flags,
        }),
        OutputKind::UnsupportedCryptoCondition {
            eval_code,
            may_carry_currency,
        } => serde_json::json!({
            "kind": "unsupported_cryptocondition",
            "eval_code": eval_code,
            "may_carry_currency": may_carry_currency,
        }),
        other => serde_json::json!({ "kind": "unknown", "debug": format!("{other:?}") }),
    };
    if let (Some(satoshis), Some(object)) = (satoshis, value.as_object_mut()) {
        object.insert("satoshis".into(), serde_json::json!(satoshis));
    }
    value
}

/// Names in JSON are additive: every currency field a consumer already reads is
/// still the full i-address it was, with a three-state `…_name` beside it.
///
/// A path that asked nothing renders through the same three states as one whose
/// lookup failed, and lands on `{"known": false, "error": "the name was not
/// looked up"}` — which is the literal truth of a run that had no node, and says
/// it without inventing a currency that has no name.
fn tokens_json(tokens: &[(CurrencyId, u64)], names: Names) -> Vec<serde_json::Value> {
    tokens
        .iter()
        .map(|(id, amount)| {
            serde_json::json!({
                "currency": currency(id),
                "name": name_json(verdict(names, id)),
                "satoshis": amount,
            })
        })
        .collect()
}

/// What was learned about one currency, where "nothing was asked" and "nothing
/// came back" both land on `None` — which is the arm `name_json` renders as
/// `the name was not looked up`, and the literal truth of an offline run.
fn verdict<'a>(names: Names<'a>, id: &CurrencyId) -> Option<&'a CurrencyName> {
    names.and_then(|names| names.get(id))
}

fn emit_json(value: &serde_json::Value) {
    crate::failure::document(value);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::text::strip_ansi;
    use crate::ui::theme::Skin;
    use crate::ui::Theme;

    /// VRSCTEST itself — the chain's own currency, and the one the README's
    /// headline command printed as `iJhCezBEx…f2yq`.
    const NATIVE: &str = "iJhCezBExJHvtyH3fGhNnt2NhU4Ztkf2yq";

    fn id(i_address: &str) -> CurrencyId {
        CurrencyId::from_bytes(
            i_address
                .parse::<Address>()
                .expect("a valid i-address")
                .hash(),
        )
    }

    /// The framed skin at a chosen width, so an assertion about elision does not
    /// depend on the terminal the tests happen to run in.
    fn ui(width: usize) -> Ui {
        let mut ui = Ui::new(crate::cli::Theme::Phosphor, false, false);
        ui.theme = Theme::with_skin(Skin::Phosphor, width);
        ui
    }

    /// The reserve deposit out of a real VRSCTEST currency launch: output #5,
    /// holding 100 VRSCTEST. Taken from the bytes rather than built by hand,
    /// because what is being asserted is what a reader sees about a transaction
    /// the daemon really produced.
    fn reserve_deposit() -> OutputKind {
        let hex = include_str!("../../fixtures/currency-launch-fractional-one-reserve.hex");
        let bytes = hex::decode(hex.trim()).expect("a valid fixture");
        let transaction = TxV4::deserialize(&bytes).expect("a transaction");
        transaction
            .outputs
            .iter()
            .find_map(|output| match decode_output_script(&output.script_pubkey) {
                Ok(kind @ OutputKind::ReserveDeposit { .. }) => Some(kind),
                _ => None,
            })
            .expect("the fixture holds a reserve deposit")
    }

    /// The line as the panel would lay it out, escapes stripped.
    fn line(ui: &Ui, names: Names) -> String {
        let kind = reserve_deposit();
        strip_ansi(&describe(ui, &kind, names).render())
    }

    fn named(verdict: CurrencyName) -> BTreeMap<CurrencyId, CurrencyName> {
        BTreeMap::from([(id(NATIVE), verdict)])
    }

    #[test]
    fn an_id_nobody_looked_up_is_still_an_id_you_can_copy() {
        // The bug as filed. `iJhCezBEx…f2yq` is not an address — base58check
        // over a truncated one does not decode — so `pecu currency show` on what
        // this printed could not work, at any terminal width.
        let rendered = line(&ui(84), None);
        assert!(rendered.contains(NATIVE), "{rendered}");
        assert!(!rendered.contains('…'), "an id was elided:\n{rendered}");
    }

    #[test]
    fn an_offline_line_claims_nothing_about_a_name() {
        // Nothing was asked, so nothing is said. The wallet prints
        // `(name unknown)` for a missing verdict and is right to — there a
        // lookup really was attempted — but here it would report a failure that
        // never happened.
        let rendered = line(&ui(84), None);
        assert!(!rendered.contains("name unknown"), "{rendered}");
        assert!(!rendered.contains("no such currency"), "{rendered}");
    }

    #[test]
    fn a_name_never_replaces_the_id_it_stands_beside() {
        let names = named(CurrencyName::Known("VRSCTEST".into()));
        let rendered = line(&ui(84), Some(&names));
        assert!(rendered.contains("VRSCTEST@"), "{rendered}");
        // Whole, not shortened: a node can hand back a lookalike name, and the
        // id is the half of the pair that settles which currency this is.
        assert!(rendered.contains(NATIVE), "{rendered}");
    }

    #[test]
    fn a_currency_the_budget_never_reached_reads_as_one_nobody_looked_up() {
        // The naming step is bounded as a whole, so a run against a slow node
        // can come back having asked about some currencies and not others. The
        // ones it did not reach are missing from the map — the same absence as
        // a currency on a path with no node at all — and they have to render
        // the same way. `(name unknown)` here would report a question the node
        // was asked and did not answer, which is a different thing entirely.
        let reached_none = BTreeMap::new();
        let rendered = line(&ui(84), Some(&reached_none));
        assert!(!rendered.contains("name unknown"), "{rendered}");
        assert!(!rendered.contains("no such currency"), "{rendered}");
        assert!(rendered.contains(NATIVE), "{rendered}");
    }

    #[test]
    fn a_lookup_that_failed_is_not_a_currency_without_a_name() {
        let names = named(CurrencyName::Failed("timed out".into()));
        let rendered = line(&ui(84), Some(&names));
        assert!(rendered.contains("(name unknown)"), "{rendered}");
        assert!(rendered.contains(NATIVE), "{rendered}");
    }

    #[test]
    fn a_currency_the_node_has_no_record_of_says_that_and_not_that_it_is_nameless() {
        // A node that has no such currency has not told us this currency is
        // nameless — and the output being described is holding a balance in it.
        let names = named(CurrencyName::Absent);
        let rendered = line(&ui(84), Some(&names));
        assert!(rendered.contains("(no such currency)"), "{rendered}");
        assert!(rendered.contains(NATIVE), "{rendered}");
    }

    #[test]
    fn the_narrowest_frame_this_tool_draws_still_carries_a_whole_id() {
        // The reason there is no shortened form here at all. A panel is never
        // drawn below `MIN_WIDTH`, an output's description is indented by
        // `OUTPUT_INDENT`, and an i-address is exactly 34 characters — so the
        // room left over is never less than an id needs, and `Panel::wrapped`
        // breaks a long line at its spaces rather than cutting it. Asked of the
        // narrowest terminal a caller can ask for.
        let narrowest = ui(1);
        assert!(
            narrowest.theme.width - OUTPUT_INDENT >= NATIVE.chars().count(),
            "an output description no longer has room for a whole id"
        );
        assert!(line(&narrowest, None).contains(NATIVE));
    }
}
