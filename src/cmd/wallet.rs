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
use verus_sdk::money::{Amount, Utxo};
use verus_sdk::network::{
    history, native_currency, spendable, AddressUtxo, ChainReader, FlowError, Funding,
    HistoryEntry, MempoolDelta, RpcError, SignedAmount, TokenBalances,
};
use verus_sdk::send::CurrencyId;
use verus_sdk::verus_keys::{Address, AddressKind};

use crate::config::Settings;
use crate::currency_name::{look_up_names, name_budget, name_json, name_result, CurrencyName};
use crate::keystore::Keystore;
use crate::node::{self, Node};
use crate::ui::{fmt, Align, Column, Panel, Table, Text, Ui};

/// How much of a node-supplied currency name is ever printed.
const NAME_BUDGET: usize = 24;

/// Whether a currency id is printed whole or shortened to fit.
///
/// Which one a table gets is not decided here and is not decided per row — see
/// [`fitted`], which builds the table both ways and measures.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum IdWidth {
    Full,
    Elided,
}

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

    #[error("`{address}` is not an address, and nothing on this chain is called that")]
    #[diagnostic(
        code(pecu::bad_address),
        help("transparent addresses start with R and identities with i — or give a VerusID name like `bob@`, which is looked up")
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
                 with --node. If it is reachable but merely slow, raise `timeout_secs` under \
                 this profile in config.toml."
                    .to_string()
            }
            FlowError::Key(_) => {
                "that address did not parse — `pecu key list` shows the stored ones".to_string()
            }
            // The node giving up, not answering oddly. Measured against
            // api.verustest.net: an address with a very large UTXO set returns
            // this intermittently, while the same query succeeds on a retry.
            // "try --node" would be poor advice for something that works when
            // asked again.
            FlowError::Rpc(RpcError::Node { code: -32603, .. }) => {
                "the node failed to build the reply. It does that on addresses with very large \
                 UTXO sets, and the same query often succeeds on a retry"
                    .to_string()
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
    ui: &Ui,
    node: &Node,
    settings: &Settings,
    address: Option<&str>,
    key: Option<&str>,
) -> Result<Target, miette::Report> {
    if let Some(given) = address {
        return resolve_name(ui, node, &settings.profile.node, given);
    }

    let store = Keystore::new(&settings.paths);
    if let Some(label) = key {
        return Ok(Target::plain(store.load(label)?.address));
    }

    let keys = store.list()?;
    match keys.len() {
        0 => Err(WalletError::NoAddress.into()),
        1 => Ok(Target::plain(keys[0].address.clone())),
        count => Err(WalletError::AmbiguousKey { count }.into()),
    }
}

/// What a read-only command was pointed at.
pub struct Target {
    pub address: String,
    /// The identity's fully qualified name, when the address came from one.
    ///
    /// Kept so the panel can show what a name resolved to. An i-address on its
    /// own does not tell you whether `bob@` meant the `bob` you had in mind.
    pub name: Option<String>,
}

impl Target {
    fn plain(address: String) -> Self {
        Self {
            address,
            name: None,
        }
    }
}

/// An address, or the i-address a VerusID name resolves to.
///
/// Names are accepted because `send --to` and `id show` accept them, and a
/// wallet where the same identity is nameable in one command and not the next
/// is a wallet that makes its user remember which is which.
///
/// Parse-don't-trust still applies to what comes back. A typo'd *address* would
/// otherwise be sent to the node and return an empty balance, which reads as
/// "no funds" — the one wrong answer a wallet must never give. Base58check
/// catches that offline; a name that resolves to nothing is caught by the node
/// saying so, not by printing zero.
fn resolve_name(ui: &Ui, node: &Node, url: &str, given: &str) -> Result<Target, miette::Report> {
    if given.parse::<Address>().is_ok() {
        return Ok(Target::plain(given.to_string()));
    }

    ui.sdk(format!("node.identity({given:?})"));
    let record = match node.identity(given) {
        Ok(record) => record,
        // Two client-side answers, both meaning "this does not name an
        // identity": `-5` is no such identity, `-8` is not a usable reference
        // at all — which is what a typo'd address gets. Every other failure
        // means the node did not answer, and reporting that as "nothing is
        // called that" would deny the existence of an identity nobody asked
        // about.
        Err(RpcError::Node { code: -5 | -8, .. }) => {
            return Err(WalletError::BadAddress {
                address: given.to_string(),
            }
            .into())
        }
        Err(other) => {
            return Err(node::NodeError::request("looking up the identity", url, other).into())
        }
    };
    ui.sdk_result(format!(
        "IdentityRecord {{ identity_address: {} }}",
        record.identity_address
    ));
    Ok(Target {
        address: record.identity_address,
        name: Some(record.fully_qualified_name),
    })
}

/// The address row, naming the identity when the address came from one.
///
/// `send` shows `bob@ (bob.VRSCTEST@)` for the same reason: an i-address alone
/// does not tell you whether the name you typed meant the identity you had in
/// mind, and the whole point of typing a name is not having to check.
fn address_row(ui: &Ui, target: &Target) -> Text {
    let palette = ui.theme.palette;
    let glyphs = ui.theme.glyphs;
    let row = Text::of(&target.address, palette.value);
    match &target.name {
        None => row,
        Some(name) => row.push(
            format!("  ({})", fmt::untrusted(name, NAME_BUDGET, glyphs.ellipsis)),
            palette.accent,
        ),
    }
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

fn gather(ui: &Ui, node: Node, address: String) -> Result<(Node, Wallet), miette::Report> {
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

/// Whether any row on screen will be missing a name because the lookup failed.
fn any_failed(names: &BTreeMap<CurrencyId, CurrencyName>) -> bool {
    names
        .values()
        .any(|name| matches!(name, CurrencyName::Failed(_)))
}

pub fn balance(
    ui: &Ui,
    settings: &Settings,
    address: Option<&str>,
    key: Option<&str>,
) -> miette::Result<()> {
    // Connecting opens no socket — the first request does — so this costs
    // nothing before the cheap local refusals inside `resolve_address`.
    let node = node::connect(&settings.profile)?;
    let target = resolve_address(ui, &node, settings, address, key)?;
    let (node, wallet) = gather(ui, node, target.address.clone())?;

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
    //
    // Bounded as a whole, like `tx explain`'s. A wallet's currency set is what
    // the address actually holds rather than what a counterparty chose, which
    // is a milder risk — but an address holding two hundred tokens is a real
    // thing, and against a node that hangs the wait is one timeout per token
    // either way. There is no reason for a balance to be less interruptible
    // than an explain.
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
        ui.sdk(format!(
            "node.currency_definition(…) for {}",
            fmt::plural(wanted.len(), "currency", "currencies")
        ));
        let named = look_up_names(&node, &wanted, name_budget(&settings.profile));
        ui.sdk_result(name_result(&named, &wanted));
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
        .row("address", address_row(ui, &target))
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
            let table = pending_table(ui, pending, &names, currency);

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

    // Said out loud, because `(name unknown)` is a row about this wallet's
    // reading of the node and not about the currency. Notes are printed
    // unwrapped, so this is one clause and stops there; the reason itself
    // varies per row and belongs in `--json`, which carries it.
    if any_failed(&names) {
        panel = panel.note(Text::of(
            "(name unknown): the name lookup failed — `pecu currency show <id>` reads it directly",
            palette.muted,
        ));
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
    let node = node::connect(&settings.profile)?;
    let target = resolve_address(ui, &node, settings, address, key)?;
    let (_node, wallet) = gather(ui, node, target.address.clone())?;
    let tip = wallet.funding.tip;

    // An unreadable mempool leaves this empty, which withholds the double-spend
    // warning below rather than denying there is one.
    let unknown = BTreeSet::new();
    let outputs = Outputs {
        spendable: &wallet.funding.utxos,
        withheld: &wallet.funding.immature,
        conditions: &wallet.funding.other,
        arriving: wallet
            .pending
            .as_ref()
            .map(|pending| pending.receiving.as_slice())
            .unwrap_or_default(),
        spent: wallet
            .pending
            .as_ref()
            .map_or(&unknown, |pending| &pending.spent),
        mempool_error: wallet.pending.as_ref().err(),
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
                    "spent_in_mempool": outputs.being_spent(&txid, utxo.vout),
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

    let Some(panel) = utxos_panel(ui, &target, tip, &outputs) else {
        ui.note(format!("{} holds no outputs", wallet.address));
        ui.explain_panel();
        return Ok(());
    };

    ui.panel(&panel);
    ui.explain_panel();
    Ok(())
}

/// The outputs at one address, split the way the panel prints them.
///
/// Gathered into a struct rather than passed as six arguments so that
/// [`utxos_panel`] can be one function: what the tests drive is then the panel
/// the command prints, and a note only a fixture attaches is not a note the
/// command can quietly lose.
struct Outputs<'a> {
    /// What `spendable` offers a builder. Bare `Utxo`s — no height on them.
    spendable: &'a [Utxo],
    /// Withheld by the maturity filter, usually an immature coinbase.
    withheld: &'a [AddressUtxo],
    /// Not plain P2PKH: reserve, identity, anything CryptoCondition.
    conditions: &'a [AddressUtxo],
    /// Unconfirmed and arriving. Not spendable and counted nowhere.
    arriving: &'a [PendingOutput],
    /// Confirmed outputs an unconfirmed transaction already spends.
    spent: &'a BTreeSet<(String, u32)>,
    /// Why the mempool is unknown, when it is. `None` means it was read.
    mempool_error: Option<&'a RpcError>,
}

impl Outputs<'_> {
    /// Nothing at all at this address — not even something unspendable.
    fn is_empty(&self) -> bool {
        self.spendable.is_empty()
            && self.withheld.is_empty()
            && self.conditions.is_empty()
            && self.arriving.is_empty()
    }

    /// Whether an unconfirmed transaction already claims this output.
    ///
    /// The chain still shows it unspent and `spendable` still offers it, so
    /// without this a second transaction funded from here would be a double
    /// spend the node rejects.
    fn being_spent(&self, txid: &str, vout: u32) -> bool {
        self.spent.contains(&(txid.to_string(), vout))
    }
}

/// The UTXOS panel, or `None` when there is nothing to put in one.
fn utxos_panel(ui: &Ui, target: &Target, tip: u32, outputs: &Outputs) -> Option<Panel> {
    if outputs.is_empty() {
        return None;
    }
    let glyphs = ui.theme.glyphs;
    let palette = ui.theme.palette;

    // The outpoint is the only column here that may be shortened. CONF and
    // STATUS are what the command is for, and an amount cut from the middle is
    // a different number rather than a shorter one.
    let mut table = Table::new(vec![
        Column::left("outpoint"),
        Column::right("amount"),
        Column::right("conf"),
        Column::left("status"),
    ])
    .elidable(0);

    for utxo in outputs.spendable {
        let txid = utxo.txid.to_string();
        let (label, style) = if outputs.being_spent(&txid, utxo.vout) {
            (format!("{} being spent", glyphs.warn), palette.warn)
        } else {
            (format!("{} spendable", glyphs.ok), palette.ok)
        };
        table.push(vec![
            outpoint(ui, &txid, utxo.vout),
            Text::of(fmt::amount(utxo.satoshis), palette.accent),
            // The height these were mined at is not carried through, so there
            // is nothing honest to print here. Said out loud in a note below,
            // because a dash on its own reads as "no confirmations".
            Text::of("—", palette.muted),
            Text::of(label, style),
        ]);
    }
    for (found, label, style) in outputs
        .withheld
        .iter()
        .map(|found| (found, "not spendable", palette.warn))
        .chain(
            outputs
                .conditions
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
    for output in outputs.arriving {
        table.push(vec![
            outpoint(ui, &output.txid, output.vout),
            Text::of(fmt::amount(output.satoshis), palette.value),
            Text::of("0", palette.muted),
            Text::of(format!("{} pending", glyphs.bullet), palette.accent),
        ]);
    }

    let mut panel = Panel::new("UTXOS")
        .row("address", address_row(ui, target))
        .row("tip", Text::of(fmt::height(tip.into()), palette.accent))
        .rule()
        .table(table)
        .note(Text::of(
            "a CryptoCondition output's value is in its payload, not its satoshi field",
            palette.muted,
        ));

    // The em dash the spendable rows print is an unknown, and an unknown that
    // does not say why reads as a confident nothing — here, as "my coins have
    // no confirmations".
    if let Some(note) = missing_conf_note(outputs.spendable) {
        panel = panel.note(Text::of(note, palette.muted));
    }
    if !outputs.arriving.is_empty() {
        panel = panel.note(Text::of(
            "pending: in this node's mempool and in no block. Not spendable, and not counted \
             by `wallet balance` either",
            palette.muted,
        ));
    }
    // Nothing above says these are unusable, because the chain still says they
    // are fine. Only the mempool disagrees.
    if outputs
        .spendable
        .iter()
        .any(|utxo| outputs.being_spent(&utxo.txid.to_string(), utxo.vout))
    {
        panel = panel.note(Text::of(
            "being spent: an unconfirmed transaction already claims this output. Still \
             unspent on chain, so coin selection will offer it — funding a second payment \
             from it builds a double spend",
            palette.warn,
        ));
    }
    // Silence here would read as "nothing pending", which is the answer this
    // read exists to avoid giving wrongly.
    if let Some(error) = outputs.mempool_error {
        panel = panel.note(Text::of(
            format!("pending outputs unknown: {error}"),
            palette.warn,
        ));
    }

    Some(panel)
}

/// Why the CONF cell on a spendable row is an em dash rather than a number.
///
/// The fear the dash raises is "my coins have no confirmations", so the note
/// answers that first: `spendable` is built from `getaddressutxos`, which
/// excludes the mempool, so every row here is confirmed. What is missing is the
/// count, not the confirmations — the withheld and CryptoCondition rows come
/// back as `AddressUtxo`, which keeps the block the output was mined in, while
/// the spendable ones come back as bare `Utxo`s, a type with no height on it at
/// all.
///
/// Only worth a line when such a row is on screen: a note about rows nobody can
/// see explains nothing.
fn missing_conf_note(spendable: &[Utxo]) -> Option<&'static str> {
    (!spendable.is_empty()).then_some(
        "— in conf: spendable means confirmed. The count is missing, not zero: the height \
         each of these was mined at does not reach this command",
    )
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
    let node = node::connect(&settings.profile)?;
    let target = resolve_address(
        ui,
        &node,
        settings,
        args.target.address.as_deref(),
        args.target.key.as_deref(),
    )?;
    let address = target.address.clone();

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
        ui.sdk(format!(
            "node.currency_definition(…) for {}",
            fmt::plural(wanted.len(), "currency", "currencies")
        ));
        let named = look_up_names(&node, &wanted, name_budget(&settings.profile));
        ui.sdk_result(name_result(&named, &wanted));
        named
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

    if entries.is_empty() {
        ui.note(format!("{address} has no transactions in that range"));
        ui.explain_panel();
        return Ok(());
    }

    let table = history_table(ui, shown, &names, &settings.profile.currency, now_seconds());

    let mut panel = Panel::new("HISTORY")
        .row("address", address_row(ui, &target))
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
    // A currency shown as a bare id here is the documented fallback, so on its
    // own it says nothing about why. Worth a line when the fallback was taken
    // because the lookup failed — a name silently missing is how this hid
    // before — but the note has to name *which* ids. The change column renders
    // a currency the node denies and a currency nobody could ask about
    // identically, so a note that only said "a currency shown as an id" would
    // be a false statement about the rows it did not mean.
    if let Some(failed) = failed_ids(ui, &names) {
        panel = panel.note(Text::of(
            format!("no name could be got for {failed} — `pecu currency show` reads it directly"),
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

/// The ids `change` fell back to because their lookup failed, elided and ready
/// to drop into a sentence — or `None` when no lookup failed.
///
/// Listed rather than counted because the change column holds every leg of a
/// transaction in one cell and cannot carry a second column saying why any one
/// of them is a bare id. Naming them is the only way a note about them is true
/// of the rows it means and silent about the rest. Capped at two, because a
/// panel note is printed on one unwrapped line.
fn failed_ids(ui: &Ui, names: &BTreeMap<CurrencyId, CurrencyName>) -> Option<String> {
    let failed: Vec<String> = names
        .iter()
        .filter(|(_, name)| matches!(name, CurrencyName::Failed(_)))
        .map(|(currency, _)| {
            let id = Address::new(AddressKind::Identity, currency.to_bytes()).to_string();
            fmt::address(&id, ui.theme.glyphs.ellipsis)
        })
        .collect();
    let (shown, rest) = failed.split_at(failed.len().min(2));
    match (shown, rest.len()) {
        ([], _) => None,
        (shown, 0) => Some(shown.join(", ")),
        (shown, more) => Some(format!("{} and {more} more", shown.join(", "))),
    }
}

/// The `wallet history` table.
///
/// Only the txid may be shortened. HEIGHT, WHEN and CHANGE are the answer the
/// command was asked for, and the change cell holds every leg of the
/// transaction in one string — cut from the middle it would drop an amount and
/// keep a currency, which reads like a smaller transfer rather than like a cut.
/// The txid is already elided to `10…6` on the way in; at a width that cannot
/// hold even that, the column gives up more of it rather than the frame giving
/// up its right-hand border.
fn history_table(
    ui: &Ui,
    entries: &[HistoryEntry],
    names: &BTreeMap<CurrencyId, CurrencyName>,
    currency: &str,
    now: u64,
) -> Table {
    let palette = ui.theme.palette;
    let glyphs = ui.theme.glyphs;
    let mut table = Table::new(vec![
        Column::right("height"),
        Column::right("when"),
        Column::right("change"),
        Column::left("transaction"),
    ])
    .elidable(3);
    for entry in entries {
        // One row per leg. A conversion moves two currencies at once, and both
        // legs on one line made a cell wider than the widest frame the theme
        // can reach — 51 cells of `+867.66527599 mambo-basket@  +45717268.
        // 17860181 mambo@` against a ceiling of 78, with the other three
        // columns already spent. No amount of txid could pay for that, so every
        // row of the table broke out of the frame at *every* width, 120
        // included, on any address that had ever converted anything.
        //
        // Down rather than across, because there is nothing here to give up:
        // an amount cut from the middle is a different number, and a leg
        // dropped is a transfer the panel did not mention. Height, age and txid
        // belong to the transaction rather than to the leg, so they stay on its
        // first row and the continuation rows carry only the change.
        let mut legs = change_legs(ui, entry, names, currency).into_iter();
        if let Some(first) = legs.next() {
            table.push(vec![
                Text::of(fmt::height(entry.height.into()), palette.muted),
                Text::of(age(now, entry.block_time), palette.muted),
                first,
                Text::of(
                    fmt::hash(&entry.txid.to_string(), glyphs.ellipsis),
                    palette.value,
                ),
            ]);
        }
        for leg in legs {
            table.push(vec![Text::new(), Text::new(), leg]);
        }
    }
    table
}

/// The change column, one [`Text`] per leg: the native leg, then any token
/// legs.
///
/// Always at least one, so a transaction that moved nothing still gets a row.
fn change_legs(
    ui: &Ui,
    entry: &HistoryEntry,
    names: &BTreeMap<CurrencyId, CurrencyName>,
    native: &str,
) -> Vec<Text> {
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
    let mut legs = Vec::new();
    if entry.net_native != SignedAmount::ZERO || entry.net_currencies.is_empty() {
        legs.push(
            Text::of(fmt::signed(entry.net_native.to_sat()), style)
                .space()
                .push(native, palette.muted),
        );
    }

    for (currency, value) in &entry.net_currencies {
        // No name, for whatever reason, falls back to the node's own key. The
        // change column is one cell holding every leg of the transaction, so
        // there is no room for a second column saying why — the id is a true
        // answer in either case, and the panel's note names the ids whose
        // lookup failed rather than tarring every bare id with the same cause.
        let label = currency
            .parse::<Address>()
            .ok()
            .map(|parsed| CurrencyId::from_bytes(parsed.hash()))
            .and_then(|id| match names.get(&id) {
                Some(CurrencyName::Known(name)) => Some(name.clone()),
                _ => None,
            })
            .map(|name| format!("{}@", fmt::untrusted(&name, NAME_BUDGET, glyphs.ellipsis)))
            .unwrap_or_else(|| fmt::address(currency, glyphs.ellipsis));
        legs.push(
            Text::of(fmt::signed(value.to_sat()), style)
                .space()
                .push(label, palette.accent),
        );
    }
    legs
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
    names: &BTreeMap<CurrencyId, CurrencyName>,
    ids: IdWidth,
) -> [Text; 2] {
    let palette = ui.theme.palette;
    let glyphs = ui.theme.glyphs;
    let id = Address::new(AddressKind::Identity, currency.to_bytes()).to_string();
    let verdict = names.get(&currency);
    let name = match verdict {
        Some(CurrencyName::Known(name)) => Text::of(
            format!("{}@", fmt::untrusted(name, NAME_BUDGET, glyphs.ellipsis)),
            palette.accent,
        ),
        // Not `(unnamed)`. The node did not say this currency is nameless; it
        // said it has no such currency at all — and this row is printing a
        // balance in it, so "merely nameless" would have the panel contradict
        // itself. Say the thing that was actually answered.
        Some(CurrencyName::Absent) => Text::of("(no such currency)", palette.muted),
        // Not `(name unreadable)` either, which would name a cause. Most of the
        // ways this fails are not a garbled answer — a timeout is no answer at
        // all — and inventing one is the same overconfidence as the `(unnamed)`
        // this issue was filed about. `unknown` is exactly what is known.
        //
        // `None` renders here too, and truthfully: a currency nobody looked up
        // has no known name either, and this wording claims nothing about why.
        // In the warning colour, because a reader has to be able to tell this
        // apart from a currency the node answered about.
        Some(CurrencyName::Failed(_)) | None => Text::of("(name unknown)", palette.warn),
    };
    // With no name beside it the id is the only handle the reader has left, and
    // `iHBwQo7LU…dK9f` cannot be copied, pasted or looked up. A named row keeps
    // the short form: there the name is the handle and the id is a check.
    let id = match (verdict, ids) {
        (Some(CurrencyName::Known(_)), _) | (_, IdWidth::Elided) => {
            fmt::address(&id, glyphs.ellipsis)
        }
        (_, IdWidth::Full) => fmt::id(&id, glyphs.ellipsis),
    };
    [name, Text::of(id, palette.muted)]
}

/// Build a table with whole currency ids, or with elided ones if that is what
/// it takes to keep the frame square.
///
/// Whether there is room for a 34-character id cannot be decided one row at a
/// time, which is what a width constant here used to assume. [`Table`] sizes
/// each column to the widest cell in it, so a *sibling* row's long name pushes
/// the id column right; and the same two cells are laid out against four
/// columns under `PENDING` and three under `TOKENS`, which is another ten
/// characters. [`Panel`] pads a content line without cutting it and clamps only
/// its own width, so a line wider than the theme runs out through the
/// right-hand border and the box comes out ragged — measured at an ordinary
/// eighty columns, not some pathological width.
///
/// So the only honest test is to build the wide version and measure it. The
/// table is cheap and this runs once per panel section.
fn fitted(theme: &crate::ui::Theme, build: impl Fn(IdWidth) -> Table) -> Table {
    let wide = build(IdWidth::Full);
    if wide
        .lines(theme)
        .iter()
        .all(|line| line.width() <= theme.width)
    {
        wide
    } else {
        build(IdWidth::Elided)
    }
}

/// The `PENDING` table: native movements, then one row per token moving.
///
/// Four columns here against `TOKENS`' three, which is why the id cells cannot
/// decide their own width: see `fitted`.
///
/// Only the id column may be shortened, unlike `token_table`'s name-then-id,
/// and it stops at the short form. The third column is not the same kind of
/// cell on every row: a currency name on the token rows, which has an id beside
/// it to be looked up from, but the native ticker on the movement rows, where
/// the fourth column is empty and the ticker is the only thing saying what
/// currency the amount is in. Marking it would turn `VRSCTEST` into `V…TEST` on
/// a money row to buy characters for a name, so it is left alone — and a table
/// with nothing left to give comes out ragged rather than giving up the id.
///
/// A function rather than a block inside `balance` because the frame test needs
/// to render the table that ships. Built inline, the test built a copy, and a
/// copy stops being the thing under test the first time the two drift.
fn pending_table(
    ui: &Ui,
    pending: &Pending,
    names: &BTreeMap<CurrencyId, CurrencyName>,
    currency: &str,
) -> Table {
    let palette = ui.theme.palette;
    fitted(&ui.theme, |ids| {
        let mut table = Table::headerless([Align::Left, Align::Right, Align::Left, Align::Left])
            .elidable_to(3, fmt::address_width(ui.theme.glyphs.ellipsis));
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
        // Only when both directions are present. On its own, a net line repeats
        // the single figure above it.
        if pending.incoming != Amount::ZERO && pending.outgoing != Amount::ZERO {
            table.push(vec![
                Text::of("NET", palette.label),
                Text::of(fmt::signed(pending.net()), palette.accent),
                Text::of(currency, palette.muted),
                Text::new(),
            ]);
        }
        for (currency, value) in &pending.tokens {
            let [name, id] = currency_cells(ui, *currency, names, ids);
            table.push(vec![
                Text::new(),
                Text::of(fmt::signed(value.to_sat()), palette.value),
                name,
                id,
            ]);
        }
        table
    })
}

/// The name column pays before the id column, for the reason `key_table` puts
/// the label ahead of the address: the name is display text and the id is the
/// identifier.
///
/// A currency name here is whatever the node said it was, already bounded to
/// `NAME_BUDGET`, and a reader who has the id can look the name up again. The
/// id cannot be recovered from the name — least of all on the rows where there
/// is no name, which are exactly the rows `currency_cells` keeps a wide id for.
/// Queued id-first, a *sibling* row's long name used to drain this column past
/// that wide id and past the short form below it, down to `i…dK9f`: four
/// informative characters, since every i-address opens with an `i`.
fn token_table(ui: &Ui, held: &TokenBalances, names: &BTreeMap<CurrencyId, CurrencyName>) -> Table {
    let palette = ui.theme.palette;
    fitted(&ui.theme, |ids| {
        let mut table = Table::headerless([Align::Right, Align::Left, Align::Left])
            .elidable(1)
            .elidable_to(2, fmt::address_width(ui.theme.glyphs.ellipsis));
        for (currency, amount) in held {
            let [name, id] = currency_cells(ui, *currency, names, ids);
            table.push(vec![
                Text::of(fmt::amount(*amount), palette.value),
                name,
                id,
            ]);
        }
        table
    })
}

fn tokens_json(
    balances: &Result<TokenBalances, verus_sdk::network::FlowError>,
    names: &BTreeMap<CurrencyId, CurrencyName>,
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
                    "name": name_json(names.get(currency)),
                    "satoshis": amount.to_sat(),
                })
            }).collect::<Vec<_>>(),
        }),
    }
}

fn pending_json(
    pending: &Result<Pending, RpcError>,
    names: &BTreeMap<CurrencyId, CurrencyName>,
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
                    "name": name_json(names.get(currency)),
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
    crate::failure::document(value);
}

#[cfg(test)]
mod tests {
    use verus_sdk::money::{Txid, Utxo};

    use super::*;
    use crate::ui::text::strip_ansi;
    use crate::ui::theme::Skin;
    use crate::ui::Theme;

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

    /// The currency from issue #46, whose name a `getcurrency` reply this build
    /// could not read used to blank — and whose lookup is now tested where the
    /// looking up lives, in `crate::currency_name`.
    const KAIJU: &str = "iHBwQo7LUmb7QKKqbsd8Kw9BxdQvgTdK9f";

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

    fn plain_ui(terminal_width: usize) -> Ui {
        let mut ui = Ui::new(crate::cli::Theme::Plain, false, false);
        // Set rather than resolved, so the assertion does not depend on the
        // terminal the tests happen to run in.
        ui.theme = Theme::with_skin(Skin::Plain, terminal_width);
        ui
    }

    /// The framed skin, which is the one with a border to run out through.
    fn framed_ui(terminal_width: usize) -> Ui {
        let mut ui = Ui::new(crate::cli::Theme::Phosphor, false, false);
        ui.theme = Theme::with_skin(Skin::Phosphor, terminal_width);
        ui
    }

    fn cells(ui: &Ui, id: &str, names: &BTreeMap<CurrencyId, CurrencyName>) -> [String; 2] {
        let [name, address] = currency_cells(ui, currency(id), names, IdWidth::Full);
        [strip_ansi(&name.render()), strip_ansi(&address.render())]
    }

    /// Every framed line of a panel, by visible width.
    fn frame_widths(rendered: &str) -> Vec<usize> {
        rendered
            .lines()
            .map(strip_ansi)
            .filter(|line| line.starts_with(['\u{250c}', '\u{2502}', '\u{251c}', '\u{2514}']))
            .map(|line| unicode_width::UnicodeWidthStr::width(line.as_str()))
            .collect()
    }

    fn holding(rows: Vec<(&str, u64)>) -> TokenBalances {
        rows.into_iter()
            .map(|(id, satoshis)| (currency(id), Amount::from_sat(satoshis)))
            .collect()
    }

    fn verdicts(rows: Vec<(&str, CurrencyName)>) -> BTreeMap<CurrencyId, CurrencyName> {
        rows.into_iter()
            .map(|(id, verdict)| (currency(id), verdict))
            .collect()
    }

    /// `name_result` for a run that reached every currency it asked about,
    /// which is what these verdict maps stand for.
    fn result_of(names: &BTreeMap<CurrencyId, CurrencyName>) -> String {
        name_result(names, &names.keys().copied().collect())
    }

    #[test]
    fn a_name_that_could_not_be_read_never_prints_as_unnamed() {
        let ui = plain_ui(80);
        let names = verdicts(vec![
            (KAIJU, CurrencyName::Failed("idimportfees: 1e-8".into())),
            (TOKEN, CurrencyName::Absent),
        ]);
        // Neither of these says "unnamed", and neither names a cause it does
        // not know: one lookup failed, and the other came back with a node
        // saying it holds no such currency at all.
        assert_eq!(cells(&ui, KAIJU, &names)[0], "(name unknown)");
        assert_eq!(cells(&ui, TOKEN, &names)[0], "(no such currency)");
    }

    #[test]
    fn a_row_with_no_name_shows_the_whole_id_when_the_frame_has_room() {
        let names = verdicts(vec![
            (KAIJU, CurrencyName::Failed("idimportfees: 1e-8".into())),
            (TOKEN, CurrencyName::Known("Kaiju".into())),
        ]);
        let ui = framed_ui(80);
        let held = holding(vec![(KAIJU, 100_000_000), (TOKEN, 100_000_000)]);
        let rows: Vec<String> = token_table(&ui, &held, &names)
            .lines(&ui.theme)
            .iter()
            .map(|line| strip_ansi(&line.render()))
            .collect();
        // The thirty-four characters that are the only handle this row has
        // left, whole rather than elided: `iHBwQo7LU…dK9f` cannot be copied.
        assert!(
            rows.iter().any(|row| row.contains(KAIJU)),
            "the id should be whole: {rows:?}"
        );
        // A named row keeps the short form. The name is the handle there.
        assert!(
            rows.iter().any(|row| row.contains("iK2k8YH1j\u{2026}bMqg")),
            "a named row should keep the elided id: {rows:?}"
        );
    }

    #[test]
    fn widening_an_id_never_pushes_a_row_out_through_the_frame() {
        // The regression this exists for, at the width most people run. The id
        // was widened on a per-row guess at the columns beside it — a constant
        // measured against one three-column row with a seventeen-character name
        // — and both assumptions are wrong in the panel it is drawn in. Here a
        // *sibling* row's name widens the shared name column, and the frame
        // came out one column ragged.
        let ui = framed_ui(80);
        let names = verdicts(vec![
            (KAIJU, CurrencyName::Failed("idimportfees: 1e-8".into())),
            (
                TOKEN,
                CurrencyName::Known("a-name-of-twentyfour-chr".into()),
            ),
        ]);
        let held = holding(vec![(KAIJU, 1_000_000_000_000), (TOKEN, 100_000_000)]);
        let panel = Panel::new("WALLET")
            .section("TOKENS")
            .table(token_table(&ui, &held, &names));
        let widths = frame_widths(&panel.render(&ui.theme));
        assert!(!widths.is_empty(), "nothing was framed");
        assert!(
            widths.windows(2).all(|pair| pair[0] == pair[1]),
            "ragged frame, widths {widths:?}:\n{}",
            panel.render(&ui.theme)
        );
        // And it stayed square by shortening the id rather than by dropping the
        // row: whichever way `fitted` went, the id is still on screen.
        assert!(panel.render(&ui.theme).contains("iHBwQo7LU"));
    }

    #[test]
    fn a_narrow_frame_shortens_the_id_rather_than_running_past_the_border() {
        // Same property from the other end. Sixty columns leaves no room for
        // thirty-four characters of id, and the honest fit is the elided form.
        for terminal in [40, 48, 52, 60, 70, 78, 80, 120] {
            let ui = framed_ui(terminal);
            let names = verdicts(vec![(KAIJU, CurrencyName::Failed("boom".into()))]);
            let held = holding(vec![(KAIJU, 927_249_511_041)]);
            let panel = Panel::new("WALLET")
                .section("TOKENS")
                .table(token_table(&ui, &held, &names));
            let widths = frame_widths(&panel.render(&ui.theme));
            assert!(
                widths.windows(2).all(|pair| pair[0] == pair[1]),
                "ragged frame at {terminal} columns, widths {widths:?}:\n{}",
                panel.render(&ui.theme)
            );
        }
    }

    #[test]
    fn json_says_a_name_is_unknown_rather_than_absent_when_the_lookup_failed() {
        let names = verdicts(vec![
            (KAIJU, CurrencyName::Failed("idimportfees: 1e-8".into())),
            (TOKEN, CurrencyName::Absent),
        ]);

        let failed = name_json(names.get(&currency(KAIJU)));
        assert_eq!(failed["known"], serde_json::json!(false));
        assert!(
            failed.get("name").is_none(),
            "a lookup that failed must not answer the question: {failed}"
        );

        // The other direction: a node that said "no such currency" answered, so
        // that row keeps the affirmative shape it always had.
        let absent = name_json(names.get(&currency(TOKEN)));
        assert_eq!(absent["known"], serde_json::json!(true));
        assert_eq!(absent["name"], serde_json::Value::Null);
    }

    #[test]
    fn explain_says_a_lookup_failed_rather_than_reporting_no_names() {
        // `0 names` on its own reads as "the chain has no name for these",
        // which is a confident answer to a question that was never answered.
        assert_eq!(
            result_of(&verdicts(vec![(
                KAIJU,
                CurrencyName::Failed("idimportfees: 1e-8".into())
            )])),
            "0 names, 1 the lookup failed for"
        );
        assert_eq!(
            result_of(&verdicts(vec![(KAIJU, CurrencyName::Absent)])),
            "0 names, 1 the node has no currency for"
        );
        assert_eq!(
            result_of(&verdicts(vec![(
                KAIJU,
                CurrencyName::Known("Kaiju".into())
            )])),
            "1 name"
        );
    }

    #[test]
    fn the_history_note_names_the_ids_it_means_and_stays_silent_about_the_rest() {
        // The change column renders a currency the node denies and a currency
        // nobody could ask about identically, as a bare id. A note saying "a
        // currency shown as an id" would therefore be a false statement about
        // the row it did not mean — the node answered that one perfectly well.
        let ui = plain_ui(80);
        let names = verdicts(vec![
            (KAIJU, CurrencyName::Absent),
            (TOKEN, CurrencyName::Failed("timed out".into())),
        ]);
        let note = failed_ids(&ui, &names).expect("one lookup failed");
        assert!(note.contains("iK2k8YH1j"), "{note}");
        assert!(
            !note.contains("iHBwQo7LU"),
            "the denied currency is not a failed lookup: {note}"
        );
        // And nothing at all to say when nothing failed.
        assert!(failed_ids(&ui, &verdicts(vec![(KAIJU, CurrencyName::Absent)])).is_none());
    }

    #[test]
    fn a_currency_the_node_denies_is_not_reported_as_merely_nameless() {
        // `"name": null` on its own reads as "this currency has no name". The
        // node said something narrower and more surprising: it has no such
        // currency, while this wallet is holding a balance in it.
        let names = verdicts(vec![(KAIJU, CurrencyName::Absent)]);
        let absent = name_json(names.get(&currency(KAIJU)));
        assert_eq!(absent["known"], serde_json::json!(true));
        assert_eq!(absent["name"], serde_json::Value::Null);
        assert!(
            absent["reason"]
                .as_str()
                .unwrap_or_default()
                .contains("no currency"),
            "{absent}"
        );
    }

    /// A history entry, with whatever legs the caller wants on it.
    fn entry(byte: u8, native: i64, legs: Vec<(&str, i64)>) -> HistoryEntry {
        HistoryEntry {
            txid: txid(byte),
            height: 1_202_941,
            block_index: 0,
            block_time: 1_700_000_000,
            net_native: SignedAmount::from_sat(native),
            net_currencies: legs
                .into_iter()
                .map(|(id, value)| (id.to_string(), SignedAmount::from_sat(value)))
                .collect(),
            spent_something: native < 0,
        }
    }

    /// Every width the theme can actually produce. `Theme::with_skin` takes the
    /// terminal's columns, spends four on the frame and its margin, then clamps
    /// to 48..=78 — so a terminal of 52 is already at the floor and one of 82 is
    /// already at the ceiling. Going wider on either side proves nothing new,
    /// and going narrower proves that the floor is a floor.
    fn reachable() -> std::ops::RangeInclusive<usize> {
        40..=120
    }

    /// The three legs the history tests share, and the numbers that make them
    /// mean something. A column is as wide as the widest cell *across* rows, so
    /// the width these come to is a property of the fixture and not of the
    /// renderer — pinned here so that changing the fixture moves the numbers
    /// loudly rather than quietly.
    fn history_fixture() -> (Vec<HistoryEntry>, BTreeMap<CurrencyId, CurrencyName>) {
        (
            vec![
                entry(0x9f, -150_000_000, vec![]),
                entry(0x41, 0, vec![(KAIJU, 927_249_511_041)]),
                entry(0x1c, 25_000_000_000, vec![]),
            ],
            verdicts(vec![(KAIJU, CurrencyName::Known("Kaiju".into()))]),
        )
    }

    fn history_at(terminal: usize) -> (Ui, String) {
        let (entries, names) = history_fixture();
        let ui = framed_ui(terminal);
        let table = history_table(&ui, &entries, &names, "VRSCTEST", 1_700_086_400);
        let rendered = Panel::new("HISTORY").table(table).render(&ui.theme);
        (ui, rendered)
    }

    #[test]
    fn a_history_frame_the_txid_column_can_pay_for_comes_out_square() {
        // Sixty-four cells of table. HEIGHT, WHEN, CHANGE and the gutters are
        // forty-seven of them and none of those may be touched; the TRANSACTION
        // column holds the other seventeen and stops at its own header, so it
        // can find six. From a fifty-eight-cell budget upwards — a terminal of
        // sixty-two — that is enough, and the box closes. Before this change
        // every one of these widths came out ragged.
        let (entries, names) = history_fixture();
        let wide = framed_ui(200);
        let natural = history_table(&wide, &entries, &names, "VRSCTEST", 1_700_086_400)
            .lines(&wide.theme)
            .iter()
            .map(Text::width)
            .max()
            .unwrap_or(0);
        assert_eq!(natural, 64, "the fixture moved; so have the widths below");

        for terminal in 62..=120 {
            let (_, rendered) = history_at(terminal);
            let widths = frame_widths(&rendered);
            assert!(
                widths.windows(2).all(|pair| pair[0] == pair[1]),
                "ragged frame at {terminal} columns, widths {widths:?}:\n{rendered}"
            );
            // Squared by shortening a hash, not by dropping a column.
            let visible = strip_ansi(&rendered);
            for column in ["HEIGHT", "WHEN", "CHANGE", "TRANSACTION"] {
                assert!(
                    visible.contains(column),
                    "{column} went missing:\n{rendered}"
                );
            }
            assert!(visible.contains("1,202,941"), "{rendered}");
            assert!(visible.contains("1d 00h ago"), "{rendered}");
        }
    }

    #[test]
    fn a_history_frame_too_narrow_to_pay_for_keeps_its_txid_rather_than_cutting_it_for_nothing() {
        // Below sixty-two columns the four cells that may not be touched have
        // already spent the whole budget, and this fix is explicit that WHEN is
        // not to be dropped to make room: removing data from a wallet table is
        // worse than a ragged frame. So the frame stays ragged — which is the
        // designed failure mode, not an oversight — and the txid comes back
        // whole, because cutting it would not have closed the box either.
        let whole = fmt::hash(&txid(0x9f).to_string(), "\u{2026}");
        for terminal in 40..=61 {
            let (_, rendered) = history_at(terminal);
            let visible = strip_ansi(&rendered);
            assert!(
                visible.contains(&whole),
                "the txid was cut for nothing at {terminal} columns:\n{rendered}"
            );
            assert!(visible.contains("1d 00h ago"), "{rendered}");
        }
    }

    #[test]
    fn a_history_frame_with_room_keeps_the_txid_it_was_given() {
        // `fmt::hash` shortens to `10…6` on the way in; nothing here shortens it
        // further while there is room for it.
        let ui = framed_ui(120);
        let entries = vec![entry(0x9f, -150_000_000, vec![])];
        let table = history_table(&ui, &entries, &BTreeMap::new(), "VRSCTEST", 1_700_086_400);
        let rendered = strip_ansi(&Panel::new("HISTORY").table(table).render(&ui.theme));
        let whole = fmt::hash(&txid(0x9f).to_string(), ui.theme.glyphs.ellipsis);
        assert!(rendered.contains(&whole), "{rendered}");
    }

    #[test]
    fn the_unspent_frame_stays_square_without_giving_up_conf_or_status() {
        // The outpoint is the only column here that may be shortened: CONF and
        // STATUS are what the command is for.
        for terminal in reachable() {
            let ui = framed_ui(terminal);
            let palette = ui.theme.palette;
            let mut table = Table::new(vec![
                Column::left("outpoint"),
                Column::right("amount"),
                Column::right("conf"),
                Column::left("status"),
            ])
            .elidable(0);
            table.push(vec![
                outpoint(&ui, &txid(0x9f).to_string(), 137),
                Text::of(
                    fmt::amount(Amount::from_sat(1_234_567_800_000_000)),
                    palette.accent,
                ),
                Text::of("1,204", palette.muted),
                Text::of(format!("{} spendable", ui.theme.glyphs.ok), palette.ok),
            ]);
            let panel = Panel::new("UTXOS").table(table);
            let rendered = panel.render(&ui.theme);
            let widths = frame_widths(&rendered);
            assert!(
                widths.windows(2).all(|pair| pair[0] == pair[1]),
                "ragged frame at {terminal} columns, widths {widths:?}:\n{rendered}"
            );
            let visible = strip_ansi(&rendered);
            assert!(visible.contains("CONF"), "{rendered}");
            assert!(visible.contains("1,204"), "{rendered}");
            assert!(visible.contains("spendable"), "{rendered}");
        }
    }

    /// A spendable output, which is all `spendable` hands back: a bare `Utxo`,
    /// with no height on it and so no confirmation count derivable from it.
    fn spendable_utxo() -> Utxo {
        Utxo {
            txid: Txid::from_internal([0x9f; 32]),
            vout: 0,
            satoshis: Amount::from_sat(25_000_000),
            script_pubkey: Vec::new(),
        }
    }

    /// The UTXOS panel the command prints, rendered.
    ///
    /// Deliberately the production [`utxos_panel`] rather than a copy of it. A
    /// fixture that reassembled the panel would attach the note itself, and so
    /// would keep passing with the note gone from the command.
    fn utxos_render(ui: &Ui, spendable: &[Utxo], withheld: &[AddressUtxo], tip: u32) -> String {
        let read = BTreeSet::new();
        let outputs = Outputs {
            spendable,
            withheld,
            conditions: &[],
            arriving: &[],
            spent: &read,
            mempool_error: None,
        };
        utxos_panel(ui, &Target::plain(ADDRESS.to_string()), tip, &outputs)
            .expect("a panel, since there are outputs to put in one")
            .render(&ui.theme)
    }

    #[test]
    fn an_empty_conf_cell_says_why_it_is_empty_rather_than_leaving_it_at_a_dash() {
        // A dash with no explanation reads as "this output has no
        // confirmations". The note has to say otherwise, and it has to do it
        // without costing the column the rows that do have a number.
        let withheld = utxo(Vec::new(), 100_000_000);
        for terminal in reachable() {
            let ui = framed_ui(terminal);
            let rendered = utxos_render(
                &ui,
                &[spendable_utxo()],
                std::slice::from_ref(&withheld),
                25_678,
            );
            let visible = strip_ansi(&rendered);
            assert!(
                visible.contains("— in conf: spendable means confirmed"),
                "no note at {terminal} columns:\n{rendered}"
            );
            assert!(
                visible.contains("The count is missing, not zero"),
                "the note leaves the dash readable as zero at {terminal} columns:\n{rendered}"
            );
            // The column survives, and so does the evidence in it: 25,678
            // confirmations on a withheld output is what the docs' claim that
            // WITHHELD is not always coinbase immaturity rests on.
            assert!(visible.contains("CONF"), "{rendered}");
            assert!(visible.contains("25,678"), "{rendered}");
            // Notes hang below the frame, so none of this may ragged it.
            let widths = frame_widths(&rendered);
            assert!(
                widths.windows(2).all(|pair| pair[0] == pair[1]),
                "ragged frame at {terminal} columns, widths {widths:?}:\n{rendered}"
            );
        }
    }

    #[test]
    fn no_spendable_row_means_no_note_about_a_cell_that_is_not_on_screen() {
        assert!(missing_conf_note(&[]).is_none());
        let withheld = utxo(Vec::new(), 100_000_000);
        let ui = framed_ui(80);
        let rendered = utxos_render(&ui, &[], std::slice::from_ref(&withheld), 25_678);
        let visible = strip_ansi(&rendered);
        assert!(
            !visible.contains("in conf"),
            "a note about rows nobody can see:\n{rendered}"
        );
        assert!(visible.contains("25,678"), "{rendered}");
    }

    #[test]
    fn a_token_table_stays_square_at_every_width_the_theme_can_reach() {
        // `fitted` picks between a whole id and a wallet-style short one; below
        // the width even the short one needs, the table shortens it further
        // rather than running out through the border.
        let names = verdicts(vec![
            (KAIJU, CurrencyName::Failed("idimportfees: 1e-8".into())),
            (
                TOKEN,
                CurrencyName::Known("a-name-of-twentyfour-chr".into()),
            ),
        ]);
        let held = holding(vec![(KAIJU, 927_249_511_041), (TOKEN, 100_000_000)]);
        for terminal in reachable() {
            let ui = framed_ui(terminal);
            let panel = Panel::new("WALLET")
                .section("TOKENS")
                .table(token_table(&ui, &held, &names));
            let rendered = panel.render(&ui.theme);
            let widths = frame_widths(&rendered);
            assert!(
                widths.windows(2).all(|pair| pair[0] == pair[1]),
                "ragged frame at {terminal} columns, widths {widths:?}:\n{rendered}"
            );
        }
    }

    /// A `Pending` carrying one native movement and one token leg.
    fn pending_with_token(id: &str, satoshis: i64) -> Pending {
        Pending {
            incoming: Amount::from_sat(150_000_000),
            outgoing: Amount::ZERO,
            transactions: 1,
            spent: BTreeSet::new(),
            receiving: Vec::new(),
            tokens: [(currency(id), SignedAmount::from_sat(satoshis))]
                .into_iter()
                .collect(),
        }
    }

    #[test]
    fn the_pending_table_stays_square_though_it_has_a_column_the_token_table_has_not() {
        // The other half of the same regression, and the half a per-row width
        // constant cannot see at all: these are the same two cells, but laid
        // out against four columns rather than three. The label column and its
        // gutter are ten more characters, and the frame came out three columns
        // ragged at every terminal width — eighty included.
        //
        // Through `pending_table` rather than a copy of it. Built inline this
        // test went on passing while the table that ships changed underneath.
        for terminal in [60, 70, 80, 100, 200] {
            let ui = framed_ui(terminal);
            let names = verdicts(vec![(
                KAIJU,
                CurrencyName::Failed("idimportfees: 1e-8".into()),
            )]);
            // The figure from the issue itself.
            let pending = pending_with_token(KAIJU, 927_249_511_041);
            let table = pending_table(&ui, &pending, &names, "VRSCTEST");
            let panel = Panel::new("WALLET").section("PENDING").table(table);
            let rendered = panel.render(&ui.theme);
            let widths = frame_widths(&rendered);
            assert!(
                widths.windows(2).all(|pair| pair[0] == pair[1]),
                "ragged frame at {terminal} columns, widths {widths:?}:\n{rendered}"
            );
        }
    }

    #[test]
    fn a_conversion_row_stays_inside_the_frame_and_keeps_both_its_legs() {
        // A currency conversion moves two currencies in one transaction. Both
        // legs on one line came to 51 cells, against the 78 the theme clamps
        // to and with three other columns already spent — so the table was 100
        // cells wide and *every* row broke out of the frame, at every width up
        // to and including 120, on any address that had ever converted
        // anything. The txid column could not have paid for it at any width.
        //
        // One row per leg instead. Nothing is elided, both amounts and both
        // currency names survive whole, and the frame squares from the
        // seventy-column split the issue was filed about upwards.
        let names = verdicts(vec![
            (KAIJU, CurrencyName::Known("mambo-basket".into())),
            (TOKEN, CurrencyName::Known("mambo".into())),
        ]);
        let entries = vec![entry(
            0x9f,
            0,
            vec![(KAIJU, 86_766_527_599), (TOKEN, 4_571_726_817_860_181)],
        )];
        for terminal in 70..=120 {
            let ui = framed_ui(terminal);
            let table = history_table(&ui, &entries, &names, "VRSCTEST", 1_700_100_000);
            let rendered = Panel::new("HISTORY").table(table).render(&ui.theme);
            let widths = frame_widths(&rendered);
            assert!(
                widths.windows(2).all(|pair| pair[0] == pair[1]),
                "ragged frame at {terminal} columns, widths {widths:?}:\n{rendered}"
            );
            let visible = strip_ansi(&rendered);
            for leg in ["+867.66527599 mambo-basket@", "+45717268.17860181 mambo@"] {
                assert!(
                    visible.contains(leg),
                    "leg `{leg}` went missing at {terminal} columns:\n{rendered}"
                );
            }
        }
    }

    #[test]
    fn a_transaction_with_one_leg_still_takes_exactly_one_row() {
        // The common case pays nothing for the case above: no conversion, no
        // continuation row, and the height and age stay on the line the change
        // is on.
        let names = verdicts(vec![]);
        let entries = vec![entry(0x9f, -75_010_000, vec![])];
        let ui = framed_ui(80);
        let table = history_table(&ui, &entries, &names, "VRSCTEST", 1_700_100_000);
        let body: Vec<String> = strip_ansi(&Panel::new("HISTORY").table(table).render(&ui.theme))
            .lines()
            .filter(|line| line.contains("1,202,941"))
            .map(str::to_string)
            .collect();
        assert_eq!(body.len(), 1, "{body:?}");
        assert!(body[0].contains("-0.75010000 VRSCTEST"), "{body:?}");
    }

    #[test]
    fn a_token_id_is_never_cut_below_the_form_that_can_still_be_looked_up() {
        // The `TOKENS` half of the same rule. Here there is a name column to
        // take from first, so the frame squares *and* the id survives: a
        // sibling row's long name must not come out of a nameless row's id.
        let whole = Address::new(AddressKind::Identity, currency(KAIJU).to_bytes()).to_string();
        let short = fmt::address(&whole, Theme::with_skin(Skin::Phosphor, 80).glyphs.ellipsis);
        for terminal in reachable() {
            let ui = framed_ui(terminal);
            let names = verdicts(vec![
                (KAIJU, CurrencyName::Failed("idimportfees: 1e-8".into())),
                (
                    TOKEN,
                    CurrencyName::Known("a-name-of-twentyfour-chr".into()),
                ),
            ]);
            let held = holding(vec![(KAIJU, 927_249_511_041), (TOKEN, 5_000_000_000)]);
            let panel = Panel::new("WALLET").table(token_table(&ui, &held, &names));
            let rendered = panel.render(&ui.theme);
            let visible = strip_ansi(&rendered);
            assert!(
                visible.contains(&whole) || visible.contains(&short),
                "the id was cut below `{short}` at {terminal} columns:\n{rendered}"
            );
            let widths = frame_widths(&rendered);
            assert!(
                widths.windows(2).all(|pair| pair[0] == pair[1]),
                "ragged frame at {terminal} columns, widths {widths:?}:\n{rendered}"
            );
        }
    }

    #[test]
    fn a_pending_id_is_never_cut_below_the_form_that_can_still_be_looked_up() {
        // `currency_cells` keeps a wide id on a row whose name the node would
        // not give up, because there the id is the only handle the reader has
        // left. The table may narrow it to the short form and no further: every
        // i-address opens with an `i`, so `i…dK9f` is four informative
        // characters, and four characters cannot be copied, pasted or looked
        // up. Under the floor the frame goes ragged instead — removing data
        // from a wallet table is worse than a cosmetic ragged frame.
        let whole = Address::new(AddressKind::Identity, currency(KAIJU).to_bytes()).to_string();
        let short = fmt::address(&whole, Theme::with_skin(Skin::Phosphor, 80).glyphs.ellipsis);
        for terminal in reachable() {
            let ui = framed_ui(terminal);
            let names = verdicts(vec![(
                KAIJU,
                CurrencyName::Failed("idimportfees: 1e-8".into()),
            )]);
            let pending = pending_with_token(KAIJU, 927_249_511_041);
            let table = pending_table(&ui, &pending, &names, "VRSCTEST");
            let rendered = strip_ansi(&Panel::new("WALLET").table(table).render(&ui.theme));
            // The whole id where the frame has room for it, the short form
            // where it has not, and never anything narrower than the two.
            assert!(
                rendered.contains(&whole) || rendered.contains(&short),
                "the id was cut below the short form `{short}` at {terminal} columns:\n{rendered}"
            );
        }
    }
}
