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
//! fetch the bytes; the decoding is the same either way.

use std::io::Read;

use miette::Diagnostic;
use thiserror::Error;
use verus_sdk::decode::{decode_output_script, Destination, OutputKind};
use verus_sdk::money::Amount;
use verus_sdk::network::ChainReader;
use verus_sdk::send::CurrencyId;
use verus_sdk::verus_keys::{Address, AddressKind};
use verus_sdk::verus_wire::TxV4;

use crate::config::Settings;
use crate::node::{self, NodeError};
use crate::ui::{fmt, Panel, Text, Ui};

/// A node-supplied name is display text and nothing more; this is how much of
/// one is ever printed.
const NAME_BUDGET: usize = 32;

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
    let source = read_input(input)?;
    let bytes = match &source {
        Source::Txid(txid) => fetch(settings, txid)?,
        Source::Hex(hex) => hex::decode(hex).map_err(|error| TxError::NotHex {
            detail: error.to_string(),
        })?,
    };

    // A transaction first, because that is what a caller almost always has. A
    // bare script is the useful fallback: "what does this scriptPubKey do" comes
    // up while debugging a builder, and at that point there is no transaction to
    // put it in yet.
    match TxV4::deserialize(&bytes) {
        Ok(transaction) => {
            if ui.is_json() {
                emit_transaction_json(&transaction);
            } else {
                render_transaction(ui, &transaction);
            }
            Ok(())
        }
        Err(transaction_error) => match decode_output_script(&bytes) {
            Ok(kind) => {
                if ui.is_json() {
                    emit_json(&serde_json::json!({
                        "kind": "output_script",
                        "output": output_json(&kind, None),
                    }));
                } else {
                    render_script(ui, &kind);
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

fn fetch(settings: &Settings, txid: &str) -> Result<Vec<u8>, miette::Report> {
    let url = &settings.profile.node;
    let node = node::connect(&settings.profile)?;
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

fn render_transaction(ui: &Ui, transaction: &TxV4) {
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
        header = header.wrapped(5, describe_script(ui, &output.script_pubkey));
    }

    ui.panel(&header);
}

fn render_script(ui: &Ui, kind: &OutputKind) {
    let palette = ui.theme.palette;
    ui.panel(
        &Panel::new("OUTPUT SCRIPT")
            .line(Text::of(
                "not a transaction — read as a single output script",
                palette.muted,
            ))
            .rule()
            .wrapped(0, describe(ui, kind)),
    );
}

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
fn describe_script(ui: &Ui, script: &[u8]) -> Text {
    match decode_output_script(script) {
        Ok(kind) => describe(ui, &kind),
        Err(error) => Text::of(format!("undecodable: {error}"), ui.theme.palette.danger),
    }
}

/// One line per output kind.
///
/// The satoshi value is printed by the caller; what this adds is everything the
/// satoshi value does not say. Every branch is `decode_output_script` telling
/// you something, and the variants of `OutputKind` are a compact map of what a
/// Verus output can be.
fn describe(ui: &Ui, kind: &OutputKind) -> Text {
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
        } => Text::of(glyphs.arrow, palette.muted)
            .space()
            .push(show(destination), palette.value)
            .push(" holds ", palette.muted)
            .push(currencies(ui, tokens), palette.accent),

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
                line = line
                    .push(", carrying ", palette.muted)
                    .push(currencies(ui, tokens), palette.accent);
            }
            line
        }

        OutputKind::ReserveDeposit {
            controlling_currency,
            tokens,
            ..
        } => Text::of("reserves held for ", palette.muted)
            .push(currency(controlling_currency), palette.value)
            .push(": ", palette.muted)
            .push(currencies(ui, tokens), palette.accent),

        OutputKind::ReserveTransfer { transfer, .. } => Text::of("value in flight ", palette.muted)
            .push(glyphs.arrow, palette.muted)
            .space()
            .push(show(&transfer.destination.recipient), palette.value)
            .push(
                format!(
                    " as {}, {} fee in {}, flags {:#x}",
                    currency(&transfer.destination_currency),
                    fmt::sats(transfer.fees),
                    currency(&transfer.fee_currency),
                    transfer.flags
                ),
                palette.muted,
            ),

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

/// `(currency, amount)` pairs, which is where a token's value actually lives.
fn currencies(ui: &Ui, tokens: &[(CurrencyId, u64)]) -> String {
    if tokens.is_empty() {
        return "no currency".to_string();
    }
    tokens
        .iter()
        .map(|(id, amount)| {
            format!(
                "{} {}",
                fmt::sats(*amount),
                fmt::address(&currency(id), ui.theme.glyphs.ellipsis)
            )
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// A txid the way RPC prints it — the reverse of how it is serialized.
fn hex_reversed(mut bytes: [u8; 32]) -> String {
    bytes.reverse();
    hex::encode(bytes)
}

fn emit_transaction_json(transaction: &TxV4) {
    let outputs: Vec<serde_json::Value> = transaction
        .outputs
        .iter()
        .enumerate()
        .map(|(index, output)| {
            let mut value = match decode_output_script(&output.script_pubkey) {
                Ok(kind) => output_json(&kind, Some(output.value)),
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
fn output_json(kind: &OutputKind, satoshis: Option<u64>) -> serde_json::Value {
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
            "tokens": tokens_json(tokens),
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
            "tokens": tokens_json(tokens),
        }),
        OutputKind::ReserveDeposit {
            controlling_currency,
            tokens,
            ..
        } => serde_json::json!({
            "kind": "reserve_deposit",
            "controlling_currency": currency(controlling_currency),
            "tokens": tokens_json(tokens),
        }),
        OutputKind::ReserveTransfer { transfer, .. } => serde_json::json!({
            "kind": "reserve_transfer",
            "recipient": show(&transfer.destination.recipient),
            "destination_currency": currency(&transfer.destination_currency),
            "fees": transfer.fees,
            "fee_currency": currency(&transfer.fee_currency),
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

fn tokens_json(tokens: &[(CurrencyId, u64)]) -> Vec<serde_json::Value> {
    tokens
        .iter()
        .map(|(id, amount)| serde_json::json!({ "currency": currency(id), "satoshis": amount }))
        .collect()
}

fn emit_json(value: &serde_json::Value) {
    crate::failure::document(value);
}
