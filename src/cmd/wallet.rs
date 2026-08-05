//! `pecu wallet …` — what an address holds, and the outputs behind it.
//!
//! Read-only, and deliberately so: this takes an address, not a key. Watching a
//! balance is the one wallet job that needs no secret at all, and a command that
//! cannot unlock anything cannot spend anything either.
//!
//! The interesting part is that a Verus balance is several numbers, not one.
//! What is spendable now; what the node withholds; what is held in
//! CryptoCondition outputs, whose value is in the *payload* rather than the
//! satoshi field; and what is moving but not yet in a block. An address can
//! report zero satoshis while holding a fortune in tokens, which is why
//! `token_balances` is asked separately and why a failure there means "unknown"
//! rather than "none" — the same reason the mempool read is kept in a `Result`
//! instead of collapsing to an empty summary.

use std::collections::{BTreeMap, BTreeSet};

use miette::Diagnostic;
use thiserror::Error;
use verus_sdk::decode::{decode_output_script, OutputKind};
use verus_sdk::money::Amount;
use verus_sdk::network::{
    currency_names, history, native_currency, spendable, AddressUtxo, ChainReader, FlowError,
    Funding, HistoryEntry, MempoolDelta, RpcError, SignedAmount, TokenBalances,
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
    /// `Err` means *unknown*, never *nothing*. A node without an address index
    /// refuses this call, and swallowing that into an empty summary would print
    /// the confident "nothing pending" this field exists to stop.
    pending: Result<Pending, RpcError>,
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

/// Value moving at this address that no block contains yet.
///
/// A UTXO set and a delta list agree that an unconfirmed payment does not
/// exist, so without this an address that has just been paid reports its old
/// balance — the one answer a wallet must not give while money is demonstrably
/// on its way. `ChainReader::address_mempool` asks the question in one request.
///
/// None of it is settled, and the rendering says so. These rows describe *one
/// node's* mempool at one instant: the transaction may be mined, may be
/// evicted, may be replaced, and a second node may never have seen it. So it is
/// reported beside the confirmed figures and never folded into them.
#[derive(Debug, Default, PartialEq, Eq)]
struct Pending {
    /// Native satoshis arriving.
    incoming: Amount,
    /// Native satoshis leaving.
    outgoing: Amount,
    /// How many distinct unconfirmed transactions touch this address. A single
    /// payment usually produces several rows — its inputs and its change.
    transactions: usize,
    /// Confirmed outputs of ours that an unconfirmed transaction already
    /// spends. `spendable` still offers these, because it reads the chain and
    /// the chain has not changed; funding a second transaction from one would
    /// build a double spend that the node then rejects.
    ///
    /// Keyed by the txid's display hex rather than by `Txid`, which is `Hash`
    /// but not `Ord`. A set that cannot be ordered would give this command's
    /// `--json` a different output ordering per run, for the same chain state.
    spent: BTreeSet<(String, u32)>,
    /// The unconfirmed outputs behind [`Pending::incoming`], so `wallet utxos`
    /// can list them. Kept apart from the confirmed set: they are not spendable
    /// and `spendable` is right to leave them out.
    receiving: Vec<PendingOutput>,
    /// Net per-currency movement, native leg excluded. Signed: a token can be
    /// on its way out as easily as in.
    tokens: BTreeMap<CurrencyId, SignedAmount>,
}

/// One unconfirmed output arriving at the address.
#[derive(Debug, PartialEq, Eq, PartialOrd, Ord)]
struct PendingOutput {
    /// Display hex, for the same ordering reason as [`Pending::spent`].
    txid: String,
    vout: u32,
    satoshis: Amount,
}

impl Pending {
    fn is_empty(&self) -> bool {
        self.transactions == 0
    }

    /// Net native movement. Saturating rather than checked: both halves are
    /// sums of real outputs, so this cannot overflow against an honest node,
    /// and a display figure is not worth failing a balance over.
    fn net(&self) -> i64 {
        i64::try_from(self.incoming.to_sat())
            .unwrap_or(i64::MAX)
            .saturating_sub(i64::try_from(self.outgoing.to_sat()).unwrap_or(i64::MAX))
    }
}

/// Fold the mempool rows for one address into what a reader needs.
///
/// `native` names the chain's own currency so its leg can be dropped from the
/// token map: the daemon reports the native value twice, once in `satoshis` and
/// again under `currency_values`, and summing both double-counts it. When the
/// native currency is unknown the token map is left empty rather than filled
/// with a figure that might be the native amount wearing a currency id — an
/// invented token is worse than a missing one.
fn summarise(rows: &[MempoolDelta], native: Option<CurrencyId>) -> Pending {
    let mut found = Pending::default();
    let mut transactions = std::collections::HashSet::new();

    for row in rows {
        transactions.insert(row.txid);

        // `satoshis` is signed and `spending` says the same thing, but only the
        // sign is arithmetic. A token-only output moves zero native value while
        // still being a spend, so the flag is what decides the bucket.
        let magnitude = row.satoshis.magnitude();
        let bucket = if row.spending {
            &mut found.outgoing
        } else {
            &mut found.incoming
        };
        *bucket = bucket.checked_add(magnitude).unwrap_or(*bucket);

        if let Some((txid, vout)) = row.spends {
            found.spent.insert((txid.to_string(), vout));
        } else {
            // Inputs and outputs are numbered separately in these rows, so on a
            // receive `index` is the vout.
            found.receiving.push(PendingOutput {
                txid: row.txid.to_string(),
                vout: row.index,
                satoshis: magnitude,
            });
        }

        let Some(native) = native else { continue };
        for (currency, value) in &row.currency_values {
            let Ok(parsed) = currency.parse::<Address>() else {
                // A currency key that is not an i-address is not something to
                // guess at. Skipped rather than shown under a mangled name.
                continue;
            };
            let id = CurrencyId::from_bytes(parsed.hash());
            if id == native {
                continue;
            }
            let entry = found.tokens.entry(id).or_insert(SignedAmount::ZERO);
            *entry = entry.checked_add(*value).unwrap_or(*entry);
        }
    }

    // Dropped once the net is zero: a currency that arrived and left again in
    // the same mempool is not pending, and a row of `+0.00000000` beside a
    // token name reads as a payment that is not there.
    found.tokens.retain(|_, value| *value != SignedAmount::ZERO);
    // The node returns these in no documented order, and `--json` should not
    // change shape between two runs against the same mempool.
    found.receiving.sort();
    found.transactions = transactions.len();
    found
}

fn gather(ui: &Ui, settings: &Settings, address: String) -> Result<(Node, Wallet), miette::Report> {
    let node = node::connect(&settings.profile)?;

    ui.sdk(format!("verus_sdk::network::spendable(&node, {address:?})"));
    let funding = spendable(&node, &address)
        .map_err(|source| WalletError::flow("reading the outputs", &address, source))?;
    ui.sdk_result(format!(
        "Funding {{ tip: {}, utxos: {}, immature: {}, other: {} }}",
        funding.tip,
        funding.utxos.len(),
        funding.immature.len(),
        funding.other.len()
    ));

    // `None` is a legitimate answer meaning "I do not know it", and only affects
    // reserve deposits and transfers. Not worth failing a balance over.
    ui.sdk("verus_sdk::network::native_currency(&node)");
    let native = native_currency(&node).ok();
    ui.sdk_result(match native {
        Some(id) => Address::new(AddressKind::Identity, id.to_bytes()).to_string(),
        None => "unknown".to_string(),
    });

    // Deliberately not folded into the error above. The confirmed reads are the
    // command; this is one more request that can fail on its own, and a balance
    // that refuses to print because the mempool was unreadable is worse than
    // one that prints and says the mempool was unreadable.
    ui.sdk(format!("node.address_mempool(&[{address:?}])"));
    let pending = node
        .address_mempool(&[address.as_str()])
        .map(|rows| summarise(&rows, native));
    ui.sdk_result(match &pending {
        Ok(pending) => format!(
            "{} unconfirmed, net {} sat",
            fmt::plural(pending.transactions, "transaction", "transactions"),
            pending.net()
        ),
        Err(error) => format!("Err({error})"),
    });
    Ok((
        node,
        Wallet {
            address,
            funding,
            native,
            pending,
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
    let (node, wallet) = gather(ui, settings, address)?;

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

    // One request per currency, so asked once for every currency that will be
    // printed — including the pending ones, which are otherwise the only tokens
    // on screen with no name beside their id.
    let mut wanted: BTreeSet<CurrencyId> = BTreeSet::new();
    for held in [&spendable_tokens, &immature_tokens].into_iter().flatten() {
        wanted.extend(held.keys().copied());
    }
    if let Ok(pending) = &wallet.pending {
        wanted.extend(pending.tokens.keys().copied());
    }
    let names = if wanted.is_empty() {
        Default::default()
    } else {
        ui.sdk("verus_sdk::network::currency_names(&node, …)");
        let named = currency_names(&node, wanted).unwrap_or_default();
        ui.sdk_result(fmt::plural(named.len(), "name", "names"));
        named
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
            // Its own object, outside `total_satoshis`, because it is not part
            // of the total. A consumer that wants to add it has to say so.
            "pending": pending_json(&wallet.pending, &names),
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

    // Its own section rather than another row in the totals. Confirmed figures
    // and mempool figures do not belong in one column of numbers: they are
    // answers to different questions, and a reader who adds them up has been
    // invited to by the layout.
    panel = match &wallet.pending {
        Err(error) => panel.section("PENDING").line(
            Text::of(glyphs.warn, palette.warn)
                .space()
                .push(format!("unknown: {error}"), palette.warn),
        ),
        Ok(pending) if pending.is_empty() => panel,
        Ok(pending) => {
            let mut table =
                Table::headerless([Align::Left, Align::Right, Align::Left, Align::Left]);
            let mut movement = |label: &str, amount: Amount, style: anstyle::Style| {
                if amount == Amount::ZERO {
                    return;
                }
                table.push(vec![
                    Text::of(label, palette.label),
                    Text::of(fmt::amount(amount), style),
                    Text::of(currency, palette.muted),
                    Text::new(),
                ]);
            };
            movement("INCOMING", pending.incoming, palette.ok);
            movement("OUTGOING", pending.outgoing, palette.warn);
            // Only when both directions are present. On its own, a net line
            // repeats the single figure above it.
            if pending.incoming != Amount::ZERO && pending.outgoing != Amount::ZERO {
                table.push(vec![
                    Text::of("NET", palette.label),
                    Text::of(fmt::signed(pending.net()), palette.accent),
                    Text::of(currency, palette.muted),
                    Text::new(),
                ]);
            }
            for (currency, value) in &pending.tokens {
                let [name, id] = currency_cells(ui, *currency, &names);
                table.push(vec![
                    Text::new(),
                    Text::of(fmt::signed(value.to_sat()), palette.value),
                    name,
                    id,
                ]);
            }

            panel
                .section("PENDING")
                .row(
                    "in flight",
                    Text::of(
                        fmt::plural(pending.transactions, "transaction", "transactions"),
                        palette.value,
                    ),
                )
                .table(table)
        }
    };

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

    if wallet.pending.as_ref().is_ok_and(|p| !p.is_empty()) {
        panel = panel.note(Text::of(
            "pending: in this node's mempool, not in any block, and excluded from the totals \
             above. It may confirm, be replaced, or never arrive, and another node may not \
             have seen it at all",
            palette.muted,
        ));
    }

    ui.panel(&panel);
    ui.explain_panel();
    Ok(())
}

pub fn utxos(
    ui: &Ui,
    settings: &Settings,
    address: Option<&str>,
    key: Option<&str>,
) -> miette::Result<()> {
    let address = resolve_address(settings, address, key)?;
    let (_node, wallet) = gather(ui, settings, address)?;
    let tip = wallet.funding.tip;
    let glyphs = ui.theme.glyphs;
    let palette = ui.theme.palette;

    // An unconfirmed transaction can already be spending a confirmed output.
    // The chain still shows it unspent and `spendable` still offers it, so
    // without this a second transaction funded from here would be a double
    // spend the node rejects.
    let being_spent = |txid: &str, vout: u32| {
        wallet
            .pending
            .as_ref()
            .is_ok_and(|pending| pending.spent.contains(&(txid.to_string(), vout)))
    };

    if ui.is_json() {
        let spendable: Vec<_> = wallet
            .funding
            .utxos
            .iter()
            .map(|utxo| {
                let txid = utxo.txid.to_string();
                serde_json::json!({
                    "vout": utxo.vout,
                    "satoshis": utxo.satoshis.to_sat(),
                    "status": "spendable",
                    "spent_in_mempool": being_spent(&txid, utxo.vout),
                    "txid": txid,
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
        if let Ok(pending) = &wallet.pending {
            outputs.extend(pending.receiving.iter().map(|output| {
                serde_json::json!({
                    "txid": output.txid,
                    "vout": output.vout,
                    "satoshis": output.satoshis.to_sat(),
                    "status": "pending",
                })
            }));
        }
        emit_json(&serde_json::json!({
            "address": wallet.address,
            "tip": tip,
            "outputs": outputs,
            // Not derivable from the list above: a failed mempool read leaves
            // every `spent_in_mempool` reading `false`, which is the wrong
            // answer stated as a fact.
            "mempool_known": wallet.pending.is_ok(),
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
        let txid = utxo.txid.to_string();
        let (label, style) = if being_spent(&txid, utxo.vout) {
            (format!("{} being spent", glyphs.warn), palette.warn)
        } else {
            (format!("{} spendable", glyphs.ok), palette.ok)
        };
        table.push(vec![
            outpoint(ui, &txid, utxo.vout),
            Text::of(fmt::amount(utxo.satoshis), palette.accent),
            // `spendable` has already filtered these against the tip, but the
            // per-output height is not carried through, so there is nothing
            // honest to print here.
            Text::of("—", palette.muted),
            Text::of(label, style),
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

    let arriving = wallet
        .pending
        .as_ref()
        .map(|pending| pending.receiving.as_slice())
        .unwrap_or_default();
    for output in arriving {
        table.push(vec![
            outpoint(ui, &output.txid, output.vout),
            Text::of(fmt::amount(output.satoshis), palette.value),
            Text::of("0", palette.muted),
            Text::of(format!("{} pending", glyphs.bullet), palette.accent),
        ]);
    }

    if wallet.funding.utxos.is_empty()
        && wallet.funding.immature.is_empty()
        && wallet.funding.other.is_empty()
        && arriving.is_empty()
    {
        ui.note(format!("{} holds no outputs", wallet.address));
        ui.explain_panel();
        return Ok(());
    }

    let mut panel = Panel::new("UTXOS")
        .row("address", Text::of(&wallet.address, palette.value))
        .row("tip", Text::of(fmt::height(tip.into()), palette.accent))
        .rule()
        .table(table)
        .note(Text::of(
            "a CryptoCondition output's value is in its payload, not its satoshi field",
            palette.muted,
        ));

    if !arriving.is_empty() {
        panel = panel.note(Text::of(
            "pending: in this node's mempool and in no block. Not spendable, and not counted \
             by `wallet balance` either",
            palette.muted,
        ));
    }
    // Nothing above says these are unusable, because the chain still says they
    // are fine. Only the mempool disagrees.
    if wallet.funding.utxos.iter().any(|utxo| {
        let txid = utxo.txid.to_string();
        being_spent(&txid, utxo.vout)
    }) {
        panel = panel.note(Text::of(
            "being spent: an unconfirmed transaction already claims this output. Still \
             unspent on chain, so coin selection will offer it — funding a second payment \
             from it builds a double spend",
            palette.warn,
        ));
    }
    // Silence here would read as "nothing pending", which is the answer this
    // read exists to avoid giving wrongly.
    if let Err(error) = &wallet.pending {
        panel = panel.note(Text::of(
            format!("pending outputs unknown: {error}"),
            palette.warn,
        ));
    }

    ui.panel(&panel);
    ui.explain_panel();
    Ok(())
}

/// `pecu wallet history` — every transaction that touched an address.
///
/// Read-only and, unlike a balance, cumulative: this is what happened rather
/// than what is left. The SDK nets each transaction down to its effect on the
/// addresses asked about, which is the number a reader wants — a transaction
/// that spent a 10-coin output and took 9.9 back moved 0.1, not 10.
pub fn history_command(
    ui: &Ui,
    settings: &Settings,
    args: &crate::cli::HistoryArgs,
) -> miette::Result<()> {
    let address = resolve_address(
        settings,
        args.target.address.as_deref(),
        args.target.key.as_deref(),
    )?;
    let node = node::connect(&settings.profile)?;

    // `None` asks the node for the whole chain. That is the honest default for
    // a fresh address and a very large reply for an old one, which is what
    // `--from-height` is for; the error already names `max_response_mb`.
    //
    // An open-ended range is closed at the tip rather than at `u32::MAX`: the
    // daemon refuses that outright with `-1: JSON integer out of range`, which
    // reads as a broken node rather than as an argument it dislikes.
    let range = match (args.from_height, args.to_height) {
        (None, None) => None,
        (from, Some(to)) => Some((from.unwrap_or(0), to)),
        (Some(from), None) => {
            ui.sdk("node.block_count()");
            let tip = node.block_count().map_err(|source| {
                node::NodeError::request("reading the tip", &settings.profile.node, source)
            })?;
            ui.sdk_result(fmt::height(tip.into()));
            Some((from, tip))
        }
    };

    ui.sdk(format!(
        "verus_sdk::network::history(&node, [{address:?}], {range:?})"
    ));
    let entries = history(&node, &[address.as_str()], range)
        .map_err(|source| WalletError::flow("reading the history", &address, source))?;
    ui.sdk_result(format!("{} entries", entries.len()));

    // Newest last, because a terminal scrolls: the most recent thing should be
    // the thing still on screen. `--limit` therefore drops from the *front*.
    let shown: &[HistoryEntry] = if entries.len() > args.limit {
        &entries[entries.len() - args.limit..]
    } else {
        &entries
    };
    let dropped = entries.len() - shown.len();

    // One request, for every currency across every row that will be printed.
    let mut wanted: BTreeSet<CurrencyId> = BTreeSet::new();
    for entry in shown {
        for currency in entry.net_currencies.keys() {
            if let Ok(parsed) = currency.parse::<Address>() {
                wanted.insert(CurrencyId::from_bytes(parsed.hash()));
            }
        }
    }
    let names = if wanted.is_empty() {
        Default::default()
    } else {
        currency_names(&node, wanted).unwrap_or_default()
    };

    if ui.is_json() {
        emit_json(&serde_json::json!({
            "address": address,
            "entries": entries.iter().map(|entry| serde_json::json!({
                "txid": entry.txid.to_string(),
                "height": entry.height,
                "block_index": entry.block_index,
                "block_time": entry.block_time,
                "net_satoshis": entry.net_native.to_sat(),
                "spent_something": entry.spent_something,
                "outgoing": entry.is_outgoing(),
                "net_currencies": entry.net_currencies.iter().map(|(currency, value)| {
                    serde_json::json!({ "currency": currency, "satoshis": value.to_sat() })
                }).collect::<Vec<_>>(),
            })).collect::<Vec<_>>(),
            // The whole list is serialised; `--limit` only shortens the render.
            "count": entries.len(),
        }));
        return Ok(());
    }

    let palette = ui.theme.palette;
    let glyphs = ui.theme.glyphs;

    if entries.is_empty() {
        ui.note(format!("{address} has no transactions in that range"));
        ui.explain_panel();
        return Ok(());
    }

    let now = now_seconds();
    let mut table = Table::new(vec![
        Column::right("height"),
        Column::right("when"),
        Column::right("change"),
        Column::left("transaction"),
    ]);
    for entry in shown {
        table.push(vec![
            Text::of(fmt::height(entry.height.into()), palette.muted),
            Text::of(age(now, entry.block_time), palette.muted),
            change(ui, entry, &names, &settings.profile.currency),
            Text::of(
                fmt::hash(&entry.txid.to_string(), glyphs.ellipsis),
                palette.value,
            ),
        ]);
    }

    let mut panel = Panel::new("HISTORY")
        .row("address", Text::of(&address, palette.value))
        .row(
            "found",
            Text::of(
                fmt::plural(entries.len(), "transaction", "transactions"),
                palette.accent,
            ),
        )
        .rule()
        .table(table);

    if dropped > 0 {
        // Said out loud. A truncated list that does not admit it reads as the
        // whole history, and "my old payment is missing" is the wrong lesson.
        panel = panel.note(Text::of(
            format!(
                "{dropped} older {} not shown — raise --limit, or narrow with --from-height",
                if dropped == 1 { "entry" } else { "entries" }
            ),
            palette.warn,
        ));
    }
    if shown
        .iter()
        .any(|entry| entry.net_native == SignedAmount::ZERO && entry.spent_something)
    {
        panel = panel.note(Text::of(
            "a change of +0.00000000 that still spent something is a transfer to yourself: \
             the value came back, and only the fee left",
            palette.muted,
        ));
    }
    panel = panel.note(Text::of(
        "net effect per transaction, not gross: an output spent and mostly returned as \
         change counts as what actually moved",
        palette.muted,
    ));

    ui.panel(&panel);
    ui.explain_panel();
    Ok(())
}

/// The change column: the native leg, then any token legs.
fn change(
    ui: &Ui,
    entry: &HistoryEntry,
    names: &BTreeMap<CurrencyId, String>,
    native: &str,
) -> Text {
    let palette = ui.theme.palette;
    let glyphs = ui.theme.glyphs;
    let style = if entry.is_outgoing() {
        palette.warn
    } else {
        palette.ok
    };

    // Zero native is not "nothing happened" — a token-only transfer moves no
    // native value at all — so the native leg is dropped when there are tokens
    // to show instead of printing a misleading 0.
    let mut text = if entry.net_native != SignedAmount::ZERO || entry.net_currencies.is_empty() {
        Text::of(fmt::signed(entry.net_native.to_sat()), style)
            .space()
            .push(native, palette.muted)
    } else {
        Text::new()
    };

    for (currency, value) in &entry.net_currencies {
        if text.width() > 0 {
            text = text.push("  ", palette.muted);
        }
        let label = currency
            .parse::<Address>()
            .ok()
            .map(|parsed| CurrencyId::from_bytes(parsed.hash()))
            .and_then(|id| names.get(&id).cloned())
            .map(|name| format!("{}@", fmt::untrusted(&name, NAME_BUDGET, glyphs.ellipsis)))
            .unwrap_or_else(|| fmt::address(currency, glyphs.ellipsis));
        text = text
            .push(fmt::signed(value.to_sat()), style)
            .space()
            .push(label, palette.accent);
    }
    text
}

/// How long ago a block was mined, from its own timestamp.
///
/// Miner-chosen and only loosely monotonic, which is why it is shown as a rough
/// age rather than a clock time — and why a block a little in the future is
/// rendered as "just now" instead of a negative duration.
fn age(now: u64, block_time: i64) -> String {
    let then = u64::try_from(block_time).unwrap_or(0);
    format!("{} ago", fmt::duration(now.saturating_sub(then)))
}

fn now_seconds() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs())
        .unwrap_or(0)
}

fn outpoint(ui: &Ui, txid: &str, vout: u32) -> Text {
    Text::of(
        fmt::hash(txid, ui.theme.glyphs.ellipsis),
        ui.theme.palette.value,
    )
    .push(format!(":{vout}"), ui.theme.palette.muted)
}

/// The name and the id cells for a currency, in that order.
///
/// Two cells rather than one string so that a column of them aligns. The name
/// comes from the node and is untrusted display text; the i-address is the part
/// that actually identifies the currency, so both are always shown and the name
/// is never shown alone.
fn currency_cells(
    ui: &Ui,
    currency: CurrencyId,
    names: &BTreeMap<CurrencyId, String>,
) -> [Text; 2] {
    let palette = ui.theme.palette;
    let glyphs = ui.theme.glyphs;
    let id = Address::new(AddressKind::Identity, currency.to_bytes()).to_string();
    let name = match names.get(&currency) {
        Some(name) => Text::of(
            format!("{}@", fmt::untrusted(name, NAME_BUDGET, glyphs.ellipsis)),
            palette.accent,
        ),
        None => Text::of("(unnamed)", palette.muted),
    };
    [
        name,
        Text::of(fmt::address(&id, glyphs.ellipsis), palette.muted),
    ]
}

fn token_table(ui: &Ui, held: &TokenBalances, names: &BTreeMap<CurrencyId, String>) -> Table {
    let palette = ui.theme.palette;
    let mut table = Table::headerless([Align::Right, Align::Left, Align::Left]);
    for (currency, amount) in held {
        let [name, id] = currency_cells(ui, *currency, names);
        table.push(vec![
            Text::of(fmt::amount(*amount), palette.value),
            name,
            id,
        ]);
    }
    table
}

fn tokens_json(
    balances: &Result<TokenBalances, verus_sdk::network::FlowError>,
    names: &BTreeMap<CurrencyId, String>,
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

fn pending_json(
    pending: &Result<Pending, RpcError>,
    names: &BTreeMap<CurrencyId, String>,
) -> serde_json::Value {
    match pending {
        // Same shape as `tokens_json`, for the same reason: a failed read means
        // "unknown", and a consumer that saw zeroes here would report no pending
        // payment to someone who has one.
        Err(error) => serde_json::json!({ "known": false, "error": error.to_string() }),
        Ok(pending) => serde_json::json!({
            "known": true,
            "transactions": pending.transactions,
            "incoming_satoshis": pending.incoming.to_sat(),
            "outgoing_satoshis": pending.outgoing.to_sat(),
            "net_satoshis": pending.net(),
            // Confirmed outputs an unconfirmed transaction already spends. A
            // caller building its own coin selection needs these excluded;
            // `spendable` cannot know about them.
            "spending": pending.spent.iter().map(|(txid, vout)| {
                serde_json::json!({ "txid": txid, "vout": vout })
            }).collect::<Vec<_>>(),
            "tokens": pending.tokens.iter().map(|(currency, value)| {
                serde_json::json!({
                    "currency": Address::new(AddressKind::Identity, currency.to_bytes()).to_string(),
                    "name": names.get(currency),
                    "satoshis": value.to_sat(),
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

    const ADDRESS: &str = "RComfCn4wHHsGR8vWBAU7T1r3tHHyxN9Hm";
    /// VRSCTEST's own currency id, as the daemon reports it in `currencyvalues`
    /// and as `getcurrency VRSCTEST` gives it.
    const NATIVE: &str = "iJhCezBExJHvtyH3fGhNnt2NhU4Ztkf2yq";
    /// A real second currency on VRSCTEST, from `listcurrencies`. Real rather
    /// than invented because these strings are base58check and a made-up one
    /// fails its checksum, which would make the token path untestable.
    const TOKEN: &str = "iK2k8YH1jfR7RLmEZ3zac2Mkx5rxSgbMqg";

    fn currency(i_address: &str) -> CurrencyId {
        CurrencyId::from_bytes(i_address.parse::<Address>().expect("an i-address").hash())
    }

    fn txid(byte: u8) -> verus_sdk::money::Txid {
        verus_sdk::money::Txid::from_internal([byte; 32])
    }

    fn receive(tx: u8, index: u32, satoshis: i64) -> MempoolDelta {
        MempoolDelta {
            address: ADDRESS.into(),
            txid: txid(tx),
            index,
            satoshis: SignedAmount::from_sat(satoshis),
            currency_values: BTreeMap::new(),
            spending: false,
            spends: None,
            timestamp: 0,
        }
    }

    fn spend(tx: u8, index: u32, satoshis: i64, prevout: (u8, u32)) -> MempoolDelta {
        MempoolDelta {
            spending: true,
            spends: Some((txid(prevout.0), prevout.1)),
            satoshis: SignedAmount::from_sat(-satoshis),
            ..receive(tx, index, 0)
        }
    }

    #[test]
    fn a_payment_on_its_way_in_is_counted_as_incoming() {
        let pending = summarise(&[receive(1, 0, 10_000_000_000)], Some(currency(NATIVE)));
        assert_eq!(pending.incoming.to_sat(), 10_000_000_000);
        assert_eq!(pending.outgoing, Amount::ZERO);
        assert_eq!(pending.transactions, 1);
        assert_eq!(pending.net(), 10_000_000_000);
        assert_eq!(pending.receiving.len(), 1);
        assert_eq!(pending.receiving[0].vout, 0);
    }

    #[test]
    fn a_payment_leaving_reports_its_change_and_the_output_it_consumes() {
        // What a send of 0.1 looks like from the sender's side: the whole input
        // is spent and most of it comes back as change, in one transaction.
        let pending = summarise(
            &[
                spend(1, 0, 100_000_000, (9, 3)),
                receive(1, 1, 89_990_000), // change
            ],
            Some(currency(NATIVE)),
        );
        assert_eq!(pending.transactions, 1, "one transaction, not two rows");
        assert_eq!(pending.outgoing.to_sat(), 100_000_000);
        assert_eq!(pending.incoming.to_sat(), 89_990_000);
        assert_eq!(pending.net(), -10_010_000, "the payment plus the fee");
        // The output being consumed is still unspent on chain. Coin selection
        // would offer it again and build a double spend.
        assert!(pending.spent.contains(&(txid(9).to_string(), 3)));
    }

    #[test]
    fn a_receive_and_a_spend_sharing_an_index_are_both_kept() {
        // Inputs and outputs are numbered separately, so index 0 legitimately
        // appears twice in one transaction. Deduplicating on it would drop one.
        let pending = summarise(
            &[receive(1, 0, 500), spend(1, 0, 700, (9, 0))],
            Some(currency(NATIVE)),
        );
        assert_eq!(pending.incoming.to_sat(), 500);
        assert_eq!(pending.outgoing.to_sat(), 700);
        assert_eq!(pending.receiving.len(), 1);
    }

    #[test]
    fn the_native_leg_is_not_counted_twice_as_a_token() {
        // The daemon reports native value in `satoshis` *and* again under
        // `currencyvalues`. Summing both would show a phantom token payment
        // alongside every ordinary one.
        let mut row = receive(1, 0, 250_000_000);
        row.currency_values
            .insert(NATIVE.into(), SignedAmount::from_sat(250_000_000));
        row.currency_values
            .insert(TOKEN.into(), SignedAmount::from_sat(7));

        let pending = summarise(&[row], Some(currency(NATIVE)));
        assert_eq!(pending.incoming.to_sat(), 250_000_000);
        assert_eq!(pending.tokens.len(), 1, "{:?}", pending.tokens);
        assert_eq!(pending.tokens[&currency(TOKEN)].to_sat(), 7);
    }

    #[test]
    fn tokens_are_left_out_entirely_when_the_native_currency_is_unknown() {
        // Without knowing which id is native there is no way to drop its leg,
        // and a native amount printed under a currency name is an invented
        // token — worse than a missing one.
        let mut row = receive(1, 0, 250_000_000);
        row.currency_values
            .insert(NATIVE.into(), SignedAmount::from_sat(250_000_000));

        let pending = summarise(&[row], None);
        assert_eq!(pending.incoming.to_sat(), 250_000_000);
        assert!(pending.tokens.is_empty());
    }

    #[test]
    fn a_token_that_arrives_and_leaves_again_is_not_listed() {
        let mut arriving = receive(1, 0, 0);
        arriving
            .currency_values
            .insert(TOKEN.into(), SignedAmount::from_sat(500));
        let mut leaving = spend(2, 0, 0, (9, 1));
        leaving
            .currency_values
            .insert(TOKEN.into(), SignedAmount::from_sat(-500));

        let pending = summarise(&[arriving, leaving], Some(currency(NATIVE)));
        assert!(pending.tokens.is_empty(), "{:?}", pending.tokens);
        assert_eq!(pending.transactions, 2);
    }

    #[test]
    fn a_token_only_transfer_moves_no_native_value_but_is_still_pending() {
        let mut row = receive(1, 0, 0);
        row.currency_values
            .insert(TOKEN.into(), SignedAmount::from_sat(1_000));

        let pending = summarise(&[row], Some(currency(NATIVE)));
        assert!(!pending.is_empty(), "a token payment is a pending payment");
        assert_eq!(pending.incoming, Amount::ZERO);
        assert_eq!(pending.tokens[&currency(TOKEN)].to_sat(), 1_000);
    }

    #[test]
    fn an_empty_mempool_is_an_answer_and_renders_nothing() {
        let pending = summarise(&[], Some(currency(NATIVE)));
        assert!(pending.is_empty());
        assert_eq!(pending, Pending::default());
    }

    #[test]
    fn a_currency_key_that_is_not_an_address_is_skipped_rather_than_guessed_at() {
        let mut row = receive(1, 0, 100);
        row.currency_values
            .insert("not-an-address".into(), SignedAmount::from_sat(42));

        let pending = summarise(&[row], Some(currency(NATIVE)));
        assert_eq!(pending.incoming.to_sat(), 100);
        assert!(pending.tokens.is_empty());
    }
}
