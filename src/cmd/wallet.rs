//! `pecu wallet …` — what an address holds, and the outputs behind it.
//!
//! Read-only, and deliberately so: this takes an address, not a key. Watching a
//! balance is the one wallet job that needs no secret at all, and a command that
//! cannot unlock anything cannot spend anything either.
//!
//! The interesting part is that a Verus balance is three numbers, not one.
//! What is spendable now; what exists but is immature; and what is held in
//! CryptoCondition outputs, whose value is in the *payload* rather than the
//! satoshi field. An address can report zero satoshis while holding a fortune in
//! tokens, which is why `token_balances` is asked separately and why a failure
//! there means "unknown" rather than "none".

use miette::Diagnostic;
use thiserror::Error;
use verus_sdk::decode::{decode_output_script, OutputKind};
use verus_sdk::money::Amount;
use verus_sdk::network::{
    currency_names, native_currency, spendable, AddressUtxo, FlowError, Funding, RpcError,
    TokenBalances,
};
use verus_sdk::send::CurrencyId;
use verus_sdk::verus_keys::{Address, AddressKind};

use crate::config::Settings;
use crate::keystore::Keystore;
use crate::node::{self, Node};
use crate::ui::{fmt, Align, Column, Panel, Table, Text, Ui};

/// How much of a node-supplied currency name is ever printed.
const NAME_BUDGET: usize = 24;

#[derive(Debug, Error, Diagnostic)]
pub enum WalletError {
    #[error("no address to look at")]
    #[diagnostic(
        code(pecu::no_address),
        help("pass --address R…, or --key <label> to use a stored key, or make one with `pecu key gen`")
    )]
    NoAddress,

    #[error("the keystore holds {count} keys, so there is no obvious default")]
    #[diagnostic(
        code(pecu::ambiguous_key),
        help("name one with --key <label>; `pecu key list` shows them")
    )]
    AmbiguousKey { count: usize },

    #[error("`{address}` is not a Verus address")]
    #[diagnostic(
        code(pecu::bad_address),
        help("transparent addresses start with R; identities are i-addresses")
    )]
    BadAddress { address: String },

    #[error("{what} for {address} failed")]
    #[diagnostic(code(pecu::flow_failed), help("{advice}"))]
    Flow {
        what: &'static str,
        /// Named in the message. Which address was being read is the first
        /// thing you want to know when a balance fails, and it is often not the
        /// one you meant — a stored key resolves silently.
        address: String,
        advice: String,
        #[source]
        source: Box<FlowError>,
    },
}

impl WalletError {
    fn flow(what: &'static str, address: &str, source: FlowError) -> Self {
        let advice = match &source {
            // Not a broken node and not a wrong URL: the node answered
            // correctly, with more data than the memory bound allows. The
            // remedy is a setting, not a retry.
            FlowError::Rpc(RpcError::ResponseTooLarge { cap }) => format!(
                "the reply was over the {} MiB ceiling — this address has an unusually large \
                 number of outputs. Raise it with `max_response_mb` under this profile in \
                 config.toml.",
                cap / (1024 * 1024)
            ),
            FlowError::Rpc(RpcError::Transport(_)) => {
                "the node could not be reached — check your connection, or point somewhere else \
                 with --node"
                    .to_string()
            }
            FlowError::Key(_) => {
                "that address did not parse — `pecu key list` shows the stored ones".to_string()
            }
            _ => "the node answered, but not with what this SDK build expected — try --node, \
                  or `pecu doctor`"
                .to_string(),
        };
        Self::Flow {
            what,
            address: address.to_string(),
            advice,
            source: Box::new(source),
        }
    }
}

/// Work out which address a read-only command should look at.
///
/// `--address` wins; then `--key <label>`; then the sole stored key, if there is
/// exactly one. Guessing between several keys would silently report the wrong
/// balance, so that is refused rather than resolved.
pub fn resolve_address(
    settings: &Settings,
    address: Option<&str>,
    key: Option<&str>,
) -> Result<String, miette::Report> {
    if let Some(address) = address {
        return validate(address).map_err(Into::into);
    }

    let store = Keystore::new(&settings.paths);
    if let Some(label) = key {
        return Ok(store.load(label)?.address);
    }

    let keys = store.list()?;
    match keys.len() {
        0 => Err(WalletError::NoAddress.into()),
        1 => Ok(keys[0].address.clone()),
        count => Err(WalletError::AmbiguousKey { count }.into()),
    }
}

/// Parse-don't-trust. A typo'd address would otherwise be sent to the node and
/// come back as an empty balance, which reads as "no funds" — the one wrong
/// answer a wallet must never give. Base58check catches it here instead.
fn validate(address: &str) -> Result<String, WalletError> {
    address
        .parse::<Address>()
        .map(|_| address.to_string())
        .map_err(|_| WalletError::BadAddress {
            address: address.to_string(),
        })
}

/// Everything both subcommands need, gathered once.
struct Wallet {
    address: String,
    funding: Funding,
    native: Option<CurrencyId>,
}

/// Native value sitting in CryptoCondition outputs.
///
/// `Funding::other` is not a curiosity to be counted and dropped. The native
/// builders refuse those outputs — a reserve output's value is in its payload,
/// so spending one as ordinary funding would destroy what it carries — but for
/// a **VerusID** the bucket *is* the balance: an identity's funds are held in
/// pay-to-identity outputs, spendable by its authority rather than by a key.
///
/// Reporting only a count of them is how this wallet showed 0 for an address
/// holding 7.7 million.
#[derive(Default)]
struct Conditions {
    /// Native satoshis held for a VerusID.
    identity: Amount,
    identity_outputs: usize,
    /// Native satoshis in every other CryptoCondition output. Usually zero —
    /// a reserve output carries its value in the payload, not the satoshi field.
    other: Amount,
    other_outputs: usize,
}

impl Conditions {
    fn total(&self) -> Amount {
        self.identity
            .checked_add(self.other)
            .unwrap_or(Amount::ZERO)
    }
}

/// Split the CryptoCondition outputs by what their scripts actually say.
///
/// The scripts are already in hand, so this costs no network and no node
/// opinion — it is the same `decode_output_script` that `tx explain` uses.
fn classify(others: &[AddressUtxo]) -> Conditions {
    let mut found = Conditions::default();
    for held in others {
        let satoshis = held.utxo.satoshis;
        let is_identity = matches!(
            decode_output_script(&held.utxo.script_pubkey),
            Ok(OutputKind::IdentityPayment { .. })
        );
        let (total, count) = if is_identity {
            (&mut found.identity, &mut found.identity_outputs)
        } else {
            (&mut found.other, &mut found.other_outputs)
        };
        *total = total.checked_add(satoshis).unwrap_or(*total);
        *count += 1;
    }
    found
}

fn gather(settings: &Settings, address: String) -> Result<(Node, Wallet), miette::Report> {
    let node = node::connect(&settings.profile)?;
    let funding = spendable(&node, &address)
        .map_err(|source| WalletError::flow("reading the outputs", &address, source))?;
    // `None` is a legitimate answer meaning "I do not know it", and only affects
    // reserve deposits and transfers. Not worth failing a balance over.
    let native = native_currency(&node).ok();
    Ok((
        node,
        Wallet {
            address,
            funding,
            native,
        },
    ))
}

pub fn balance(
    ui: &Ui,
    settings: &Settings,
    address: Option<&str>,
    key: Option<&str>,
) -> miette::Result<()> {
    let address = resolve_address(settings, address, key)?;
    let (node, wallet) = gather(settings, address)?;

    let spendable_tokens = wallet.funding.token_balances(wallet.native);
    let immature_tokens = wallet.funding.immature_token_balances(wallet.native);
    let conditions = classify(&wallet.funding.other);
    // What an explorer shows, and therefore the figure worth being able to
    // cross-check at a glance.
    let total = [
        wallet.funding.total,
        wallet.funding.immature_total(),
        conditions.total(),
    ]
    .into_iter()
    .try_fold(Amount::ZERO, Amount::checked_add)
    .unwrap_or(Amount::ZERO);

    // One request per currency, so only asked when there is something to name.
    let names = match &spendable_tokens {
        Ok(held) if !held.is_empty() => {
            currency_names(&node, held.keys().copied()).unwrap_or_default()
        }
        _ => Default::default(),
    };

    if ui.is_json() {
        emit_json(&serde_json::json!({
            "address": wallet.address,
            "tip": wallet.funding.tip,
            "currency": settings.profile.currency,
            "spendable": {
                "satoshis": wallet.funding.total.to_sat(),
                "outputs": wallet.funding.utxos.len(),
            },
            "withheld": {
                "satoshis": wallet.funding.immature_total().to_sat(),
                "outputs": wallet.funding.immature.len(),
            },
            "held_for_identity": {
                "satoshis": conditions.identity.to_sat(),
                "outputs": conditions.identity_outputs,
            },
            "in_conditions": {
                "satoshis": conditions.other.to_sat(),
                "outputs": conditions.other_outputs,
            },
            "total_satoshis": total.to_sat(),
            "tokens": tokens_json(&spendable_tokens, &names),
            "withheld_tokens": tokens_json(&immature_tokens, &names),
        }));
        return Ok(());
    }

    let palette = ui.theme.palette;
    let glyphs = ui.theme.glyphs;
    let currency = &settings.profile.currency;

    let mut totals = Table::headerless([Align::Left, Align::Right, Align::Left, Align::Left]);
    totals.push(vec![
        Text::of("SPENDABLE", palette.label),
        Text::of(fmt::amount(wallet.funding.total), palette.accent),
        Text::of(currency, palette.muted),
        Text::of(
            format!(
                "({})",
                fmt::plural(wallet.funding.utxos.len(), "output", "outputs")
            ),
            palette.muted,
        ),
    ]);
    if !wallet.funding.immature.is_empty() {
        totals.push(vec![
            Text::of("WITHHELD", palette.label),
            Text::of(fmt::amount(wallet.funding.immature_total()), palette.warn),
            Text::of(currency, palette.muted),
            Text::of(
                format!(
                    "({})",
                    fmt::plural(wallet.funding.immature.len(), "output", "outputs")
                ),
                palette.muted,
            ),
        ]);
    }

    let mut row = |label: &str, amount: Amount, style: anstyle::Style, outputs: usize| {
        totals.push(vec![
            Text::of(label, palette.label),
            Text::of(fmt::amount(amount), style),
            Text::of(currency, palette.muted),
            Text::of(
                format!("({})", fmt::plural(outputs, "output", "outputs")),
                palette.muted,
            ),
        ]);
    };
    if conditions.identity_outputs > 0 {
        row(
            "HELD BY ID",
            conditions.identity,
            palette.value,
            conditions.identity_outputs,
        );
    }
    if conditions.other_outputs > 0 {
        row(
            "IN CONDITIONS",
            conditions.other,
            palette.value,
            conditions.other_outputs,
        );
    }
    // Only worth a line when it is not simply the spendable figure again.
    if total != wallet.funding.total {
        totals.push(vec![
            Text::of("TOTAL", palette.label),
            Text::of(fmt::amount(total), palette.accent),
            Text::of(currency, palette.muted),
            Text::new(),
        ]);
    }

    let mut panel = Panel::new("WALLET")
        .row("address", Text::of(&wallet.address, palette.value))
        .row(
            "tip",
            Text::of(glyphs.bullet, palette.accent)
                .space()
                .push(fmt::height(wallet.funding.tip.into()), palette.accent),
        )
        .rule()
        .table(totals);

    panel = match &spendable_tokens {
        Err(error) => panel.section("TOKENS").line(
            Text::of(glyphs.warn, palette.warn)
                .space()
                .push(format!("unknown: {error}"), palette.warn),
        ),
        Ok(held) if held.is_empty() => panel,
        Ok(held) => panel.section("TOKENS").table(token_table(ui, held, &names)),
    };

    if let Ok(stuck) = &immature_tokens {
        if !stuck.is_empty() {
            panel = panel
                .section("WITHHELD TOKENS")
                .table(token_table(ui, stuck, &names));
        }
    }

    // Not called "immature". Coinbase maturity is the usual cause, but the SDK
    // routes *any* output the node reports as unspendable into this bucket,
    // whatever its age or script — and an output with a million confirmations
    // labelled "immature" is a wrong answer printed confidently.
    if !wallet.funding.immature.is_empty() {
        panel = panel.note(Text::of(
            "withheld: the node reported these as not spendable. Usually coinbase maturity \
             — 100 confirmations — but not always",
            palette.muted,
        ));
    }

    // The value in these outputs is real, and it is not spendable by a key —
    // saying only the first half would overstate what a signer can move, and
    // saying only the second is how this once reported 0 for an address holding
    // 7.7 million.
    if conditions.identity_outputs > 0 {
        panel = panel.note(Text::of(
            "held by id: this VerusID's own funds. Spendable by its authority, not by a \
             transparent key, so the native builders leave them alone",
            palette.muted,
        ));
    }
    if conditions.other_outputs > 0 {
        panel = panel.note(Text::of(
            "in conditions: CryptoCondition outputs that are not plain identity payments. \
             Any token they carry is counted above, in its own currency",
            palette.muted,
        ));
    }

    ui.panel(&panel);
    Ok(())
}

pub fn utxos(
    ui: &Ui,
    settings: &Settings,
    address: Option<&str>,
    key: Option<&str>,
) -> miette::Result<()> {
    let address = resolve_address(settings, address, key)?;
    let (_node, wallet) = gather(settings, address)?;
    let tip = wallet.funding.tip;
    let glyphs = ui.theme.glyphs;
    let palette = ui.theme.palette;

    if ui.is_json() {
        let spendable: Vec<_> = wallet
            .funding
            .utxos
            .iter()
            .map(|utxo| {
                serde_json::json!({
                    "txid": utxo.txid.to_string(),
                    "vout": utxo.vout,
                    "satoshis": utxo.satoshis.to_sat(),
                    "status": "spendable",
                })
            })
            .collect();
        let mut outputs = spendable;
        outputs.extend(
            wallet
                .funding
                .immature
                .iter()
                .map(|found| found_json(found, tip, "withheld")),
        );
        outputs.extend(
            wallet
                .funding
                .other
                .iter()
                .map(|found| found_json(found, tip, "cryptocondition")),
        );
        emit_json(&serde_json::json!({
            "address": wallet.address,
            "tip": tip,
            "outputs": outputs,
        }));
        return Ok(());
    }

    let mut table = Table::new(vec![
        Column::left("outpoint"),
        Column::right("amount"),
        Column::right("conf"),
        Column::left("status"),
    ]);

    for utxo in &wallet.funding.utxos {
        table.push(vec![
            outpoint(ui, &utxo.txid.to_string(), utxo.vout),
            Text::of(fmt::amount(utxo.satoshis), palette.accent),
            // `spendable` has already filtered these against the tip, but the
            // per-output height is not carried through, so there is nothing
            // honest to print here.
            Text::of("—", palette.muted),
            Text::of(format!("{} spendable", glyphs.ok), palette.ok),
        ]);
    }
    for (found, label, style) in wallet
        .funding
        .immature
        .iter()
        .map(|found| (found, "not spendable", palette.warn))
        .chain(
            wallet
                .funding
                .other
                .iter()
                .map(|found| (found, "cryptocondition", palette.muted)),
        )
    {
        table.push(vec![
            outpoint(ui, &found.utxo.txid.to_string(), found.utxo.vout),
            Text::of(fmt::amount(found.utxo.satoshis), palette.value),
            Text::of(fmt::height(found.confirmations(tip).into()), palette.muted),
            Text::of(label, style),
        ]);
    }

    if wallet.funding.utxos.is_empty()
        && wallet.funding.immature.is_empty()
        && wallet.funding.other.is_empty()
    {
        ui.note(format!("{} holds no outputs", wallet.address));
        return Ok(());
    }

    ui.panel(
        &Panel::new("UTXOS")
            .row("address", Text::of(&wallet.address, palette.value))
            .row("tip", Text::of(fmt::height(tip.into()), palette.accent))
            .rule()
            .table(table)
            .note(Text::of(
                "a CryptoCondition output's value is in its payload, not its satoshi field",
                palette.muted,
            )),
    );
    Ok(())
}

fn outpoint(ui: &Ui, txid: &str, vout: u32) -> Text {
    Text::of(
        fmt::hash(txid, ui.theme.glyphs.ellipsis),
        ui.theme.palette.value,
    )
    .push(format!(":{vout}"), ui.theme.palette.muted)
}

fn token_table(
    ui: &Ui,
    held: &TokenBalances,
    names: &std::collections::BTreeMap<CurrencyId, String>,
) -> Table {
    let palette = ui.theme.palette;
    let glyphs = ui.theme.glyphs;
    let mut table = Table::headerless([Align::Right, Align::Left, Align::Left]);
    for (currency, amount) in held {
        let id = Address::new(AddressKind::Identity, currency.to_bytes()).to_string();
        // The name comes from the node and is untrusted display text; the id is
        // the part that identifies the currency, so both are always shown.
        let name = match names.get(currency) {
            Some(name) => Text::of(
                format!("{}@", fmt::untrusted(name, NAME_BUDGET, glyphs.ellipsis)),
                palette.accent,
            ),
            None => Text::of("(unnamed)", palette.muted),
        };
        table.push(vec![
            Text::of(fmt::amount(*amount), palette.value),
            name,
            Text::of(fmt::address(&id, glyphs.ellipsis), palette.muted),
        ]);
    }
    table
}

fn tokens_json(
    balances: &Result<TokenBalances, verus_sdk::network::FlowError>,
    names: &std::collections::BTreeMap<CurrencyId, String>,
) -> serde_json::Value {
    match balances {
        // An error here means "unknown", never zero, and the JSON has to say so
        // rather than serialising an empty list.
        Err(error) => serde_json::json!({ "known": false, "error": error.to_string() }),
        Ok(held) => serde_json::json!({
            "known": true,
            "balances": held.iter().map(|(currency, amount)| {
                serde_json::json!({
                    "currency": Address::new(AddressKind::Identity, currency.to_bytes()).to_string(),
                    "name": names.get(currency),
                    "satoshis": amount.to_sat(),
                })
            }).collect::<Vec<_>>(),
        }),
    }
}

fn found_json(
    found: &verus_sdk::network::AddressUtxo,
    tip: u32,
    status: &str,
) -> serde_json::Value {
    serde_json::json!({
        "txid": found.utxo.txid.to_string(),
        "vout": found.utxo.vout,
        "satoshis": found.utxo.satoshis.to_sat(),
        "height": found.height,
        "confirmations": found.confirmations(tip),
        "status": status,
    })
}

fn emit_json(value: &serde_json::Value) {
    println!(
        "{}",
        serde_json::to_string_pretty(value).expect("the report is plain data")
    );
}

#[cfg(test)]
mod tests {
    use verus_sdk::money::{Txid, Utxo};

    use super::*;

    fn utxo(script: Vec<u8>, satoshis: u64) -> AddressUtxo {
        AddressUtxo {
            utxo: Utxo {
                txid: Txid::from_internal([0u8; 32]),
                vout: 0,
                satoshis: Amount::from_sat(satoshis),
                script_pubkey: script,
            },
            address: "iJhCezBExJHvtyH3fGhNnt2NhU4Ztkf2yq".into(),
            height: 1,
            is_spendable: false,
        }
    }

    /// A real pay-to-identity script, produced by the daemon and taken from the
    /// SDK's own fixtures.
    fn identity_payment() -> Vec<u8> {
        let hex = include_str!("../../fixtures/script-identity-payment.hex");
        hex::decode(hex.trim()).expect("a valid fixture")
    }

    #[test]
    fn native_value_held_for_an_identity_is_counted_not_just_tallied() {
        // The regression this exists for: `wallet balance` used to print a
        // *count* of these outputs and none of their value, so a VerusID holding
        // 7.7 million reported zero.
        let found = classify(&[
            utxo(identity_payment(), 100_000_000),
            utxo(identity_payment(), 25_000_000),
        ]);
        assert_eq!(found.identity_outputs, 2);
        assert_eq!(found.identity.to_sat(), 125_000_000);
        assert_eq!(found.other_outputs, 0);
        assert_eq!(found.total().to_sat(), 125_000_000);
    }

    #[test]
    fn anything_that_is_not_an_identity_payment_lands_in_the_other_bucket() {
        // An OP_RETURN: decodable as a script, not as an identity payment.
        let found = classify(&[utxo(vec![0x6a, 0x01, 0x00], 7)]);
        assert_eq!(found.identity_outputs, 0);
        assert_eq!(found.other_outputs, 1);
        assert_eq!(found.other.to_sat(), 7);
    }

    #[test]
    fn an_empty_bucket_totals_zero_rather_than_panicking() {
        assert_eq!(classify(&[]).total(), Amount::ZERO);
    }
}
