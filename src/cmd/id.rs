//! `pecu id show` · `pecu id register` — the VerusID lifecycle.
//!
//! # Registration is two transactions, and the gap between them matters
//!
//! Step one commits to a name under a salt nobody else can see. Step two
//! reveals it and pays. Between them sits a confirmation, and a salt that
//! exists nowhere but in memory — lose it and the name is unclaimable and the
//! commitment fee is gone.
//!
//! So `id register` persists the [`Pending`] to `<config>/pending/<name>.json`
//! **before broadcasting anything**, and re-running the same command picks it
//! back up. The SDK makes the ordering hard to get wrong: `complete` exists only
//! on `Pending<ReadyToRegister>`, and the only way to hold one is a `poll` that
//! saw the commitment confirm. Running step two early is a compile error rather
//! than a spent commitment.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use miette::Diagnostic;
use thiserror::Error;
use verus_sdk::money::Amount;
use verus_sdk::network::{
    prepare_registration, AwaitingCommitment, ChainReader, CommitmentStatus, FlowError,
    IdentityAtAddress, IdentityRecord, Pending, RegistrationOptions, WaitPolicy,
};
use verus_sdk::send::CurrencyId;
use verus_sdk::verus_keys::{Address, AddressKind};
use verus_sdk::verus_tx::{Timelock, FLAG_LOCKED};

use crate::cli::{Globals, IdListTarget, IdRegisterArgs};
use crate::cmd::{uncertain_broadcast_advice, wallet};
use crate::config::Settings;
use crate::currency_name::{look_up_qualified_names, name_budget, name_result, CurrencyName};
use crate::keystore::{self, Envelope, Keystore};
use crate::node::{self, Node};
use crate::ui::{fmt, Column, Panel, Table, Text, Ui};

/// How much of a node-supplied name is ever printed. Identity names are
/// untrusted display text, the same as currency names.
const NAME_BUDGET: usize = 40;

#[derive(Debug, Error, Diagnostic)]
pub enum IdError {
    #[error("nothing on this chain is called `{name}`")]
    #[diagnostic(
        code(pecu::no_such_identity),
        help("VerusID names end with @, as in `bob@`")
    )]
    NotFound { name: String },

    #[error("`{value}` is not an address a key controls")]
    #[diagnostic(
        code(pecu::not_a_primary_address),
        help("--address takes a transparent R-address here. This asks the chain which identities list that address among the ones that SIGN for them, so a VerusID name or an i-address is not an input to the question — an identity is what comes back, not what goes in. `pecu key list` shows the addresses your keys hold, or name one with --key <label>. To read a single identity by name, `pecu id show <name@>`")
    )]
    NotAPrimaryAddress { value: String },

    #[error("the node would not look up identities for `{address}`")]
    #[diagnostic(
        code(pecu::address_refused),
        help("`getidentitieswithaddress` serves transparent R-addresses only, and this daemon did not read that one as one. Nothing was listed, and this is not an answer about how many identities the address controls. `pecu key list` shows the addresses your keys hold")
    )]
    AddressRefused { address: String },

    #[error("`{name}` is not a name this can register")]
    #[diagnostic(
        code(pecu::bad_name),
        help("give the name on its own — `alice`, or `alice@`. Sub-identities under a parent are not wired up yet")
    )]
    BadName { name: String },

    #[error("the `{profile}` profile is not allowed to spend")]
    #[diagnostic(
        code(pecu::spending_disabled),
        help("registering burns a real fee. Set `allow_spend = true` under [profiles.{profile}] in config.toml")
    )]
    SpendingDisabled { profile: String },

    #[error("no key to register with")]
    #[diagnostic(
        code(pecu::no_key),
        help("pass --from <label>, or make one with `pecu key gen`")
    )]
    NoKey,

    #[error("the keystore holds {count} keys, so there is no obvious one to pay with")]
    #[diagnostic(code(pecu::ambiguous_key), help("name one with --from <label>"))]
    AmbiguousKey { count: usize },

    #[error("`{name}` is already registered")]
    #[diagnostic(
        code(pecu::name_taken),
        help("names are first come, first served — pick another")
    )]
    Taken { name: String },

    #[error("`{value}` cannot be a primary address")]
    #[diagnostic(
        code(pecu::bad_primary),
        help("--primary takes transparent R-addresses: a registration writes its primary condition as bare key hashes, so a VerusID cannot hold one. `pecu key list` shows yours. Handing control to another identity is what the revocation and recovery authorities are for. Nothing was broadcast")
    )]
    BadPrimary { value: String },

    #[error("a {min_sigs}-of-{primaries} identity is one nobody could ever sign for")]
    #[diagnostic(
        code(pecu::bad_min_sigs),
        help("--min-sigs must be at least 1 and at most the number of --primary addresses — {primaries} here, which without --primary is the paying key alone. Pass more --primary addresses, or lower --min-sigs. Nothing was broadcast")
    )]
    BadMinSigs { min_sigs: u32, primaries: usize },

    #[error("cannot {action} {}", path.display())]
    #[diagnostic(
        code(pecu::pending_io),
        help("this file holds the salt for a registration in progress. Without it the name cannot be claimed and the commitment fee is lost")
    )]
    PendingIo {
        action: &'static str,
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("{} is not a registration this version understands", path.display())]
    #[diagnostic(code(pecu::pending_corrupt), help("{detail}"))]
    PendingCorrupt { path: PathBuf, detail: String },

    #[error("the saved reservation for `{name}` can never be completed: {detail}")]
    #[diagnostic(
        code(pecu::pending_unusable),
        help("a resumed registration takes its primary addresses and threshold from the file, written when the commitment was made — the flags on this run are not the ones in play, so correcting them changes nothing. Only the commitment's miner fee was ever spent: run the same command with --restart to discard the reservation and claim the name again. The file, if you want it, is at {}", path.display())
    )]
    PendingUnusable {
        name: String,
        path: PathBuf,
        detail: String,
    },

    #[error("the chain moved under this registration")]
    #[diagnostic(
        code(pecu::reorged),
        help("{detail}. Nothing is lost — run the same command again once the chain settles")
    )]
    Reorged { detail: String },

    #[error("the saved commitment for `{name}` can no longer be broadcast")]
    #[diagnostic(
        code(pecu::commitment_stale),
        help("its inputs have been spent by something else, so these bytes can never land — which happens when two spends are started before either confirms, because coin selection does not see the mempool and picks the same coins twice. Nothing was spent on this one: run the same command with --restart to discard it and claim the name again. The file, if you want it, is at {}", path.display())
    )]
    CommitmentStale { name: String, path: PathBuf },

    #[error("`{name}` was registered but is not on chain after {minutes} minutes")]
    #[diagnostic(
        code(pecu::not_mined_in_time),
        help("the registration was broadcast and nothing is lost — it is waiting to be mined. `pecu id show {name}@` will find it once it is, and the launch can be run again then")
    )]
    NotMinedInTime { name: String, minutes: u64 },

    #[error("`{name}@` does not exist, and a dry run cannot register it")]
    #[diagnostic(
        code(pecu::dry_run_cannot_register),
        help("--register would burn 100 VRSCTEST creating `{name}@` first, and --dry-run promises to spend nothing — which leaves no identity for the launch to be defined by and so nothing to preview: a currency's id *is* the defining identity's i-address. Register it with `pecu id register {name}`, then re-run this with --dry-run to see the launch")
    )]
    DryRunCannotRegister { name: String },

    #[error("`{name}@` is committed to but not registered yet")]
    #[diagnostic(
        code(pecu::registration_unfinished),
        help("registering is two transactions and only the first was broadcast, so there is no identity for this launch to define a currency on and nothing on its way to a block to wait for. Nothing is lost — the reservation is saved and the same command carries on from it once the commitment confirms, a block or so")
    )]
    RegistrationUnfinished { name: String },

    #[error("this version of pecu does not recognise the state of `{name}`'s commitment")]
    #[diagnostic(
        code(pecu::unknown_commitment_state),
        help("the SDK reported a state this build has no arm for, which means it is newer than this binary: `{detail}`. Nothing was broadcast and the saved reservation is intact — `pecu --version` names the SDK revision this was built against, and updating pecu should give it a name")
    )]
    UnknownCommitmentState { name: String, detail: String },

    #[error("the saved commitment for `{name}` has expired")]
    #[diagnostic(
        code(pecu::commitment_expired),
        help("a commitment carries the expiry height it was built at, and the chain has passed it. Re-broadcasting the same bytes can never work — they are refused before they reach the mempool. Nothing was spent: run the same command with --restart to discard the reservation and claim the name again")
    )]
    CommitmentExpired { name: String },

    #[error("cancelled")]
    #[diagnostic(code(pecu::cancelled), help("nothing was broadcast"))]
    Cancelled,

    #[error("--json will not register without --yes")]
    #[diagnostic(
        code(pecu::needs_yes),
        help("registering burns 100 VRSCTEST. --json is machine-readable output, not consent to spend it: the confirmation prompt would go to the same stream you are parsing, and there is nobody to answer it. Add --yes to go ahead, or --dry-run to see the cost and stop")
    )]
    NeedsYes,

    #[error("cannot ask for confirmation")]
    #[diagnostic(
        code(pecu::no_tty),
        help("there is no terminal to confirm on — pass --yes")
    )]
    CannotConfirm,

    #[error("{what} failed")]
    #[diagnostic(code(pecu::flow_failed), help("{advice}"))]
    Flow {
        what: &'static str,
        advice: String,
        #[source]
        source: Box<FlowError>,
    },
}

fn flow(what: &'static str, source: FlowError) -> IdError {
    let advice = match &source {
        // The node answered, or the connection broke — either way `pecu doctor`
        // blames a node that is not the problem, and the retry it invites is
        // how the 100 VRSCTEST gets paid twice.
        FlowError::BroadcastUncertain { txid, hex, .. } => uncertain_broadcast_advice(txid, hex),
        _ => "run `pecu doctor`, or point somewhere else with --node".to_string(),
    };
    IdError::Flow {
        what,
        advice,
        source: Box::new(source),
    }
}

// ── show ────────────────────────────────────────────────────────────────────

pub fn show(ui: &Ui, settings: &Settings, name: &str) -> miette::Result<()> {
    let node = node::connect(&settings.profile)?;
    ui.sdk(format!("node.identity({name:?})"));
    let record = match node.identity(name) {
        Ok(record) => record,
        // `-5` is no such identity and `-8` is not a usable reference at all;
        // both are the daemon answering. Anything else is it failing to, and
        // saying "nothing is called that" would be this program inventing an
        // answer it does not have.
        Err(verus_sdk::network::RpcError::Node { code: -5 | -8, .. }) => {
            return Err(IdError::NotFound {
                name: name.to_string(),
            }
            .into())
        }
        Err(other) => {
            return Err(node::NodeError::request(
                "reading the identity",
                &settings.profile.node,
                other,
            )
            .into())
        }
    };
    ui.sdk_result(format!(
        "IdentityRecord {{ {}, {} }}",
        record.identity_address, record.status
    ));

    // One extra request, and only for what is shown. A block height alone does
    // not say when — "block 1,177,254" means nothing without a clock — but the
    // command is already the slowest read here, so a failure to fetch it drops
    // the date rather than the answer.
    let mined = block_time(ui, &node, record.block_height);

    let timelock = timelock_of(&record.identity);
    // Only asked for when there is a lock to measure against it. Most
    // identities have none, and the common path stays at two requests.
    let tip = match timelock {
        Timelock::UntilBlock(_) => node.block_count().ok(),
        _ => None,
    };

    if ui.is_json() {
        emit(&serde_json::json!({
            "name": record.fully_qualified_name,
            "identity_address": record.identity_address,
            "status": record.status,
            "revoked": record.is_revoked(),
            "block_height": record.block_height,
            "block_time": mined,
            // The output currently holding the identity. This and
            // `block_height` describe the last change, not the registration.
            "outpoint": {
                "txid": record.outpoint.0.to_string(),
                "vout": record.outpoint.1,
            },
            // Named separately from the raw object because the raw object
            // spells it as two fields whose meaning depends on each other.
            "timelock": timelock_json(timelock),
            "identity": record.identity,
        }));
        ui.explain_panel();
        return Ok(());
    }

    ui.panel(&panel(ui, &record, mined, timelock, tip));
    ui.explain_panel();
    Ok(())
}

/// The IDENTITY panel, built away from the node so a test can render it.
///
/// `show` does the reading; everything here is a function of what came back.
/// Split out because four strings on this panel are the node's — the i-address,
/// the status, the primary addresses and the two authorities — and a display
/// filter nothing exercises is a filter that stops working.
fn panel(
    ui: &Ui,
    record: &IdentityRecord,
    mined: Option<i64>,
    timelock: Timelock,
    tip: Option<u32>,
) -> Panel {
    let palette = ui.theme.palette;
    let glyphs = ui.theme.glyphs;
    let identity = &record.identity;

    let status = if record.is_revoked() {
        Text::of(glyphs.danger, palette.danger)
            .space()
            .push("revoked", palette.danger)
    } else {
        Text::of(glyphs.ok, palette.ok)
            .space()
            // A word out of the node's JSON, not one this program chose.
            .push(
                fmt::untrusted(&record.status, NAME_BUDGET, glyphs.ellipsis),
                palette.value,
            )
    };

    let mut panel = Panel::new("IDENTITY")
        .row(
            "name",
            Text::of(
                fmt::untrusted(&record.fully_qualified_name, NAME_BUDGET, glyphs.ellipsis),
                palette.accent,
            ),
        )
        // An i-address, and the node is the only thing saying so. This is the
        // row a reader is told to compare against, so it gets the same filter
        // the name above it gets.
        .row(
            "i-address",
            Text::of(
                fmt::id(&record.identity_address, glyphs.ellipsis),
                palette.value,
            ),
        )
        .row("status", status)
        // Not "registered". `getidentity` reports the block and txid of the
        // output *currently* holding the identity, so for anything that has
        // ever been updated this is the last change, not the first. Adding the
        // date is what made that visible: an identity registered this morning
        // was claiming to have been registered an hour ago.
        .row(
            "last change",
            registered_row(ui, record.block_height, mined),
        )
        .row(
            "output",
            Text::of(
                fmt::hash(&record.outpoint.0.to_string(), ui.theme.glyphs.ellipsis),
                palette.muted,
            )
            .push(format!(":{}", record.outpoint.1), palette.muted),
        );

    // The raw identity object is whatever the daemon sent. Read the fields that
    // decide who controls this, and treat anything missing as unknown rather
    // than as a default.
    let min_sigs = identity.get("minimumsignatures").and_then(|v| v.as_u64());
    let primaries: Vec<String> = identity
        .get("primaryaddresses")
        .and_then(|v| v.as_array())
        .map(|list| {
            list.iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();

    panel = panel.section("CONTROL").row(
        "signatures",
        match (min_sigs, primaries.len()) {
            (Some(min), total) if total > 0 => {
                Text::of(format!("{min}-of-{total}"), palette.accent)
            }
            _ => Text::of("unknown", palette.muted),
        },
    );
    for address in &primaries {
        panel = panel.line(
            Text::of(glyphs.bullet, palette.muted)
                .space()
                // Straight out of `primaryaddresses`, which is arbitrary JSON.
                .push(fmt::id(address, glyphs.ellipsis), palette.value),
        );
    }
    let mut self_held = false;
    // Tracked separately from `self_held` because only this one decides whether
    // the identity can be revoked at all. An identity may be its own revocation
    // authority and still be revocable, provided somebody else can recover it.
    let mut self_recovery = false;
    for (label, field) in [
        ("revocation", "revocationauthority"),
        ("recovery", "recoveryauthority"),
    ] {
        if let Some(authority) = identity.get(field).and_then(|v| v.as_str()) {
            let mut row = Text::of(fmt::id(authority, glyphs.ellipsis), palette.value);
            // Compared raw, on purpose. `(itself)` is a fact about what the
            // node holds, not about what is printed, and two different
            // authorities that collapse to the same run of `·` would read as
            // one identity on the panel whose whole job is who controls this.
            if authority == record.identity_address {
                row = row.push("  (itself)", palette.warn);
                self_held = true;
                self_recovery |= field == "recoveryauthority";
            }
            panel = panel.row(label, row);
        }
    }

    let content = identity
        .get("contentmultimap")
        .and_then(|v| v.as_object())
        .map(|map| map.len())
        .unwrap_or(0)
        + identity
            .get("contentmap")
            .and_then(|v| v.as_object())
            .map(|map| map.len())
            .unwrap_or(0);
    if content > 0 {
        panel = panel.row(
            "published",
            Text::of(
                fmt::plural(content, "content key", "content keys"),
                palette.value,
            ),
        );
    }

    panel = timelock_panel(ui, panel, timelock, tip);

    if self_recovery {
        // The consensus rule, and the one fact about an identity that a reader
        // is most likely to be wrong about. It is the *recovery* authority that
        // decides this; self-revocation on its own is fine.
        panel = panel.note(Text::of(
            "this identity cannot be revoked: it is its own recovery authority, and consensus \
             refuses a revocation nobody could undo",
            palette.warn,
        ));
    }
    if self_held {
        // The authority is the identity, so it answers to the primary keys
        // above: there is no independent guardian. Those keys can still hand it
        // to another VerusID, and that hand-off is one-way.
        panel = panel.note(Text::of(
            "an authority pointing at the identity itself answers to the same primary keys \
             above; they can hand it to another VerusID, but cannot take it back afterwards",
            palette.muted,
        ));
    }

    panel
}

// ── list ────────────────────────────────────────────────────────────────────

/// What `id list` promises, and the sentence a reader has to have seen before
/// concluding an identity of theirs is gone.
///
/// `getidentitieswithaddress` matches on the identity's **primary** addresses
/// and on nothing else. Revocation and recovery authorities are i-addresses —
/// they name a VerusID, not a key — so the reply cannot answer for them, and
/// this list is silent about a role a reader would very reasonably expect it to
/// cover. Silence with no note attached reads as "you have no such identity".
const PRIMARY_ONLY: &str = "primary addresses only: these are the identities this address SIGNS \
                            for. One it can revoke or recover but is not primary on cannot appear \
                            — the reply does not carry those";

/// The other half of the promise: the answer is present tense.
///
/// The SDK sends `unspent: true`, which asks what the identity looks like now
/// rather than what it has ever looked like. That is the only form whose answer
/// is safe to act on — an outpoint from a superseded version produces a
/// transaction the chain rejects after it has been signed — but it does mean
/// this list drops an identity the moment its primary addresses change.
const PRESENT_TENSE: &str = "and as the chain stands now: one that listed this address in an \
                             older version, and no longer does, is not here either";

/// The identities one address is a primary of.
///
/// The one identity read that does not start from a name. Every other `id`
/// subcommand takes a name the caller already knows, which is exactly what
/// somebody restored from a seed on a new machine does not have — and on Verus
/// funds live under an identity rather than under a bare key, so a name they
/// cannot recover is money they cannot reach.
pub fn list(ui: &Ui, settings: &Settings, target: &IdListTarget) -> miette::Result<()> {
    // Refused offline, before anything is connected, and by name.
    //
    // `getidentitieswithaddress` serves transparent addresses only and answers
    // anything else with `-5: no valid PKH or PK address` — measured. Under
    // this repo's own rule a `-5` is the daemon answering rather than failing,
    // which everywhere else in this tree means "no such thing"; here that arm
    // would print an empty list. So `pecu id list --address bob@` would resolve
    // happily and then report that bob controls nothing, which is the one
    // confident wrong answer this command exists to avoid. Base58check settles
    // the shape without asking anybody.
    if let Some(given) = target.address.as_deref() {
        if given.parse::<Address>().map(|address| address.kind()) != Ok(AddressKind::PubKeyHash) {
            return Err(IdError::NotAPrimaryAddress {
                value: given.to_string(),
            }
            .into());
        }
    }

    let node = node::connect(&settings.profile)?;
    let address = match target.address.as_deref() {
        Some(given) => given.to_string(),
        // `wallet`'s resolver rather than a second one spelled differently: its
        // two refusals already name the flags that fix them — `--key <label>`
        // and `pecu key list` — and a wallet where the same choice is made one
        // way here and another way in `wallet balance` is a wallet whose user
        // has to remember which is which. `None` for the address keeps it off
        // the node: the name-resolving half is the half this command must not
        // reach, and it is the half that needs one.
        None => wallet::resolve_address(ui, &node, settings, None, target.key.as_deref())?.address,
    };

    ui.sdk(format!("node.identities_with_address({address:?})"));
    let entries = match node.identities_with_address(&address) {
        Ok(entries) => entries,
        // Deliberately *not* the "nothing found" arm this repo uses everywhere
        // else. An address that controls nothing comes back as `Ok(vec![])` —
        // the SDK is explicit that the empty list is a real answer — so a `-5`
        // here is the daemon refusing the address, not describing it. Folded
        // into the empty case it would tell somebody their identities are gone.
        Err(verus_sdk::network::RpcError::Node { code: -5 | -8, .. }) => {
            return Err(IdError::AddressRefused { address }.into())
        }
        Err(other) => {
            return Err(node::NodeError::request(
                "listing the identities at the address",
                &settings.profile.node,
                other,
            )
            .into())
        }
    };
    ui.sdk_result(fmt::plural(entries.len(), "identity", "identities"));

    let rows = rows(ui, &node, settings, &entries);

    // One request, asked only when a row carries an absolute unlock height.
    // Whether such a height has passed cannot be answered without the tip, and
    // an identity that carries no timelock at all — which is nearly all of them
    // — needs nothing to say so. A tip the node will not give is carried as
    // unread rather than as zero: the difference decides a word on every locked
    // row, and the panel says which one it printed.
    let tip = if rows
        .iter()
        .any(|row| matches!(row.timelock, Timelock::UntilBlock(_)))
    {
        ui.sdk("node.block_count()");
        let tip = node.block_count().ok();
        ui.sdk_result(match tip {
            Some(tip) => fmt::height(tip.into()),
            None => "the node would not say".to_string(),
        });
        match tip {
            Some(tip) => Tip::Known(tip),
            None => Tip::Unread,
        }
    } else {
        Tip::NotNeeded
    };

    let listing = Listing { address, rows, tip };

    if ui.is_json() {
        emit(&serde_json::json!({
            "address": listing.address,
            "identities": entries
                .iter()
                .zip(&listing.rows)
                .map(|(entry, row)| serde_json::json!({
                    // The name component the node sent, verbatim and on its
                    // own. Not a name any `pecu` command accepts — that is what
                    // `qualified_name` is for — but it is what the reply said.
                    "name": entry.name,
                    "qualified_name": qualified_json(&row.name),
                    "identity_address": entry.identity_address,
                    "parent": entry.parent,
                    "flags": entry.flags,
                    "revoked": entry.is_revoked(),
                    "timelock": list_timelock_json(row.timelock, entry.is_revoked(), listing.tip),
                    // Both verbatim off the reply, and both arrive with every
                    // entry at no extra request. Whether the queried address is
                    // enough on its own is half of "which identities does this
                    // key control", and without these two a 2-of-3 identity is
                    // indistinguishable here from one the key alone can move.
                    "minimum_signatures": entry
                        .identity
                        .get("minimumsignatures")
                        .cloned()
                        .unwrap_or(serde_json::Value::Null),
                    "primary_addresses": entry
                        .identity
                        .get("primaryaddresses")
                        .cloned()
                        .unwrap_or(serde_json::Value::Null),
                    "outpoint": {
                        "txid": entry.outpoint.0.to_string(),
                        "vout": entry.outpoint.1,
                    },
                }))
                .collect::<Vec<_>>(),
            "count": listing.rows.len(),
            // Not derivable from the list above, and the difference between an
            // address that is primary on nothing and one whose identities this
            // call cannot see. A consumer that treats an empty `identities` as
            // "controls no identities" is wrong in a way the array cannot show.
            "primary_only": true,
            // Present when a row needed it, `null` when none did and `null`
            // again when the node would not say — two facts this cannot tell
            // apart and the panel can. Read `timelock.spendable` for whether a
            // row can be moved: it is already decided against this tip, so a
            // consumer has no arithmetic of its own to do, and it is `null`
            // exactly when the tip was needed and not got.
            "tip": match listing.tip {
                Tip::Known(tip) => serde_json::Value::from(tip),
                Tip::NotNeeded | Tip::Unread => serde_json::Value::Null,
            },
        }));
        ui.explain_panel();
        return Ok(());
    }

    if listing.rows.is_empty() {
        // An empty list is an answer, and it is the node's. Said as one:
        // "nothing found" with no provenance is indistinguishable from a
        // command that failed to look, and this is the command somebody runs
        // when they already suspect they have lost something.
        ui.note(format!(
            "no identity on this chain lists {} among its primary addresses",
            // On the `--key` path this is the keystore's word, and a keystore
            // is a file that can be edited or corrupted. Same filter the panel
            // gives it.
            fmt::id(&listing.address, ui.theme.glyphs.ellipsis)
        ));
        ui.note(
            "that is the node's answer, not a failure to ask: a node that had not answered \
                 would be an error here rather than an empty list",
        );
        ui.note(PRIMARY_ONLY);
        ui.note(PRESENT_TENSE);
        ui.note("`pecu id register <name>` registers one");
        ui.explain_panel();
        return Ok(());
    }

    ui.panel(&list_panel(ui, &listing));
    ui.explain_panel();
    Ok(())
}

/// The listing as the panel prints it.
///
/// Gathered into a struct rather than passed as four arguments so that
/// [`list_panel`] can be one function: what the tests drive is then the panel
/// the command prints, and a note only a fixture attaches is not a note the
/// command can quietly lose.
struct Listing {
    address: String,
    rows: Vec<Row>,
    tip: Tip,
}

/// One line of the list.
struct Row {
    name: Name,
    identity_address: String,
    revoked: bool,
    timelock: Timelock,
    control: Control,
}

/// How many of an identity's primary addresses have to sign, out of how many.
///
/// The question a reader of this list is actually asking. They ran it because
/// they hold a key and no name, and "which identities does this key control"
/// has two halves: which ones list the address at all, and whether the address
/// is enough on its own. `getidentitieswithaddress` answers both in the same
/// reply — `minimumsignatures` and `primaryaddresses` arrive with every entry —
/// so the second half costs no request, and dropping it left a 2-of-3 identity
/// rendering exactly like one the key alone can move.
///
/// Both halves are optional because both are read off the raw identity object.
/// A reply that does not carry them leaves an unknown rather than a confident
/// `1-of-1`, which is the answer somebody acts on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Control {
    min_sigs: Option<u64>,
    primaries: Option<usize>,
}

impl Control {
    /// The same two fields `id show`'s CONTROL section reads, under the same
    /// rule: anything missing is unknown, never a default.
    fn of(identity: &serde_json::Value) -> Self {
        Self {
            min_sigs: identity.get("minimumsignatures").and_then(|v| v.as_u64()),
            primaries: identity
                .get("primaryaddresses")
                .and_then(|v| v.as_array())
                .map(Vec::len),
        }
    }

    /// Whether the address that was asked about is enough on its own.
    ///
    /// A threshold of one, whatever the number of primaries. The queried
    /// address is one of them by construction — that is what the RPC matched
    /// on — so one signature being enough means *this* signature is enough.
    /// `monkins@` on VRSCTEST is the live case: two primary addresses, one
    /// required, and either key moves it. `2-of-3` is the case that is not.
    ///
    /// An unknown is not this case either, and must not read as one: a
    /// threshold nobody stated is not a threshold this key meets.
    fn moves_alone(self) -> bool {
        self.min_sigs == Some(1) && self.primaries.is_some_and(|total| total > 0)
    }

    /// Whether this row is the ordinary one: one key, its own, nothing to
    /// explain. Anything else earns the note under the table.
    fn is_plain(self) -> bool {
        (self.min_sigs, self.primaries) == (Some(1), Some(1))
    }

    /// What the `signers` column prints, or `None` when the reply did not say.
    fn threshold(self) -> Option<String> {
        match (self.min_sigs, self.primaries) {
            (Some(min), Some(total)) if total > 0 => Some(format!("{min}-of-{total}")),
            _ => None,
        }
    }
}

/// The chain tip, and whether it was ever wanted.
///
/// Three states rather than an `Option<u32>`, because "nobody asked" and "the
/// node would not say" print different words on a locked row and warrant
/// different notes. Collapsed to `None` they printed the same, and the more
/// alarming of the two was the one that meant nothing was wrong.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Tip {
    /// No row carried an absolute unlock height, so none was fetched.
    NotNeeded,
    Known(u32),
    /// A row needed one and the request failed.
    Unread,
}

/// A name for one identity, in the spelling another `pecu` command accepts.
///
/// Three answers, not two, for the reason [`crate::currency_name::CurrencyName`]
/// has three: a name that could not be built is an **unknown**, and printing the
/// bare name component instead would hand the reader a string that looks like a
/// name and is not one.
enum Name {
    /// Hand this to `pecu id show`, `pecu send --to` or `pecu id update`
    /// unedited: `pecucli7@`, `crypto.Kaiju.VRSCTEST@`.
    Usable(String),
    /// The node knows no currency with this identity's parent id, so there is
    /// no parent name to build one out of.
    NoParent,
    /// No parent name was got, and that says nothing about the identity. The
    /// string is why.
    Unknown(String),
}

/// Whether an identity's own name component is the whole of its name.
enum Parentage {
    /// It sits directly on the chain being read, so `name@` is the whole name
    /// and costs no request at all.
    TopLevel,
    /// It sits under another currency, whose name is needed to build one.
    Under(CurrencyId),
    /// The reply did not say. Not a name, and not a licence to guess at one.
    Unsaid,
}

/// Read an identity's parentage off the reply, without asking anything.
///
/// `systemid` is the field that makes this free. It is in every entry
/// `getidentitieswithaddress` returns — verified against api.verustest.net —
/// and an identity whose `parent` *is* the system is one that sits at the top
/// of this chain, where `name@` is already the fully qualified name. Twelve of
/// the twelve identities across the three sample addresses in #48 are that
/// case, so the common path spends nothing.
///
/// Appending `@` unconditionally instead would be wrong rather than merely
/// optimistic. `crypto.Kaiju.VRSCTEST@` has the name component `crypto`, and
/// `crypto@` is refused today only because nobody has registered a top-level
/// `crypto` — nothing stops one being registered tomorrow, and then this list
/// would print a name that resolves to somebody else's identity.
fn parentage(entry: &IdentityAtAddress) -> Parentage {
    let Some(system) = entry.identity.get("systemid").and_then(|v| v.as_str()) else {
        return Parentage::Unsaid;
    };
    // The chain's own identity is checked first, because its `parent` is the
    // chain it was launched *from* — `VRSCTEST@`'s parent is the VRSC mainnet
    // root, an i-address this node has no currency for. The parent test below
    // would send that to `getcurrency`, take the refusal, and print
    // `(no such parent)` for the one identity whose name is on every panel.
    if entry.identity_address == system || entry.parent == system {
        return Parentage::TopLevel;
    }
    match entry.parent.parse::<Address>() {
        Ok(parent) if parent.kind() == AddressKind::Identity => {
            Parentage::Under(CurrencyId::from_bytes(parent.hash()))
        }
        _ => Parentage::Unsaid,
    }
}

/// Every row, with its name resolved — one request per **distinct** parent, and
/// none at all for an address whose identities are all top-level.
///
/// The deduplication is free: [`look_up_qualified_names`] takes a set, so nine
/// identities sharing one parent collapse to one `getcurrency` without this
/// having to arrange it. The bound on the bad case is [`name_budget`]'s single
/// deadline, shared across every parent rather than one timeout each — a name
/// is display text and the i-address beside it is the answer, so the whole
/// naming step is worth roughly one request's worth of waiting however many
/// parents turn up.
fn rows(ui: &Ui, node: &Node, settings: &Settings, entries: &[IdentityAtAddress]) -> Vec<Row> {
    let wanted: BTreeSet<CurrencyId> = entries
        .iter()
        .filter_map(|entry| match parentage(entry) {
            Parentage::Under(parent) => Some(parent),
            Parentage::TopLevel | Parentage::Unsaid => None,
        })
        .collect();

    let named = if wanted.is_empty() {
        BTreeMap::new()
    } else {
        // One `--explain` line naming the count, not one per parent.
        ui.sdk(format!(
            "node.currency_definition(…) for {}",
            fmt::plural(wanted.len(), "parent", "parents")
        ));
        let named = look_up_qualified_names(node, &wanted, name_budget(&settings.profile));
        ui.sdk_result(name_result(&named, &wanted));
        named
    };

    build_rows(entries, &named)
}

/// The rows a set of entries and a set of parent names make, with no node in
/// sight — which is what lets a test drive the naming rule that decides whether
/// this list is usable at all.
fn build_rows(
    entries: &[IdentityAtAddress],
    named: &BTreeMap<CurrencyId, CurrencyName>,
) -> Vec<Row> {
    entries
        .iter()
        .map(|entry| Row {
            name: name_of(entry, named),
            identity_address: entry.identity_address.clone(),
            // Off the flag, not off a status string: this reply carries no
            // status string, so `id show`'s source for the same fact does not
            // exist here.
            revoked: entry.is_revoked(),
            // `entry.identity` is the whole identity object, flags and timelock
            // reinserted, so the one reader in this tree that pairs those two
            // fields correctly works on it unchanged.
            timelock: timelock_of(&entry.identity),
            control: Control::of(&entry.identity),
        })
        .collect()
}

fn name_of(entry: &IdentityAtAddress, named: &BTreeMap<CurrencyId, CurrencyName>) -> Name {
    match parentage(entry) {
        Parentage::TopLevel => Name::Usable(format!("{}@", entry.name)),
        Parentage::Under(parent) => match named.get(&parent) {
            Some(CurrencyName::Known(parent)) => Name::Usable(format!("{}.{parent}@", entry.name)),
            Some(CurrencyName::Absent) => Name::NoParent,
            Some(CurrencyName::Failed(why)) => Name::Unknown(why.clone()),
            // No entry at all: the shared deadline ran out before this parent
            // came up. Nothing was asked, which is a different fact from a
            // lookup that was made and came back empty-handed.
            None => Name::Unknown("the parent's name was not looked up".to_string()),
        },
        Parentage::Unsaid => {
            Name::Unknown("the reply did not say what system this identity is on".to_string())
        }
    }
}

/// One identity's usable name, as JSON.
///
/// The same three-answer grammar [`crate::currency_name::name_json`] uses, and
/// for the same reason: a bare string cannot say that no name was got, and
/// `null` on its own reads as "this identity has no name".
fn qualified_json(name: &Name) -> serde_json::Value {
    match name {
        Name::Usable(name) => serde_json::json!({ "known": true, "name": name }),
        Name::NoParent => serde_json::json!({
            "known": true,
            "name": null,
            "reason": "the node has no currency with this identity's parent id",
        }),
        Name::Unknown(why) => serde_json::json!({ "known": false, "error": why }),
    }
}

/// The IDENTITIES panel, built away from the node so a test can render it.
fn list_panel(ui: &Ui, listing: &Listing) -> Panel {
    let palette = ui.theme.palette;
    let glyphs = ui.theme.glyphs;
    let table = list_table(ui, &listing.rows, listing.tip);

    // Two different ways a printed name stops being the name, and a reader can
    // act on neither. One is this command's own budget on untrusted display
    // text; the other is the frame taking the column down to fit, which happens
    // after the row is built and without the row knowing. The second is
    // measured against the same ceiling `Panel` fits the table to — and only
    // under the framed skin, because the plain one has no border to run out
    // through and prints the table unbudgeted.
    let over_budget = listing.rows.iter().any(|row| match &row.name {
        Name::Usable(name) => shortened_name(name, glyphs.ellipsis).1,
        Name::NoParent | Name::Unknown(_) => false,
    });
    let over_frame = !ui.theme.is_plain() && table.shortens_at(&ui.theme, ui.theme.width);

    let mut panel = Panel::new("IDENTITIES")
        // Filtered, because on the `--key` path this is the keystore's word
        // rather than something parsed off the command line: a keystore is a
        // file, and a newline in one forges a row inside the frame. An honest
        // address is exactly an address wide and comes back untouched.
        .row(
            "address",
            Text::of(fmt::id(&listing.address, glyphs.ellipsis), palette.value),
        )
        .row(
            "found",
            Text::of(
                fmt::plural(listing.rows.len(), "identity", "identities"),
                palette.accent,
            ),
        )
        .rule()
        .table(table)
        .note(Text::of(PRIMARY_ONLY, palette.muted))
        .note(Text::of(PRESENT_TENSE, palette.muted));

    if over_budget || over_frame {
        // Silence here is the expensive kind. `fmt::untrusted` cuts from the
        // middle and keeps the tail, so a shortened name came out still ending
        // in `@` and read as a whole one — and `pecu id show` answers "nothing
        // on this chain is called that" for it. The cut now shows in the cell,
        // and this says where the whole name is.
        panel = panel.note(Text::of(
            "a name too wide for its column is shortened here, and a shortened name is not one \
             another `pecu` command accepts: `pecu id list --json` carries every name whole, and \
             the i-address on the row is the handle that needs no name",
            palette.warn,
        ));
    }

    if listing.rows.iter().any(|row| !row.control.is_plain()) {
        // The whole point of the column, said once — and said carefully,
        // because the threshold and the number of primaries are two different
        // facts. `monkins@` on this chain is `1-of-2`: a second key exists and
        // either one is enough, which is not the same warning as `2-of-3`.
        panel = panel.note(Text::of(
            "`signers` is how many of an identity's primary addresses have to sign, out of how \
             many: `2-of-3` means this address is not enough on its own, `1-of-2` means it is and \
             so is another key, and `unknown` means the reply did not carry the threshold",
            palette.warn,
        ));
    }

    if listing
        .rows
        .iter()
        .any(|row| !matches!(row.name, Name::Usable(_)))
    {
        // The row is still actionable, and saying which handle to use is the
        // difference between a line a reader can act on and one they conclude
        // is broken.
        panel = panel.note(Text::of(
            "a row with no name is one whose parent could not be named; its i-address is whole, \
             and `pecu id show` takes one",
            palette.warn,
        ));
    }
    if listing.tip == Tip::Unread {
        // Silence here would let `timelocked` read as `locked`, which is a
        // claim about spendability that nothing in this run established.
        panel = panel.note(Text::of(
            "the chain tip could not be read, so `timelocked` says a timelock is set, not that \
             it is still in force",
            palette.warn,
        ));
    }
    if listing
        .rows
        .iter()
        .any(|row| !row.revoked && !matches!(row.timelock, Timelock::None))
    {
        panel = panel.note(Text::of(
            "`pecu id show <name>` gives the form of a timelock and the height it turns on",
            palette.muted,
        ));
    }
    panel
}

/// The `id list` table.
///
/// Two columns may be shortened, and the order matters: the name pays first,
/// and the i-address is touched only once the name is already down to its own
/// header — and then only as far as the short address form, never below it.
///
/// The name is the column that makes this table too wide. It is display text a
/// registrant chose, bounded by nothing the chain enforces, and it is the one
/// thing on the row recoverable without the row: `id list --json` carries it in
/// full. The i-address cannot be recovered from anywhere else on screen, it is
/// what the SDK itself steers any destructive follow-up at, and it is what
/// keeps a nameless row usable. So the queue is name, then id — the other way
/// round, one long name would cut *every* row's i-address to the width of the
/// word I-ADDRESS, because a column's width is the maximum across its rows.
///
/// The state column is never shortened: `revoked` cut from the middle is not a
/// shorter word, it is a different one, on the one cell that says whether an
/// identity is still yours.
fn list_table(ui: &Ui, rows: &[Row], tip: Tip) -> Table {
    let palette = ui.theme.palette;
    let glyphs = ui.theme.glyphs;
    let mut table = Table::new(vec![
        Column::left("name"),
        Column::left("i-address"),
        Column::left("state"),
        Column::left("signers"),
    ])
    .elidable(0)
    .elidable_to(1, fmt::address_width(glyphs.ellipsis));
    for row in rows {
        table.push(vec![
            name_cell(ui, &row.name),
            // An i-address, and the node is the only thing saying so.
            Text::of(
                fmt::id(&row.identity_address, glyphs.ellipsis),
                palette.value,
            ),
            state_cell(ui, row, tip),
            signers_cell(ui, row.control),
        ]);
    }
    table
}

/// Whether the address that was asked about is enough on its own to move this.
///
/// `1-of-1` is printed rather than left blank, and it is printed on every row.
/// A column that appears only when something is unusual cannot be told from a
/// command that does not report the fact at all, and this is the fact the
/// reader came for. The grammar is `id show`'s, so the two commands do not
/// spell the same thing two ways.
fn signers_cell(ui: &Ui, control: Control) -> Text {
    let palette = ui.theme.palette;
    match control.threshold() {
        Some(threshold) if control.moves_alone() => Text::of(threshold, palette.muted),
        Some(threshold) => Text::of(threshold, palette.warn),
        // Not `1-of-1`. That is the answer somebody acts on, and the reply did
        // not give it.
        None => Text::of("unknown", palette.warn),
    }
}

/// One usable name as the column prints it, and whether the budget cut it.
///
/// Not `fmt::untrusted`, and the difference is the whole of #48's promise that
/// a printed name is one another command accepts. `untrusted` cuts from the
/// middle and keeps the tail, which on a VerusID name keeps the `@` — so a
/// sixty-character name came out as `aaaa…aaaa@`, wearing the one mark that
/// says "this is a whole name", and `pecu id show` answered "nothing on this
/// chain is called that" for it. Verus permits 64-byte name components, and a
/// qualified sub-identity passes this budget with two ordinary ones, so this is
/// a name a chain really carries rather than a pathological case.
///
/// Cut from the end instead: the `@` goes with everything else that goes, the
/// cell ends in the ellipsis, and what is kept is the leaf component — the part
/// that says which identity this is. The frame may cut the cell again, and it
/// cuts *around* an ellipsis that is already there rather than opening a second
/// hole, so the mark survives. The whole name is in `--json`, and the panel
/// says so on any run where this fires.
fn shortened_name(name: &str, ellipsis: &str) -> (String, bool) {
    let filtered = fmt::neutralised(name);
    if filtered.chars().count() <= NAME_BUDGET {
        return (filtered, false);
    }
    let head = NAME_BUDGET.saturating_sub(ellipsis.chars().count());
    (
        filtered
            .chars()
            .take(head)
            .chain(ellipsis.chars())
            .collect(),
        true,
    )
}

fn name_cell(ui: &Ui, name: &Name) -> Text {
    let palette = ui.theme.palette;
    let glyphs = ui.theme.glyphs;
    match name {
        // Untrusted display text: a registrant chose it, and a newline in it
        // would forge a row inside the frame.
        Name::Usable(name) => Text::of(shortened_name(name, glyphs.ellipsis).0, palette.accent),
        // Not `(unnamed)`. The chain has not said this identity has no name; it
        // has said it has no currency with the parent id this identity names.
        Name::NoParent => Text::of("(no such parent)", palette.muted),
        // And not `(name unreadable)` either, which would name a cause. Most of
        // the ways this happens are not a garbled answer.
        Name::Unknown(_) => Text::of("(name unknown)", palette.warn),
    }
}

/// The one word that says whether this identity is still yours to move.
///
/// Kept to a word so the row fits beside a whole i-address: the height a
/// timelock turns on belongs to `id show`, which has a section for it and the
/// room to say which of the two forms it is.
fn state_cell(ui: &Ui, row: &Row, tip: Tip) -> Text {
    let palette = ui.theme.palette;
    let glyphs = ui.theme.glyphs;
    let word = |glyph: &str, style: anstyle::Style, word: &str| {
        Text::of(glyph, style).space().push(word.to_string(), style)
    };
    if row.revoked {
        return word(glyphs.danger, palette.danger, "revoked");
    }
    match row.timelock {
        Timelock::None => word(glyphs.ok, palette.ok, "active"),
        // Certain without a tip: the delay does not start counting until an
        // unlock is asked for, so there is no height at which this opens.
        Timelock::DelayAfterUnlock(_) => word(glyphs.warn, palette.warn, "locked"),
        Timelock::UntilBlock(height) => match tip {
            // A countdown that has elapsed leaves its height on the identity
            // forever, because nothing clears it. Reporting that as locked
            // would be wrong about an identity that unlocked years ago.
            Tip::Known(tip) if tip >= height => word(glyphs.ok, palette.ok, "active"),
            Tip::Known(_) => word(glyphs.warn, palette.warn, "locked"),
            // The height is known and whether it has passed is not, so this
            // says a timelock is set and stops there. `locked` would be a guess
            // about spendability, and the panel carries a note saying so.
            Tip::NotNeeded | Tip::Unread => word(glyphs.warn, palette.warn, "timelocked"),
        },
    }
}

// ── register ────────────────────────────────────────────────────────────────

/// Start a registration, or pick up one already in progress.
///
/// Re-running the same command is the whole interface: step one broadcasts and
/// saves, step two waits for the confirmation, step three reveals and pays.
pub fn register(
    ui: &Ui,
    settings: &Settings,
    globals: &Globals,
    args: &IdRegisterArgs,
) -> miette::Result<()> {
    let outcome = register_inner(ui, settings, globals, args);
    if !ui.is_json() {
        ui.explain_panel();
    }
    outcome
}

fn register_inner(
    ui: &Ui,
    settings: &Settings,
    globals: &Globals,
    args: &IdRegisterArgs,
) -> miette::Result<()> {
    if !settings.profile.allow_spend {
        return Err(IdError::SpendingDisabled {
            profile: settings.profile.name.clone(),
        }
        .into());
    }

    let name = bare_name(&args.name)?;
    let path = pending_path(settings, &name);
    let node = node::connect(&settings.profile)?;

    // A reservation that can no longer be broadcast is worth nothing, but the
    // file alone is enough to wedge every later attempt at the same name. This
    // is the only way out, so it discards rather than repairs.
    if args.restart {
        // A dry run that deletes this has already done the one irreversible
        // thing in the command. The salt is not on the chain, not on the node
        // and not anywhere else, so discarding it loses the name and the
        // commitment fee both — which is precisely what --dry-run promises not
        // to do, however explicit --restart is about wanting it gone.
        if globals.dry_run {
            if path.exists() {
                ui.note(format!(
                    "--dry-run — the saved reservation for `{name}` would be discarded"
                ));
            }
        } else if path.exists() {
            std::fs::remove_file(&path).map_err(|source| IdError::PendingIo {
                action: "delete",
                path: path.clone(),
                source,
            })?;
            ui.note(format!("discarded the saved reservation for `{name}`"));
        }
        return begin(ui, settings, globals, &node, args, &name, &path);
    }

    match load_pending(&path)? {
        Some(pending) => resume(ui, settings, globals, &node, args, pending, &path),
        None => begin(ui, settings, globals, &node, args, &name, &path),
    }
}

/// A name with no parent and no `@`. Sub-identities are a different shape and
/// are refused rather than half-supported.
fn bare_name(given: &str) -> Result<String, IdError> {
    let trimmed = given.trim().trim_end_matches('@');
    if trimmed.is_empty() || trimmed.contains('.') || trimmed.contains('@') {
        return Err(IdError::BadName {
            name: given.to_string(),
        });
    }
    Ok(trimmed.to_string())
}

fn pending_path(settings: &Settings, name: &str) -> PathBuf {
    settings
        .paths
        .pending_dir()
        .join(format!("{}.json", name.to_lowercase()))
}

/// Whether the registration that just ran got as far as the reveal.
///
/// The reservation holds the salt and is deleted in two places: `--restart`
/// discards it in `register_inner` before re-committing, and `resume` removes
/// it after `complete` returns, at the "Only now is the salt worthless" line.
/// `ensure_exists` builds its own `IdRegisterArgs` with `restart: false`, so
/// the second is the only one it can reach. A file that survived `register` is
/// therefore a registration that stopped at step one, and a file that is gone
/// is a name that has been claimed and only needs mining.
///
/// Anything that wires `--restart` through to `ensure_exists` breaks that: the
/// discard happens *before* `begin` writes a fresh reservation, so absence
/// would no longer mean the reveal went out.
fn reveal_was_broadcast(reservation: &Path) -> bool {
    !reservation.exists()
}

fn load_pending(path: &PathBuf) -> Result<Option<Pending<AwaitingCommitment>>, IdError> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(source) => {
            return Err(IdError::PendingIo {
                action: "read",
                path: path.clone(),
                source,
            })
        }
    };
    serde_json::from_str(&text)
        .map(Some)
        .map_err(|error| IdError::PendingCorrupt {
            path: path.clone(),
            detail: error.to_string(),
        })
}

fn save_pending(path: &PathBuf, pending: &Pending<AwaitingCommitment>) -> Result<(), IdError> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|source| IdError::PendingIo {
            action: "create",
            path: parent.to_path_buf(),
            source,
        })?;
    }
    let json = serde_json::to_string_pretty(pending).expect("a Pending is plain data");
    std::fs::write(path, format!("{json}\n")).map_err(|source| IdError::PendingIo {
        action: "write",
        path: path.clone(),
        source,
    })
}

/// Step one: commit to the name.
fn begin(
    ui: &Ui,
    settings: &Settings,
    globals: &Globals,
    node: &Node,
    args: &IdRegisterArgs,
    name: &str,
    path: &PathBuf,
) -> miette::Result<()> {
    // Cheap and worth doing before a passphrase prompt, let alone a fee.
    if node.identity(&format!("{name}@")).is_ok() {
        return Err(IdError::Taken {
            name: format!("{name}@"),
        }
        .into());
    }

    let store = Keystore::new(&settings.paths);
    let envelope = choose_key(&store, args.from.as_deref())?;
    let options = RegistrationOptions {
        primary_addresses: if args.primary.is_empty() {
            vec![envelope.address.clone()]
        } else {
            args.primary.clone()
        },
        min_sigs: args.min_sigs,
        referral: args.referral.clone(),
        // `None` is the daemon's default and today's behaviour: the identity
        // is its own revocation and recovery authority. The SDK can now set
        // both at registration, which saves a second transaction — but that is
        // a new pair of flags and a new panel row, not part of moving the pin.
        revocation_authority: None,
        recovery_authority: None,
        pin_fee: None,
    };

    // Cheap, offline, and the last chance: everything after this leads to a
    // commitment the reveal would refuse to spend. `unwrap_or(1)` is the SDK's
    // own default, applied where `prepare_registration` builds the `Pending` —
    // checking the flag as written would not be checking what gets stored.
    check_controls(&options.primary_addresses, options.min_sigs.unwrap_or(1))?;

    let secret = keystore::passphrase(&format!("passphrase for `{}`", envelope.label), false)?;
    let key = envelope.unlock(&secret)?;

    ui.sdk(format!(
        "verus_sdk::network::prepare_registration(&node, &key, {name:?}, &options)"
    ));
    let mut pending = prepare_registration(node, &key, name, &options)
        .map_err(|source| flow("preparing the registration", source))?;
    ui.sdk_result(format!(
        "Pending<AwaitingCommitment> {{ fee: {}, txid: {} }}",
        pending.registration_fee, pending.commitment_txid
    ));

    if !ui.is_json() {
        ui.panel(&cost_panel(
            ui, settings, name, &envelope, &pending, &options,
        ));
    }

    // Stops here, and crucially *before* the save below. A saved registration is
    // what the next run resumes; one whose commitment was never broadcast would
    // send that run to poll for a transaction nobody made.
    //
    // The salt drawn above is discarded with it. That is free — nothing was
    // committed to, so the next real run simply draws another.
    if globals.dry_run {
        if ui.is_json() {
            emit(&serde_json::json!({
                "kind": "estimate",
                "name": name,
                "registration_fee": pending.registration_fee.to_sat(),
                "referral": options.referral,
                "primary_addresses": options.primary_addresses,
                "min_sigs": options.min_sigs,
                "broadcast": false,
            }));
        } else {
            ui.blank();
            ui.note("nothing was broadcast and nothing was saved. Drop --dry-run to claim it");
        }
        return Ok(());
    }

    // `--json` is output, not consent — the same rule `pecu send` follows, and
    // this one burns a hundred coins rather than moving them.
    if !globals.yes {
        if ui.is_json() {
            return Err(IdError::NeedsYes.into());
        }
        confirm(ui)?;
    }

    // Saved *before* anything is broadcast. The salt inside cannot be recovered
    // from the chain, from the node, or from this program's memory once it
    // exits: losing it loses the name and the fee both.
    save_pending(path, &pending)?;

    ui.sdk("pending.broadcast_commitment(&node, &node)");
    pending
        .broadcast_commitment(node, node)
        .map_err(|source| flow("broadcasting the commitment", source))?;
    ui.sdk_result(format!("commitment {}", pending.commitment_txid));
    save_pending(path, &pending)?;

    if ui.is_json() {
        emit(&serde_json::json!({
            "kind": "commitment",
            "name": name,
            "commitment_txid": pending.commitment_txid,
            "registration_fee": pending.registration_fee.to_sat(),
            "saved_to": path,
            "next": "run the same command again once it confirms",
        }));
        return Ok(());
    }

    let palette = ui.theme.palette;
    ui.blank();
    ui.ok(format!(
        "step 1 of 2 — commitment {}",
        pending.commitment_txid
    ));
    ui.panel(
        &Panel::new("SAVED")
            .path("registration", path)
            .note(Text::of(
                "this file holds the salt. Without it the name cannot be claimed and the \
                 commitment fee is lost",
                palette.warn,
            ))
            .note(Text::of(
                if args.no_wait {
                    "run the same command again once the commitment confirms — a block or so"
                } else {
                    "waiting for it to confirm. Interrupting is safe — the file above survives \
                     and the same command picks up where this left off"
                },
                palette.muted,
            )),
    );
    if args.no_wait {
        return Ok(());
    }
    // Straight into step two rather than asking the caller to run it again. The
    // file is already on disk, so a Ctrl-C here costs nothing but the wait.
    resume(ui, settings, globals, node, args, pending, path)
}

/// The two facts about an identity's control that the SDK only checks in step
/// two — after the commitment is on chain and the wait is spent.
///
/// `build_identity_registration` refuses `min_sigs == 0` or a threshold above
/// the number of primaries with `InvalidMinSigs`, and any primary that is not
/// a pubkey hash with `UnsupportedRecipient`. `prepare_registration` checks
/// neither: it front-loads the name, fee and referral checks precisely so a
/// commitment is not wasted on a registration that cannot complete, and then
/// stores these two untouched. A `3-of-1` was discovered only once the
/// commitment had confirmed, and `--restart` was the only way out.
///
/// Takes the values as the SDK will hold them rather than the flags as typed:
/// `--min-sigs 3` with no `--primary` is the case that matters, and its list is
/// the paying key's single address, not an empty one. Needs no node and no
/// key, so it runs before the passphrase prompt.
fn check_controls(primary_addresses: &[String], min_sigs: u32) -> Result<(), IdError> {
    for address in primary_addresses {
        match address.parse::<Address>() {
            Ok(parsed) if parsed.kind() == AddressKind::PubKeyHash => {}
            _ => {
                return Err(IdError::BadPrimary {
                    value: address.clone(),
                })
            }
        }
    }
    if min_sigs == 0 || min_sigs as usize > primary_addresses.len() {
        return Err(IdError::BadMinSigs {
            min_sigs,
            primaries: primary_addresses.len(),
        });
    }
    Ok(())
}

/// Register `name` if the chain does not have it yet, and wait until it does —
/// or refuse, when no amount of waiting could get it there.
///
/// For `currency launch --register`, where the identity is a prerequisite
/// rather than the point. Returns without doing anything if the name already
/// exists, so it is safe to call on a re-run — and a registration interrupted
/// half way is picked up by the same saved-reservation path everything else
/// uses.
///
/// Two outcomes are neither of those. Under `--dry-run` nothing is broadcast,
/// so there is no identity to be defined by and nothing on its way to a block:
/// `DryRunCannotRegister`. And a registration that stopped after the
/// commitment has claimed nothing yet, so there is again no identity and
/// nothing to wait for: `RegistrationUnfinished`. Both are errors rather than
/// quiet skips because the caller reads the defining identity off the chain
/// the moment this returns — a launch with no defining identity is not a
/// smaller launch, it is no launch at all, and neither is it something a
/// preview can stand in for.
///
/// Deliberately not reachable without an explicit flag. Registration burns 100
/// VRSCTEST and a typo'd name is a plausible mistake; creating `pecubaskt1@`
/// because somebody misspelled their own basket would be an expensive
/// convenience.
pub fn ensure_exists(
    ui: &Ui,
    settings: &Settings,
    globals: &Globals,
    node: &Node,
    name: &str,
    from: Option<&str>,
    timeout: u64,
) -> miette::Result<()> {
    let bare = bare_name(name)?;
    if node.identity(name).is_ok() {
        return Ok(());
    }

    // Nothing below can happen under --dry-run: `begin` stops before it
    // broadcasts, so the identity never appears and the poll below would run out
    // the whole --register-timeout waiting for a transaction nobody made.
    // Refused rather than skipped — the launch preview reads the defining
    // identity off the chain (`node.identity` in `launch`, right after this
    // returns), so with no identity there is no launch to preview.
    if globals.dry_run {
        return Err(IdError::DryRunCannotRegister { name: bare }.into());
    }

    ui.note(format!(
        "{bare}@ does not exist yet — registering it first, for 100 VRSCTEST"
    ));
    ui.blank();

    let args = IdRegisterArgs {
        name: bare.clone(),
        from: from.map(str::to_string),
        primary: Vec::new(),
        min_sigs: None,
        referral: None,
        restart: false,
        no_wait: false,
        timeout,
    };
    let path = pending_path(settings, &bare);
    register(ui, settings, globals, &args)?;

    // `register` returns Ok from six places that broadcast no reveal: --json
    // stops after the commitment, --no-wait stops there too, and `resume`
    // returns on a commitment still unconfirmed or one it had to re-broadcast.
    // Waiting for the identity in any of those is waiting for a transaction
    // that was never sent — which is what burned --register-timeout minutes and
    // then reported a broadcast that had not happened.
    if !reveal_was_broadcast(&path) {
        return Err(IdError::RegistrationUnfinished { name: bare }.into());
    }

    // Step two is broadcast, not mined. The launch that follows reads the
    // identity off the chain, so waiting for the transaction is not enough —
    // it has to be *there*.
    ui.blank();
    ui.note("waiting for the registration to be mined");
    let polls = (timeout * 60) / POLL_SECONDS;
    for attempt in 0..polls.max(1) {
        if node.identity(name).is_ok() {
            ui.ok(format!("{bare}@ is on chain"));
            ui.blank();
            return Ok(());
        }
        std::thread::sleep(std::time::Duration::from_secs(POLL_SECONDS));
        if !ui.is_json() {
            eprintln!(
                "  waiting — not mined yet, {}s elapsed",
                (attempt + 1) * POLL_SECONDS
            );
        }
    }
    Err(IdError::NotMinedInTime {
        name: bare,
        minutes: timeout,
    }
    .into())
}

/// Seconds between polls while waiting for the commitment.
///
/// `Pending::poll` costs up to four requests, so this is a request every few
/// seconds against a public endpoint nobody here pays for. The SDK floors the
/// interval at five seconds for the same reason; thirty is polite and still
/// well inside a block time.
const POLL_SECONDS: u64 = 30;

/// Step two: wait for the confirmation, then reveal and pay.
fn resume(
    ui: &Ui,
    settings: &Settings,
    globals: &Globals,
    node: &Node,
    args: &IdRegisterArgs,
    mut pending: Pending<AwaitingCommitment>,
    path: &PathBuf,
) -> miette::Result<()> {
    let palette = ui.theme.palette;
    let name = pending.name().to_string();

    // These came off disk, not off this run's flags: a reservation written
    // before this check existed still carries bad values into `complete`,
    // where the SDK refuses them after the whole wait and `flow()` blames the
    // node for a bad-flag error. Above the dry-run gate as well as the poll —
    // pricing a registration that can never complete would be a lie. Nothing
    // is deleted: the salt stays until the user says --restart.
    check_controls(&pending.primary_addresses, pending.min_sigs).map_err(|error| {
        IdError::PendingUnusable {
            name: name.clone(),
            path: path.clone(),
            detail: error.to_string(),
        }
    })?;

    // Above the poll rather than beside the `complete` it guards. Step two is
    // the transaction that burns the hundred coins, and `CommitmentGone` below
    // re-broadcasts the commitment and rewrites the saved file — so a gate any
    // further down would let a dry run both spend and write on the path that
    // was already the least expected to. Nothing here asks the chain anything:
    // the fee, the referral chain and the txid all survive on disk.
    //
    // Safe to state the fee before the poll: the SDK records `registration_fee`
    // at prepare and carries it through the transition rather than re-reading
    // it, so this is the number the confirmation below would show.
    if globals.dry_run {
        if ui.is_json() {
            emit(&serde_json::json!({
                "kind": "estimate",
                "name": name,
                "registration_fee": pending.registration_fee.to_sat(),
                "commitment_txid": pending.commitment_txid,
                "primary_addresses": pending.primary_addresses,
                "min_sigs": pending.min_sigs,
                "broadcast": false,
            }));
        } else {
            ui.panel(
                &Panel::new("STEP 2 OF 2")
                    .row("name", Text::of(format!("{name}@"), palette.accent))
                    .row("fee", fee_row(ui, &pending, &settings.profile.currency)),
            );
            ui.blank();
            ui.note(
                "nothing was broadcast and the saved registration is untouched. The commitment \
                 was not polled either — drop --dry-run to check it and claim the name",
            );
        }
        return Ok(());
    }

    // One poll when the caller wants a snapshot; otherwise the SDK's own loop,
    // which floors the interval so a public node is not hammered. Either way a
    // single `CommitmentStatus` comes out and everything below is unchanged.
    let status = if args.no_wait {
        ui.sdk("pending.poll(&node)");
        pending
            .poll(node)
            .map_err(|source| flow("checking the commitment", source))?
    } else {
        let interval = std::time::Duration::from_secs(POLL_SECONDS);
        let max_polls = u32::try_from((args.timeout * 60) / POLL_SECONDS)
            .unwrap_or(u32::MAX)
            .max(1);
        ui.sdk(format!(
            "pending.wait_blocking(&node, &WaitPolicy {{ interval: {POLL_SECONDS}s, max_polls: {max_polls} }})"
        ));
        // The callback outlives this borrow of `ui`, so it prints for itself
        // rather than capturing the renderer. One line per poll, on stderr, so
        // a caller piping stdout still gets clean output.
        let quiet = ui.is_json();
        let policy = WaitPolicy {
            interval,
            max_polls,
            progress: Box::new(move |attempt, confirmations| {
                if quiet {
                    return;
                }
                let elapsed = u64::from(attempt + 1) * POLL_SECONDS;
                if confirmations == 0 {
                    eprintln!("  waiting — in the mempool, {elapsed}s elapsed");
                } else {
                    eprintln!("  waiting — {confirmations} confirmation(s), {elapsed}s elapsed");
                }
            }),
        };
        pending
            .wait_blocking(node, &policy)
            .map_err(|source| flow("waiting for the commitment", source))?
    };

    let ready = match status {
        CommitmentStatus::Waiting { confirmations } => {
            ui.sdk_result(format!("Waiting {{ confirmations: {confirmations} }}"));
            if ui.is_json() {
                emit(&serde_json::json!({
                    "kind": "waiting",
                    "name": name,
                    "confirmations": confirmations,
                    "commitment_txid": pending.commitment_txid,
                }));
                return Ok(());
            }
            ui.panel(
                &Panel::new("WAITING")
                    .row("name", Text::of(format!("{name}@"), palette.accent))
                    .row(
                        "commitment",
                        Text::of(&pending.commitment_txid, palette.value),
                    )
                    .row(
                        "confirmations",
                        Text::of(confirmations.to_string(), palette.value).push(
                            if confirmations == 0 {
                                "  (still in the mempool)"
                            } else {
                                ""
                            },
                            palette.muted,
                        ),
                    )
                    .note(Text::of(
                        if args.no_wait {
                            "run the same command again in a minute".to_string()
                        } else {
                            format!(
                                "still unconfirmed after {} minutes. Nothing is lost — the saved \
                                 registration is intact and the same command carries on",
                                args.timeout
                            )
                        },
                        palette.muted,
                    )),
            );
            return Ok(());
        }
        CommitmentStatus::Ready(ready) => {
            ui.sdk_result("Ready — the commitment confirmed");
            *ready
        }
        CommitmentStatus::Reorged { detail } => return Err(IdError::Reorged { detail }.into()),
        // The documented remedy, not a dead end: "the salt is still good, so
        // the commitment can be re-broadcast". Reporting this and stopping left
        // a registration that could only be abandoned — and it happens for the
        // ordinary reason that a broadcast did not land.
        //
        // Re-broadcasting the same bytes is safe either way. If it merely never
        // propagated the node already has it and the txid is unchanged; if it
        // was dropped this is exactly the retry. `anchor` re-reads the chain
        // position first, so the reorg check afterwards compares against where
        // the chain is now rather than where it was.
        CommitmentStatus::CommitmentGone => {
            ui.sdk("pending.anchor(&node)");
            let unsent = pending
                .anchor(node)
                .map_err(|source| flow("re-anchoring the commitment", source))?;
            ui.sdk_result(format!("re-broadcasting {}", unsent.txid));

            // The reason the node gives is kept rather than flattened back into
            // "it has never seen it": that is the state we came from, and it is
            // what a retry already failed to fix.
            unsent.broadcast(node).map_err(|source| -> miette::Report {
                // Inputs that no longer exist mean these bytes can never land,
                // however often they are retried. Distinct from "the node has
                // not seen it", which is what we came from and what a retry
                // does fix.
                if matches!(
                    &source,
                    FlowError::Rpc(verus_sdk::network::RpcError::Node { code: -26, message })
                        if message.contains("inputs-missing") || message.contains("inputs-spent")
                ) {
                    return IdError::CommitmentStale {
                        name: name.clone(),
                        path: path.clone(),
                    }
                    .into();
                }
                // The expiry is baked into the signed bytes, so re-anchoring
                // cannot move it. A dead reservation that costs nothing to
                // abandon, but only if the user is told to abandon it.
                if matches!(
                    &source,
                    FlowError::Rpc(verus_sdk::network::RpcError::Node { code: -26, message })
                        if message.contains("expiring-soon") || message.contains("tx-expired")
                ) {
                    return IdError::CommitmentExpired { name: name.clone() }.into();
                }
                flow("re-broadcasting the commitment", source).into()
            })?;
            save_pending(path, &pending)?;

            if !args.no_wait && !ui.is_json() {
                ui.note("re-broadcast — waiting for it to confirm");
                return resume(ui, settings, globals, node, args, pending, path);
            }
            if ui.is_json() {
                emit(&serde_json::json!({
                    "kind": "recommitted",
                    "name": name,
                    "commitment_txid": pending.commitment_txid,
                    "next": "run the same command again once it confirms",
                }));
                return Ok(());
            }
            ui.ok(format!(
                "the node had not seen the commitment — re-broadcast {}",
                pending.commitment_txid
            ));
            ui.note("run the same command again once it confirms");
            return Ok(());
        }
        // The state this file used to infer by string-matching `expiring-soon`
        // on a rejected re-broadcast. The SDK tracks the expiry height now and
        // says so by name, which is the difference between knowing and
        // guessing — the two states need opposite actions, retry versus start
        // over, and the guess only ever fired once a broadcast had been
        // attempted. The string match below is kept: it still covers the same
        // rejection arriving from a node the SDK did not classify.
        CommitmentStatus::Expired { .. } => return Err(IdError::CommitmentExpired { name }.into()),
        // `CommitmentStatus` is `#[non_exhaustive]`, and the SDK's own note
        // says why: the set of ways a commitment can fail is not closed. A
        // state this version cannot name is not an error in the registration —
        // the reservation is intact — so it says what it is rather than
        // guessing which of the arms above it resembles.
        other => {
            return Err(IdError::UnknownCommitmentState {
                name,
                detail: format!("{other:?}"),
            }
            .into())
        }
    };

    // `--from`, the same as step one. Ignoring it here meant a keystore with
    // more than one key could not finish a registration at all: the commitment
    // is paid, the name is committed, and the only command that can claim it
    // refuses as ambiguous. The saved file warns that losing it loses the name
    // and the fee — this was the tool being what could not claim it.
    let store = Keystore::new(&settings.paths);
    let envelope = choose_key(&store, args.from.as_deref())?;
    let secret = keystore::passphrase(&format!("passphrase for `{}`", envelope.label), false)?;
    let key = envelope.unlock(&secret)?;

    // `--json` is output, not consent — the same rule `begin` follows two
    // hundred lines up, and this is the half that actually burns the hundred.
    if !globals.yes {
        if ui.is_json() {
            return Err(IdError::NeedsYes.into());
        }
        ui.panel(
            &Panel::new("STEP 2 OF 2")
                .row("name", Text::of(format!("{name}@"), palette.accent))
                .row(
                    "fee",
                    Text::of(fmt::amount(ready.registration_fee), palette.accent)
                        .space()
                        .push(&settings.profile.currency, palette.muted)
                        .push("  burned, not recoverable", palette.warn),
                ),
        );
        confirm(ui)?;
    }

    ui.sdk("ready.complete(&node, &node, &key)");
    // Taken before `complete` consumes `ready`. `Registered::fee_paid` is the
    // undiscounted policy fee — the same number the estimate had to correct —
    // so the receipt would otherwise claim 100 was paid when 80 left.
    let split = Referral::new(
        ready.registration_fee,
        ready.referral_levels,
        ready.referral_chain.len(),
    );
    let referred = !ready.referral_chain.is_empty();

    let registered = ready
        .complete(node, node, &key)
        .map_err(|source| flow("completing the registration", source))?;
    ui.sdk_result(format!("Registered {{ txid: {} }}", registered.txid));

    // Only now is the salt worthless. Removing it early would risk losing a
    // registration that had not actually landed.
    let _ = std::fs::remove_file(path);

    let address = Address::new(AddressKind::Identity, registered.identity_address).to_string();
    if ui.is_json() {
        emit(&serde_json::json!({
            "kind": "registered",
            "name": registered.name,
            "identity_address": address,
            "txid": registered.txid,
            // What actually left the funding address. `fee_paid` is policy
            // before any referral discount, so it is reported under its own
            // name rather than as the cost.
            "paid": if referred { split.outlay.to_sat() } else { registered.fee_paid.to_sat() },
            "registration_fee": registered.fee_paid.to_sat(),
            "referral_paid_out": split.paid_out.to_sat(),
            "burned": if referred { split.burned.to_sat() } else { registered.fee_paid.to_sat() },
        }));
        return Ok(());
    }

    // "Registered" is what the SDK calls the outcome of a successful broadcast,
    // and for a user it would imply the identity exists. It does not until the
    // transaction is mined -- `id show` will say nothing is called this until
    // then, which reads as a failure if the wording promised otherwise.
    ui.blank();
    ui.ok(format!("broadcast — txid {}", registered.txid));
    ui.panel(
        &Panel::new("REGISTRATION SENT")
            .row(
                "name",
                Text::of(format!("{}@", registered.name), palette.accent),
            )
            .row("i-address", Text::of(&address, palette.value))
            .row(
                "paid",
                if referred {
                    Text::of(fmt::amount(split.outlay), palette.value)
                        .space()
                        .push(&settings.profile.currency, palette.muted)
                        .push(
                            format!(
                                "  {} to referrers, {} burned",
                                fmt::amount(split.paid_out),
                                fmt::amount(split.burned)
                            ),
                            palette.muted,
                        )
                } else {
                    Text::of(fmt::amount(registered.fee_paid), palette.value)
                        .space()
                        .push(&settings.profile.currency, palette.muted)
                },
            )
            .note(Text::of(
                "not on chain until this is mined — `pecu id show` will not find it until then",
                palette.warn,
            ))
            .note(Text::of(
                format!(
                    "{}/tx/{}",
                    settings.profile.explorer.trim_end_matches('/'),
                    registered.txid
                ),
                palette.muted,
            )),
    );
    Ok(())
}

/// What a registration actually costs, and where it goes.
///
/// `Pending::registration_fee` is chain policy — the fee *before* any referral
/// discount, as the SDK documents it. Naming a referral makes the registrant
/// pay **less**: each referrer receives `fee / (levels + 2)` and the outlay is
/// `fee * (levels + 1) / (levels + 2)`, with the remainder burned. On VRSCTEST
/// that is 80 paid rather than 100, of which 20 reaches a single referrer.
///
/// Showing the undiscounted figure beside a referral was wrong twice over: too
/// high, and describing money as burned when a fifth of it is a payment to
/// somebody.
struct Referral {
    /// What the registrant actually parts with.
    outlay: Amount,
    /// What reaches referrers, one payout per level in the chain.
    paid_out: Amount,
    /// What is destroyed.
    burned: Amount,
    payouts: usize,
}

impl Referral {
    /// From the three numbers, so both the estimate and the receipt can use it:
    /// `Pending<AwaitingCommitment>` and `Pending<ReadyToRegister>` carry them
    /// alike, and `Registered` carries only the undiscounted fee.
    fn new(fee: Amount, levels: u32, payouts: usize) -> Self {
        let fee = fee.to_sat();
        let divisor = u64::from(levels) + 2;
        let each = fee / divisor;
        let outlay = fee / divisor * (u64::from(levels) + 1);
        let paid_out = each.saturating_mul(payouts as u64);
        Self {
            outlay: Amount::from_sat(outlay),
            paid_out: Amount::from_sat(paid_out),
            burned: Amount::from_sat(outlay.saturating_sub(paid_out)),
            payouts,
        }
    }

    fn of(pending: &Pending<AwaitingCommitment>) -> Self {
        // One payout per referrer actually in the chain, which is the depth the
        // referrer was itself referred at — not the number of levels allowed.
        Self::new(
            pending.registration_fee,
            pending.referral_levels,
            pending.referral_chain.len(),
        )
    }
}

/// The fee row: what leaves the funding address, and what becomes of it.
fn fee_row(ui: &Ui, pending: &Pending<AwaitingCommitment>, currency: &str) -> Text {
    let palette = ui.theme.palette;
    if pending.referral_chain.is_empty() {
        return Text::of(fmt::amount(pending.registration_fee), palette.accent)
            .space()
            .push(currency, palette.muted)
            .push("  burned, not recoverable", palette.warn);
    }
    let split = Referral::of(pending);
    Text::of(fmt::amount(split.outlay), palette.accent)
        .space()
        .push(currency, palette.muted)
        .push(
            format!(
                "  reduced from {} by the referral",
                fmt::amount(pending.registration_fee)
            ),
            palette.muted,
        )
}

fn cost_panel(
    ui: &Ui,
    settings: &Settings,
    name: &str,
    envelope: &Envelope,
    pending: &Pending<AwaitingCommitment>,
    options: &RegistrationOptions,
) -> Panel {
    let palette = ui.theme.palette;
    let currency = &settings.profile.currency;
    let mut panel = Panel::new("REGISTER")
        .row("name", Text::of(format!("{name}@"), palette.accent))
        .row(
            "paying from",
            Text::of(&envelope.address, palette.value)
                .space()
                .push(format!("({})", envelope.label), palette.muted),
        )
        .row("fee", fee_row(ui, pending, currency))
        .row(
            "signatures",
            Text::of(
                format!(
                    "{}-of-{}",
                    options.min_sigs.unwrap_or(1),
                    options.primary_addresses.len()
                ),
                palette.value,
            ),
        );
    for address in &options.primary_addresses {
        panel = panel.line(
            Text::of(ui.theme.glyphs.bullet, palette.muted)
                .space()
                .push(address, palette.value),
        );
    }
    if let Some(referral) = &options.referral {
        let split = Referral::of(pending);
        panel = panel
            .row("referral", Text::of(referral, palette.accent))
            .row(
                "  to referrers",
                Text::of(fmt::amount(split.paid_out), palette.value)
                    .space()
                    .push(currency, palette.muted)
                    .push(
                        format!("  across {}", fmt::plural(split.payouts, "level", "levels")),
                        palette.muted,
                    ),
            )
            .row(
                "  burned",
                Text::of(fmt::amount(split.burned), palette.value)
                    .space()
                    .push(currency, palette.muted),
            );

        // At the cap the chain may have been truncated, and the walk cannot say
        // whether it was: it stops at `referral_levels` and returns. Anyone
        // further back receives nothing, silently, so the possibility is worth
        // stating rather than leaving a referrer to work out why no payment
        // arrived. VRSCTEST allows three, so a fourth level is never paid.
        if split.payouts as u32 >= pending.referral_levels {
            panel = panel.note(Text::of(
                format!(
                    "this chain is at the {} this currency allows, so any referrer further \
                     back receives nothing",
                    fmt::plural(pending.referral_levels as usize, "level", "levels")
                ),
                palette.muted,
            ));
        }
    }
    panel
        .note(Text::of(
            "two transactions: this commits to the name, and a second one claims it once the \
             first confirms",
            palette.muted,
        ))
        // What the default actually costs you, and it is worse than "no
        // independent guardian". A consensus rule in `identity.cpp` refuses a
        // revocation whose subject is its own **recovery** authority, because
        // nobody could recover it afterwards:
        //
        //     if (oldIdentity.IsRevocation(newIdentity) &&
        //         oldIdentity.recoveryAuthority == oldIdentity.GetID() &&
        //         !oldIdentity.HasTokenizedControl())
        //
        // So as registered this identity is unrevokable, full stop. The SDK
        // refuses it too, as `TxError::RevocationWouldStrand`, before a
        // signature exists.
        //
        // The trigger is *recovery*, not revocation: an identity may revoke
        // itself as long as somebody else can recover it.
        .note(Text::of(
            "as registered this identity CANNOT BE REVOKED: it is its own recovery authority, \
             and consensus refuses a revocation nobody could undo",
            palette.warn,
        ))
        .note(Text::of(
            "both authorities point at the identity itself, so the keys above are the only \
             thing protecting it — there is nothing else to fall back on",
            palette.warn,
        ))
        // The half of the old warning that really was false. An update signed
        // by these primary keys can hand either authority to another VerusID —
        // but only while the identity is still its own authority. Once one
        // points elsewhere, these keys can no longer move it back.
        .note(Text::of(
            "you can point recovery at another VerusID later, which makes it revocable — but \
             only while it is still its own authority. That hand-off is one-way",
            palette.muted,
        ))
}

/// When the block at `height` was mined, if the node will say.
///
/// `None` on any failure. A date is a nicety; refusing to show an identity
/// because its block header could not be fetched would not be.
fn block_time(ui: &Ui, node: &Node, height: u32) -> Option<i64> {
    ui.sdk(format!("node.block(\"{height}\")"));
    let block = node.block(&height.to_string()).ok()?;
    let time = block.get("time").and_then(|v| v.as_i64());
    ui.sdk_result(match time {
        Some(time) => fmt::timestamp(time),
        None => "no time in the header".to_string(),
    });
    time
}

/// The block a change landed in, with its date and how long ago that was.
fn registered_row(ui: &Ui, height: u32, mined: Option<i64>) -> Text {
    let palette = ui.theme.palette;
    let mut row = Text::of(
        format!("block {}", fmt::height(height.into())),
        palette.value,
    );
    if let Some(time) = mined {
        row = row
            .push("   ", palette.muted)
            .push(fmt::timestamp(time), palette.value);
        // Relative as well as absolute: "2026-02-02" answers a different
        // question from "six months ago", and a reader usually wants both.
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|elapsed| elapsed.as_secs())
            .unwrap_or(0);
        let then = u64::try_from(time).unwrap_or(0);
        row = row.push(
            format!("  ({} ago)", fmt::duration(now.saturating_sub(then))),
            palette.muted,
        );
    }
    row
}

/// The TIMELOCK section, always.
///
/// This used to be omitted for an unlocked identity, on the reasoning that a
/// row saying "not locked" everywhere is noise and the section's presence is
/// the signal. That was wrong. An identity is always in one of these states, so
/// silence does not read as "not locked" — it reads as "this wallet did not
/// say", which is indistinguishable from "did not look". Whether funds can move
/// is not a question to answer only sometimes.
fn timelock_panel(ui: &Ui, panel: Panel, timelock: Timelock, tip: Option<u32>) -> Panel {
    let palette = ui.theme.palette;
    let glyphs = ui.theme.glyphs;
    match timelock {
        // "never locked" rather than "unlocked": this is the absence of a
        // timelock, not the end of one. An identity whose countdown finished
        // says something different below, and keeps its leftover height.
        Timelock::None => panel.section("TIMELOCK").row(
            "state",
            Text::of(glyphs.ok, palette.ok)
                .space()
                .push("never locked", palette.ok),
        ),
        Timelock::UntilBlock(height) => {
            // Past or future decides the tense, and both are ordinary states: a
            // countdown that has elapsed leaves its height on the identity
            // forever, because nothing clears it. "unlocks at" on a height that
            // went by last week reads as though something were still pending.
            let elapsed = tip.is_some_and(|tip| tip >= height);
            let panel = panel.section("TIMELOCK").row(
                if elapsed { "unlocked at" } else { "unlocks at" },
                Text::of(
                    format!("block {}", fmt::height(height.into())),
                    palette.value,
                ),
            );
            match tip {
                Some(tip) if tip >= height => panel
                    .row(
                        "state",
                        Text::of(glyphs.ok, palette.ok)
                            .space()
                            .push("unlocked", palette.ok),
                    )
                    .note(Text::of(
                        "the height stays on the identity after a countdown finishes — a \
                         leftover, not a pending unlock",
                        palette.muted,
                    )),
                Some(tip) => panel
                    .row(
                        "state",
                        Text::of(glyphs.warn, palette.warn).space().push(
                            format!(
                                "locked for {} more",
                                fmt::plural((height - tip) as usize, "block", "blocks")
                            ),
                            palette.warn,
                        ),
                    )
                    .note(Text::of(
                        "the countdown started when this was set and cannot be paused",
                        palette.muted,
                    )),
                // The height is known and whether it has passed is not. Saying
                // "locked" would be a guess about spendability.
                None => panel.row("state", Text::of("tip unknown", palette.muted)),
            }
        }
        Timelock::DelayAfterUnlock(blocks) => panel
            .section("TIMELOCK")
            .row(
                "unlock delay",
                Text::of(
                    fmt::plural(blocks as usize, "block", "blocks"),
                    palette.value,
                ),
            )
            .row(
                "state",
                Text::of(glyphs.warn, palette.warn)
                    .space()
                    .push("locked, and no unlock requested", palette.warn),
            )
            .note(Text::of(
                "the delay does not start until an unlock is asked for, so this is locked \
                 indefinitely rather than until some height",
                palette.muted,
            ))
            .note(Text::of(
                "only the revocation and recovery authorities can act on it while it is locked",
                palette.muted,
            )),
    }
}

/// Read an identity's timelock out of the daemon's JSON.
///
/// `timelock` is **either an absolute height or a relative delay**, and which
/// one it is depends on `FLAG_LOCKED`. The SDK makes this a type precisely so
/// the pairing cannot be got wrong; this is the one place that reconstructs it
/// from a rendering, and it follows `Timelock::of` exactly.
pub(crate) fn timelock_of(identity: &serde_json::Value) -> Timelock {
    let flags = identity.get("flags").and_then(|v| v.as_u64()).unwrap_or(0);
    let after = identity
        .get("timelock")
        .and_then(|v| v.as_u64())
        .and_then(|v| u32::try_from(v).ok())
        .unwrap_or(0);

    if flags & u64::from(FLAG_LOCKED) != 0 {
        Timelock::DelayAfterUnlock(after)
    } else if after != 0 {
        Timelock::UntilBlock(after)
    } else {
        Timelock::None
    }
}

/// The timelock as `id list` reports it: [`timelock_json`], with `spendable`
/// decided rather than left as a shape.
///
/// `spendable` answers one question — can the primary keys move this now — and
/// a consumer keying on it has to get the same answer the panel beside it
/// prints. Two things `id show` cannot settle for free, this command has in
/// hand:
///
/// * Revocation settles it whatever the timelock says. A revoked identity with
///   no timelock reported `"kind": "none", "spendable": true`, which is true
///   about the timelock and wrong about the identity.
/// * An `until_block` height is decided by the tip, and this run already
///   fetched one if any row needed it. Left `null` a consumer had to combine
///   the document's `tip` with `unlock_height` itself, on the one field the
///   notes point it at.
///
/// `null` survives for exactly one case: a height that needed a tip the node
/// would not give. That is an unknown, and it stays one.
fn list_timelock_json(timelock: Timelock, revoked: bool, tip: Tip) -> serde_json::Value {
    let mut value = timelock_json(timelock);
    let spendable = if revoked {
        Some(false)
    } else {
        match (timelock, tip) {
            (Timelock::None, _) => Some(true),
            (Timelock::DelayAfterUnlock(_), _) => Some(false),
            (Timelock::UntilBlock(height), Tip::Known(tip)) => Some(tip >= height),
            (Timelock::UntilBlock(_), Tip::NotNeeded | Tip::Unread) => None,
        }
    };
    value["spendable"] = match spendable {
        Some(spendable) => serde_json::Value::Bool(spendable),
        None => serde_json::Value::Null,
    };
    value
}

fn timelock_json(timelock: Timelock) -> serde_json::Value {
    match timelock {
        Timelock::None => serde_json::json!({ "kind": "none", "spendable": true }),
        Timelock::UntilBlock(height) => serde_json::json!({
            "kind": "until_block",
            "unlock_height": height,
            // Whether it is spendable *now* needs the tip, which the caller may
            // not have fetched. Absent rather than guessed.
            "spendable": null,
        }),
        Timelock::DelayAfterUnlock(blocks) => serde_json::json!({
            "kind": "delay_after_unlock",
            "delay_blocks": blocks,
            // Never spendable by this measure: no unlock has been requested, so
            // there is no height at which it opens.
            "spendable": false,
        }),
    }
}

fn choose_key(store: &Keystore, label: Option<&str>) -> Result<Envelope, miette::Report> {
    if let Some(label) = label {
        return Ok(store.load(label)?);
    }
    let keys = store.list()?;
    match keys.len() {
        0 => Err(IdError::NoKey.into()),
        1 => Ok(keys.into_iter().next().expect("just checked")),
        count => Err(IdError::AmbiguousKey { count }.into()),
    }
}

fn confirm(ui: &Ui) -> Result<(), IdError> {
    use std::io::{IsTerminal, Write};
    if !std::io::stdin().is_terminal() {
        return Err(IdError::CannotConfirm);
    }
    ui.blank();
    print!("  type `yes` to continue: ");
    let _ = std::io::stdout().flush();
    let mut answer = String::new();
    std::io::stdin()
        .read_line(&mut answer)
        .map_err(|_| IdError::CannotConfirm)?;
    if answer.trim() != "yes" {
        return Err(IdError::Cancelled);
    }
    Ok(())
}

fn emit(value: &serde_json::Value) {
    crate::failure::document(value);
}

#[cfg(test)]
mod tests {
    use unicode_width::UnicodeWidthStr;

    use super::*;
    use crate::cli::Theme as ThemeFlag;
    use crate::config::Paths;

    /// A real i-address and a real R-address, so the polarity test runs against
    /// the shapes a well-behaved daemon actually returns.
    const HONEST_IDENTITY: &str = "i7r29bDQfrwjkTxjv4bcYD6B1ZV7WZ4kGo";
    const HONEST_PRIMARY: &str = "RComfCn4wHHsGR8vWBAU7T1r3tHHyxN9Hm";

    /// Everything a node could put in one of these fields that a frame cannot
    /// survive: an escape run, an embedded row, and a delete.
    const HOSTILE: &str = "i\u{1b}[31m\nSPENDABLE  999.00000000\u{7f}";

    fn record(
        identity_address: &str,
        status: &str,
        primary: &str,
        authority: &str,
    ) -> IdentityRecord {
        IdentityRecord {
            fully_qualified_name: "alice@".into(),
            identity_address: identity_address.to_string(),
            status: status.to_string(),
            outpoint: (verus_sdk::money::Txid::from_internal([0u8; 32]), 0),
            block_height: 1_176_650,
            identity: serde_json::json!({
                "minimumsignatures": 1,
                "primaryaddresses": [primary],
                "revocationauthority": authority,
                "recoveryauthority": authority,
            }),
        }
    }

    /// The panel as `id show` renders it, with the escapes stripped so the
    /// assertions can read one row at a time.
    fn rendered(record: &IdentityRecord) -> String {
        let ui = Ui::new(ThemeFlag::Phosphor, false, false);
        crate::ui::text::strip_ansi(
            &panel(&ui, record, None, Timelock::None, None).render(&ui.theme),
        )
    }

    /// Four strings on this panel are the node's, and none of them was filtered
    /// before this. A daemon that answers with an escape run must not reach the
    /// terminal, and must not be able to forge a row inside the box.
    #[test]
    fn a_hostile_identity_record_cannot_break_the_identity_frame() {
        let out = rendered(&record(HOSTILE, HOSTILE, HOSTILE, HOSTILE));
        assert!(!out.contains('\u{1b}'), "escape survived:\n{out}");
        assert!(!out.contains('\u{7f}'), "delete survived:\n{out}");
        let widths: Vec<usize> = out
            .lines()
            .filter(|line| line.starts_with(['┌', '│', '├', '└']))
            .map(UnicodeWidthStr::width)
            .collect();
        assert!(!widths.is_empty(), "nothing was framed:\n{out}");
        assert!(
            widths.windows(2).all(|pair| pair[0] == pair[1]),
            "ragged frame {widths:?}:\n{out}"
        );
    }

    /// The polarity, and the one thing the filter could quietly break: the
    /// `(itself)` marker is decided by comparing the *raw* strings, so it has to
    /// survive a filter going in front of the printed one.
    #[test]
    fn an_honest_identity_still_prints_its_addresses_whole_and_still_says_itself() {
        let out = rendered(&record(
            HONEST_IDENTITY,
            "active",
            HONEST_PRIMARY,
            HONEST_IDENTITY,
        ));
        assert!(out.contains(HONEST_IDENTITY), "the i-address moved:\n{out}");
        assert!(
            out.contains(HONEST_PRIMARY),
            "a primary address moved:\n{out}"
        );
        assert!(out.contains("active"), "the status moved:\n{out}");
        assert!(
            out.contains("(itself)"),
            "an authority that is the identity stopped saying so:\n{out}"
        );
    }

    /// A `Settings` rooted at a temporary directory, returned with the guard so
    /// the directory outlives the files the test writes into it.
    fn settings() -> (tempfile::TempDir, Settings) {
        let dir = tempfile::tempdir().expect("a temp dir");
        let settings = Settings::resolve_in(Paths::at(dir.path()), None, None)
            .expect("no config file is the built-in profile");
        (dir, settings)
    }

    /// The six `Ok(())`s `ensure_exists` has to tell apart from a real reveal:
    /// `begin` under --json after the commitment and under --no-wait; `resume`
    /// on a commitment still `Waiting`, json and rendered; and `resume` on a
    /// `CommitmentGone` it re-broadcast, json and rendered. Every one of them
    /// leaves the reservation on disk, because the salt is still needed.
    ///
    /// --dry-run is *not* among them, and `reveal_was_broadcast` does not catch
    /// it: `begin` returns before `save_pending`, so nothing reaches disk, and
    /// the absent file reads as a finished registration — straight into the
    /// poll. That is why --dry-run is refused separately, before `register`
    /// runs at all.
    #[test]
    fn a_reservation_that_outlived_the_registration_means_no_reveal_was_sent() {
        let (_dir, settings) = settings();
        std::fs::create_dir_all(settings.paths.pending_dir()).expect("writable temp dir");
        let path = pending_path(&settings, "mybasket");
        std::fs::write(&path, "{}").expect("writable temp dir");

        assert!(!reveal_was_broadcast(&pending_path(&settings, "mybasket")));
    }

    /// The polarity, which is the one way this guard could go wrong invisibly:
    /// inverted, it lets the twenty-minute wait straight back in and every
    /// other test still passes.
    #[test]
    fn a_completed_registration_leaves_no_reservation_to_find() {
        let (_dir, settings) = settings();
        std::fs::create_dir_all(settings.paths.pending_dir()).expect("writable temp dir");

        assert!(reveal_was_broadcast(&pending_path(&settings, "mybasket")));
    }

    /// `pending_path` lowercases and `ensure_exists` hands it the caller's name
    /// verbatim, so a mixed-case `--register` name that stopped at the
    /// commitment has to be caught rather than sent to the poll.
    #[test]
    fn a_reservation_is_found_whatever_case_the_name_was_typed_in() {
        let (_dir, settings) = settings();
        std::fs::create_dir_all(settings.paths.pending_dir()).expect("writable temp dir");
        std::fs::write(pending_path(&settings, "mybasket"), "{}").expect("writable temp dir");

        assert!(!reveal_was_broadcast(&pending_path(&settings, "MyBasket")));
    }

    // ── id list ─────────────────────────────────────────────────────────────

    /// VRSCTEST's own i-address, which is the `systemid` on every identity
    /// registered on that chain.
    const SYSTEM: &str = "iJhCezBExJHvtyH3fGhNnt2NhU4Ztkf2yq";

    /// `Kaiju.VRSCTEST@` — a real VRSCTEST currency with identities under it,
    /// and the reason a sub-identity's name cannot be built by appending `@`.
    const KAIJU: &str = "iHBwQo7LUmb7QKKqbsd8Kw9BxdQvgTdK9f";

    /// Two more real i-addresses, so a multi-row table is measured over the
    /// shapes a daemon actually returns.
    const OTHER_IDENTITY: &str = "i7kDJurgpZA63cjPTuyK49CeCKihB5ryDB";

    fn at_address(
        name: &str,
        identity_address: &str,
        parent: &str,
        system: &str,
        flags: u32,
        timelock: u32,
    ) -> IdentityAtAddress {
        IdentityAtAddress {
            identity_address: identity_address.to_string(),
            name: name.to_string(),
            parent: parent.to_string(),
            flags,
            timelock,
            outpoint: (verus_sdk::money::Txid::from_internal([0u8; 32]), 0),
            // The whole object, the way `into_typed` rebuilds it: the lifted
            // fields reinserted alongside everything else the daemon sent.
            identity: serde_json::json!({
                "identityaddress": identity_address,
                "name": name,
                "parent": parent,
                "systemid": system,
                "flags": flags,
                "timelock": timelock,
                // Carried because the wire carries them on every entry, and
                // the `signers` column reads them. A fixture that left them out
                // would test the unknown arm and call it the ordinary case.
                "minimumsignatures": 1,
                "primaryaddresses": [HONEST_PRIMARY],
            }),
        }
    }

    /// The same, with a threshold no single key can meet.
    ///
    /// The row the whole `signers` column exists for: the queried address is
    /// one of three primaries and two of them have to sign, so finding this
    /// identity does not mean the key that found it can move it.
    fn shared(
        name: &str,
        identity_address: &str,
        min_sigs: u64,
        primaries: usize,
    ) -> IdentityAtAddress {
        let mut entry = at_address(name, identity_address, SYSTEM, SYSTEM, 0, 0);
        entry.identity["minimumsignatures"] = serde_json::json!(min_sigs);
        entry.identity["primaryaddresses"] =
            serde_json::Value::Array(vec![serde_json::json!(HONEST_PRIMARY); primaries]);
        entry
    }

    /// An entry whose reply carried neither field, which is the only honest
    /// source of an `unknown` in the column.
    fn threshold_unsaid(name: &str, identity_address: &str) -> IdentityAtAddress {
        let mut entry = at_address(name, identity_address, SYSTEM, SYSTEM, 0, 0);
        let object = entry.identity.as_object_mut().expect("an identity object");
        object.remove("minimumsignatures");
        object.remove("primaryaddresses");
        entry
    }

    /// A plain top-level identity on VRSCTEST.
    fn top_level(name: &str, identity_address: &str) -> IdentityAtAddress {
        at_address(name, identity_address, SYSTEM, SYSTEM, 0, 0)
    }

    fn currency(i_address: &str) -> CurrencyId {
        CurrencyId::from_bytes(
            i_address
                .parse::<Address>()
                .expect("a valid i-address")
                .hash(),
        )
    }

    fn listing(rows: Vec<Row>, tip: Tip) -> Listing {
        Listing {
            address: HONEST_PRIMARY.to_string(),
            rows,
            tip,
        }
    }

    /// The panel as `id list` renders it at `terminal` columns.
    fn list_rendered(listing: &Listing, terminal: usize) -> String {
        let ui = framed_id(terminal);
        list_panel(&ui, listing).render(&ui.theme)
    }

    /// The framed skin at `terminal` columns — the one with a border to run out
    /// through. `Ui::new` alone picks a theme off the real stdout, which in a
    /// test harness is a pipe and therefore unframed.
    fn framed_id(terminal: usize) -> Ui {
        let mut ui = Ui::new(ThemeFlag::Phosphor, false, false);
        ui.theme = crate::ui::theme::Theme::with_skin(crate::ui::theme::Skin::Phosphor, terminal);
        ui
    }

    fn name_text(name: &Name) -> String {
        crate::ui::text::strip_ansi(&name_cell(&framed_id(120), name).render())
    }

    /// The whole design decision behind this command, pinned where it is made.
    ///
    /// An identity whose parent is the chain it lives on is fully named by
    /// `name@`, which `id show`, `id update` and `send --to` all accept — so
    /// the list is actionable for the twelve of twelve sample identities in
    /// #48 without spending a single extra request. `systemid` is what makes
    /// that free; it is in the reply already.
    #[test]
    fn a_top_level_identity_is_named_without_asking_the_node_anything() {
        let entries = [top_level("pecucli7", HONEST_IDENTITY)];

        // No parent names at all — the map a node that was never asked leaves.
        let rows = build_rows(&entries, &BTreeMap::new());

        assert!(
            matches!(&rows[0].name, Name::Usable(name) if name == "pecucli7@"),
            "{}",
            name_text(&rows[0].name)
        );
    }

    /// The other half of it, and the reason `format!("{name}@")` is not the
    /// whole implementation.
    ///
    /// `crypto.Kaiju.VRSCTEST@`'s name component is `crypto`. `crypto@` is
    /// refused by the chain today only because nobody has registered a
    /// top-level `crypto` — and the day somebody does, a list that appended
    /// `@` would print a name resolving to a different person's identity.
    /// The parent's **fully qualified** name is what makes the answer mean one
    /// thing: built from the unqualified `Kaiju` this would read
    /// `crypto.Kaiju@`, which resolves only because `Kaiju` happens to sit at
    /// the top of this chain.
    #[test]
    fn a_sub_identity_is_qualified_through_its_parent_rather_than_by_appending_an_at_sign() {
        let entries = [at_address("crypto", HONEST_IDENTITY, KAIJU, SYSTEM, 5, 0)];
        let named = BTreeMap::from([(
            currency(KAIJU),
            CurrencyName::Known("Kaiju.VRSCTEST".to_string()),
        )]);

        let rows = build_rows(&entries, &named);

        assert!(
            matches!(&rows[0].name, Name::Usable(name) if name == "crypto.Kaiju.VRSCTEST@"),
            "{}",
            name_text(&rows[0].name)
        );
    }

    /// One request per *distinct* parent, and none for a top-level identity.
    ///
    /// The deduplication is what makes the worst case bearable: nine
    /// identities under one parent are one `getcurrency`, not nine. It comes
    /// free from `look_up_qualified_names` taking a set, which is exactly why
    /// it is worth a test — nothing in `rows` looks like it is doing it.
    #[test]
    fn parents_are_asked_about_once_each_and_top_level_identities_not_at_all() {
        let entries = [
            top_level("pecucli7", HONEST_IDENTITY),
            at_address("crypto", OTHER_IDENTITY, KAIJU, SYSTEM, 5, 0),
            at_address("mobile", HONEST_IDENTITY, KAIJU, SYSTEM, 0, 0),
        ];

        let wanted: BTreeSet<CurrencyId> = entries
            .iter()
            .filter_map(|entry| match parentage(entry) {
                Parentage::Under(parent) => Some(parent),
                Parentage::TopLevel | Parentage::Unsaid => None,
            })
            .collect();

        assert_eq!(wanted, BTreeSet::from([currency(KAIJU)]), "{wanted:?}");
    }

    /// The chain's own identity, whose `parent` is the chain it was launched
    /// *from*: `VRSCTEST@`'s parent is the VRSC mainnet root, which this node
    /// has no currency for. Named off `identityaddress == systemid` instead, or
    /// the one identity on every panel in this repo prints as nameless.
    #[test]
    fn the_chains_own_identity_is_named_even_though_its_parent_is_another_chain() {
        let entries = [at_address(
            "VRSCTEST",
            SYSTEM,
            "i3UXS5QPRQGNRDDqVnyWTnmFCTHDbzmsYk",
            SYSTEM,
            1,
            0,
        )];

        let rows = build_rows(&entries, &BTreeMap::new());

        assert!(
            matches!(&rows[0].name, Name::Usable(name) if name == "VRSCTEST@"),
            "{}",
            name_text(&rows[0].name)
        );
    }

    /// Three ways a parent goes unnamed, and none of them may print a name.
    ///
    /// A lookup that failed, a deadline that never reached it, and a node that
    /// denies the parent exists are different facts, and the one thing they
    /// have in common is that no name was got. Printing the bare component —
    /// `crypto`, or `crypto@` — would hand the reader a string that looks like
    /// a name, is not one, and in the `@` case may name somebody else.
    #[test]
    fn a_parent_that_was_not_named_leaves_the_row_nameless_rather_than_bare() {
        let entries = [at_address("crypto", HONEST_IDENTITY, KAIJU, SYSTEM, 0, 0)];
        let unnamed = [
            (
                BTreeMap::from([(
                    currency(KAIJU),
                    CurrencyName::Failed("connection refused".to_string()),
                )]),
                "(name unknown)",
            ),
            // No entry at all: the shared deadline ran out first.
            (BTreeMap::new(), "(name unknown)"),
            (
                BTreeMap::from([(currency(KAIJU), CurrencyName::Absent)]),
                "(no such parent)",
            ),
        ];

        for (named, expected) in unnamed {
            let rows = build_rows(&entries, &named);
            let cell = name_text(&rows[0].name);
            assert_eq!(cell, expected, "{named:?}");
            assert!(
                !cell.contains("crypto"),
                "a bare name component leaked: {cell}"
            );
        }
    }

    /// And the row stays usable: the i-address is whole, and the panel says
    /// which handle to reach for. A nameless row a reader cannot act on is a
    /// row they read as broken.
    #[test]
    fn a_nameless_row_keeps_a_whole_i_address_and_says_to_use_it() {
        let entries = [at_address("crypto", HONEST_IDENTITY, KAIJU, SYSTEM, 0, 0)];
        let rows = build_rows(&entries, &BTreeMap::new());

        let out = crate::ui::text::strip_ansi(&list_rendered(&listing(rows, Tip::NotNeeded), 120));

        assert!(
            out.contains(HONEST_IDENTITY),
            "the i-address was cut:\n{out}"
        );
        assert!(out.contains("`pecu id show`"), "no handle named:\n{out}");
    }

    /// A timelock whose height may already have passed is not a locked
    /// identity, and without the tip nothing here knows which it is.
    #[test]
    fn a_timelock_the_tip_cannot_settle_is_not_reported_as_locked() {
        let entries = [at_address(
            "alice",
            HONEST_IDENTITY,
            SYSTEM,
            SYSTEM,
            0,
            1_100_000,
        )];
        let rows = build_rows(&entries, &BTreeMap::new());
        assert!(
            matches!(rows[0].timelock, Timelock::UntilBlock(1_100_000)),
            "{:?}",
            rows[0].timelock
        );

        let unread = crate::ui::text::strip_ansi(&list_rendered(&listing(rows, Tip::Unread), 120));
        assert!(unread.contains("timelocked"), "{unread}");
        assert!(
            unread.contains("the chain tip could not be read"),
            "no note said the word was hedged:\n{unread}"
        );

        // With a tip past the height it is an ordinary identity: the height
        // stays on it forever once the countdown finishes, so `locked` here
        // would be wrong about something that unlocked long ago.
        let rows = build_rows(&entries, &BTreeMap::new());
        let elapsed =
            crate::ui::text::strip_ansi(&list_rendered(&listing(rows, Tip::Known(1_200_000)), 120));
        assert!(!elapsed.contains("locked"), "{elapsed}");
        assert!(elapsed.contains("active"), "{elapsed}");
    }

    /// Read off the flag, because this reply carries no status string for it.
    #[test]
    fn a_revoked_identity_says_so_in_the_list() {
        let entries = [at_address(
            "alice",
            HONEST_IDENTITY,
            SYSTEM,
            SYSTEM,
            verus_sdk::verus_tx::identity::FLAG_REVOKED,
            0,
        )];
        let rows = build_rows(&entries, &BTreeMap::new());

        let out = crate::ui::text::strip_ansi(&list_rendered(&listing(rows, Tip::NotNeeded), 120));

        assert!(out.contains("revoked"), "{out}");
    }

    /// A name is display text a registrant chose, and this table prints it
    /// inside a frame. A newline in one forges a row; an escape run repaints
    /// the terminal from inside the box.
    #[test]
    fn a_hostile_identity_name_cannot_forge_a_row_in_the_list() {
        let entries = [
            at_address(HOSTILE, HOSTILE, SYSTEM, SYSTEM, 0, 0),
            top_level("alice", HONEST_IDENTITY),
        ];
        for terminal in 48..=120 {
            let out = crate::ui::text::strip_ansi(&list_rendered(
                &listing(build_rows(&entries, &BTreeMap::new()), Tip::NotNeeded),
                terminal,
            ));
            assert!(
                !out.contains('\u{1b}'),
                "escape survived at {terminal}:\n{out}"
            );
            assert!(
                !out.contains('\u{7f}'),
                "delete survived at {terminal}:\n{out}"
            );
            assert_list_square(&out, terminal);
        }

        // The polarity, at a width with room for both: the frame is square
        // because the hostile cells were filtered, not because the row beside
        // them was swallowed.
        let wide = crate::ui::text::strip_ansi(&list_rendered(
            &listing(build_rows(&entries, &BTreeMap::new()), Tip::NotNeeded),
            120,
        ));
        assert!(wide.contains("alice@"), "the honest row vanished:\n{wide}");
        assert!(
            wide.contains(HONEST_IDENTITY),
            "a hostile neighbour cut an honest i-address:\n{wide}"
        );
    }

    /// #53 on the one row of this panel that does not come from the node.
    ///
    /// On the `--key` path the address is the keystore's word, and a keystore
    /// is a file that can be edited or corrupted. Rendered unfiltered, a
    /// newline in it forged a row inside the IDENTITIES frame and left the box
    /// ragged. `--address` is safe — base58check settles it offline — so this
    /// is the vector that is left.
    #[test]
    fn a_tampered_keystore_address_cannot_forge_a_row_in_the_list() {
        let entries = [top_level("alice", HONEST_IDENTITY)];
        let listing = Listing {
            address: format!("{HONEST_PRIMARY}\n\u{2502} forged  {HONEST_IDENTITY}  ok active"),
            rows: build_rows(&entries, &BTreeMap::new()),
            tip: Tip::NotNeeded,
        };

        for terminal in 48..=120 {
            let out = crate::ui::text::strip_ansi(&list_rendered(&listing, terminal));
            assert!(
                !out.contains("forged"),
                "a forged row at {terminal}:\n{out}"
            );
            assert_list_square(&out, terminal);
        }
    }

    /// #50's budget, over every width the theme can reach.
    #[test]
    fn the_identity_list_frame_stays_square_at_every_width_the_theme_can_reach() {
        let entries = [
            top_level("a", HONEST_IDENTITY),
            top_level("pecu-demo-id", OTHER_IDENTITY),
            // Far longer than anything the budget prints, so the table is over
            // its frame at every width and the elision queue is exercised.
            top_level(&"n".repeat(96), "iKh6DBXjPVU72BBD4sq5qbdFFeQGVcYokg"),
            at_address(
                "crypto",
                "i9dpvtcsH6FRD4UmNVur75cLXj7rUx9iD1",
                KAIJU,
                SYSTEM,
                0,
                0,
            ),
        ];

        for terminal in 48..=120 {
            let rows = build_rows(&entries, &BTreeMap::new());
            let out = crate::ui::text::strip_ansi(&list_rendered(
                &listing(rows, Tip::NotNeeded),
                terminal,
            ));
            assert_list_square(&out, terminal);
        }
    }

    /// The elision queue, pinned where it is decided.
    ///
    /// Queued id-first, one ninety-six-character name drained the i-address
    /// column to the width of the word I-ADDRESS for *every* row — column
    /// widths are the maximum across rows, which is what makes one long name
    /// everybody's problem. The i-address is the handle a nameless row has
    /// left, and the one the SDK itself steers a destructive follow-up at.
    #[test]
    fn a_long_name_pays_for_the_frame_rather_than_the_i_addresses() {
        let entries = [
            top_level("alice", HONEST_IDENTITY),
            top_level(&"n".repeat(96), OTHER_IDENTITY),
        ];

        for terminal in 80..=120 {
            let rows = build_rows(&entries, &BTreeMap::new());
            let out = crate::ui::text::strip_ansi(&list_rendered(
                &listing(rows, Tip::NotNeeded),
                terminal,
            ));
            assert!(
                out.contains(HONEST_IDENTITY),
                "a neighbour's long name cut alice's i-address at {terminal} columns:\n{out}"
            );
        }
    }

    fn assert_list_square(rendered: &str, at: usize) {
        let widths: Vec<usize> = rendered
            .lines()
            .filter(|line| line.starts_with(['┌', '│', '├', '└']))
            .map(UnicodeWidthStr::width)
            .collect();
        assert!(!widths.is_empty(), "nothing was framed at {at}");
        assert!(
            widths.windows(2).all(|pair| pair[0] == pair[1]),
            "ragged frame at {at} columns, widths {widths:?}:\n{rendered}"
        );
    }

    /// The `--json` grammar for a name, which has three answers and not two.
    #[test]
    fn the_json_never_says_a_nameless_identity_has_no_name() {
        assert_eq!(
            qualified_json(&Name::Usable("pecucli7@".into())),
            serde_json::json!({ "known": true, "name": "pecucli7@" })
        );
        // `known: false` — nothing was learned, so a consumer must not read
        // this as an identity the chain declines to name.
        assert_eq!(
            qualified_json(&Name::Unknown("connection refused".into()))["known"],
            serde_json::json!(false)
        );
        // The one case that really is the chain answering, and even then the
        // reason is carried rather than left as a bare `null`.
        let absent = qualified_json(&Name::NoParent);
        assert_eq!(absent["known"], serde_json::json!(true));
        assert_eq!(absent["name"], serde_json::Value::Null);
        assert!(absent["reason"].is_string(), "{absent}");
    }

    /// The promise the whole command rests on, at the length that broke it.
    ///
    /// A name over the budget used to come back cut from the middle and still
    /// ending in `@` — `aaaa…aaaa@`, which wears the one mark that says "this
    /// is a whole VerusID name" and is not one. Verus permits 64-byte name
    /// components, so this is a name a chain really carries. Now the cut takes
    /// the `@` with it and the cell ends in the ellipsis.
    #[test]
    fn a_name_too_long_for_the_column_does_not_come_out_looking_like_a_whole_one() {
        let long = "a".repeat(60);
        let entries = [top_level(&long, HONEST_IDENTITY)];
        let rows = build_rows(&entries, &BTreeMap::new());

        let cell = name_text(&rows[0].name);

        assert!(
            !cell.ends_with('@'),
            "a cut name still ends in the mark of a whole one: {cell}"
        );
        assert!(cell.ends_with('…'), "the cut is not visible: {cell}");
        // And the leaf component is what survives, so the row still says which
        // identity it is.
        assert!(cell.starts_with("aaaa"), "{cell}");
        // `--json` is where the whole name lives, and it is whole there.
        assert_eq!(
            qualified_json(&rows[0].name)["name"],
            serde_json::json!(format!("{long}@"))
        );
    }

    /// A name a reader cannot copy is a name the panel has to own up to.
    ///
    /// Two ways it happens and both are silent without this: the budget above,
    /// and the frame taking the column down to fit. At 48 columns every name in
    /// this table elides to four characters, and the reader is left staring at
    /// the column they came for with nothing saying it is no longer the answer.
    #[test]
    fn a_panel_that_shortened_a_name_says_so_and_says_where_the_whole_one_is() {
        let entries = [
            top_level("pecucli7", HONEST_IDENTITY),
            top_level("pecu-demo-id", OTHER_IDENTITY),
        ];
        let rows = || build_rows(&entries, &BTreeMap::new());

        // Room for everything: nothing was cut, so nothing is claimed.
        let roomy =
            crate::ui::text::strip_ansi(&list_rendered(&listing(rows(), Tip::NotNeeded), 120));
        assert!(roomy.contains("pecucli7@"), "{roomy}");
        assert!(!roomy.contains("carries every name whole"), "{roomy}");

        // The frame takes the column down, and says so.
        let tight =
            crate::ui::text::strip_ansi(&list_rendered(&listing(rows(), Tip::NotNeeded), 48));
        assert!(
            !tight.contains("pecucli7@"),
            "nothing was cut at 48:\n{tight}"
        );
        assert!(tight.contains("carries every name whole"), "{tight}");

        // The budget cuts one on its own, at a width with room to spare.
        let long = [top_level(&"a".repeat(60), HONEST_IDENTITY)];
        let over = crate::ui::text::strip_ansi(&list_rendered(
            &listing(build_rows(&long, &BTreeMap::new()), Tip::NotNeeded),
            120,
        ));
        assert!(over.contains("carries every name whole"), "{over}");
    }

    /// The question a reader of this list is actually asking.
    ///
    /// They ran it holding a key and no name. "Which identities does this key
    /// control" has a second half — whether the key is enough on its own — and
    /// `minimumsignatures` and `primaryaddresses` answer it in the same reply,
    /// at no extra request. Dropped, a 2-of-3 identity rendered exactly like
    /// one the key alone can move.
    #[test]
    fn an_identity_the_key_cannot_move_alone_does_not_render_like_one_it_can() {
        let entries = [
            top_level("pecucli7", HONEST_IDENTITY),
            shared("sharedvault", OTHER_IDENTITY, 2, 3),
        ];
        let rows = build_rows(&entries, &BTreeMap::new());

        assert!(rows[0].control.moves_alone());
        assert!(!rows[1].control.moves_alone());

        let out = crate::ui::text::strip_ansi(&list_rendered(&listing(rows, Tip::NotNeeded), 120));

        assert!(out.contains("1-of-1"), "{out}");
        assert!(out.contains("2-of-3"), "{out}");
        // And the column is explained, once, on the run where it matters.
        assert!(out.contains("not enough on its own"), "{out}");
    }

    /// The threshold and the number of primaries are two different facts.
    ///
    /// `monkins@` on VRSCTEST is `1-of-2`: two primary addresses, one required,
    /// so the queried key moves it on its own and so does the other. Read as
    /// "anything but `1-of-1` means you cannot move this", the panel would warn
    /// about an identity this key controls outright — the opposite of the
    /// mistake `2-of-3` was added to stop.
    #[test]
    fn one_signature_out_of_two_is_still_a_key_that_moves_the_identity_alone() {
        let entries = [shared("monkins", HONEST_IDENTITY, 1, 2)];
        let rows = build_rows(&entries, &BTreeMap::new());

        assert!(rows[0].control.moves_alone());
        // Still not the ordinary row, so the column is still explained.
        assert!(!rows[0].control.is_plain());

        let out = crate::ui::text::strip_ansi(&list_rendered(&listing(rows, Tip::NotNeeded), 120));

        assert!(out.contains("1-of-2"), "{out}");
        assert!(out.contains("so is another key"), "{out}");
    }

    /// A reply that did not carry the threshold is an unknown, not a `1-of-1`.
    ///
    /// `1-of-1` is the answer somebody acts on. Defaulting to it would be this
    /// program's guess printed in the column whose whole job is saying whether
    /// the key in hand is enough.
    #[test]
    fn a_reply_that_did_not_say_the_threshold_is_not_a_key_that_signs_alone() {
        let entries = [threshold_unsaid("quiet", HONEST_IDENTITY)];
        let rows = build_rows(&entries, &BTreeMap::new());

        assert_eq!(
            rows[0].control,
            Control {
                min_sigs: None,
                primaries: None
            }
        );
        assert!(!rows[0].control.moves_alone());
        assert!(!rows[0].control.is_plain());

        let out = crate::ui::text::strip_ansi(&list_rendered(&listing(rows, Tip::NotNeeded), 120));

        let row = out
            .lines()
            .find(|line| line.contains("quiet@"))
            .expect("the identity's row");
        assert!(row.contains("unknown"), "{out}");
        assert!(!row.contains("1-of-1"), "a guess was printed:\n{out}");
    }

    /// `--json`'s `spendable` answers one question, and it has to be the same
    /// answer the panel prints beside it.
    #[test]
    fn the_json_says_whether_the_primary_keys_can_move_it_rather_than_describing_the_timelock() {
        // Revoked settles it whatever the timelock says. `kind: none` used to
        // carry `spendable: true` for an identity nobody's primary keys move.
        assert_eq!(
            list_timelock_json(Timelock::None, true, Tip::NotNeeded)["spendable"],
            serde_json::json!(false)
        );
        // A height already passed is spendable, which is what the panel decided
        // off the same tip. `null` here left the consumer to redo the
        // comparison on the one field the notes point it at.
        assert_eq!(
            list_timelock_json(Timelock::UntilBlock(1_000), false, Tip::Known(900_000))
                ["spendable"],
            serde_json::json!(true)
        );
        assert_eq!(
            list_timelock_json(Timelock::UntilBlock(2_000_000), false, Tip::Known(900_000))
                ["spendable"],
            serde_json::json!(false)
        );
        // And the one unknown that survives: a height nothing could be measured
        // against, because the node would not give a tip.
        assert_eq!(
            list_timelock_json(Timelock::UntilBlock(1_000), false, Tip::Unread)["spendable"],
            serde_json::Value::Null
        );
    }

    /// `id register` burns 100 VRSCTEST across two transactions, so the advice
    /// that invites a blind resend is the expensive one to get wrong.
    #[test]
    fn an_uncertain_registration_broadcast_does_not_blame_the_node() {
        // The advice saves the signed bytes, so it needs somewhere that is not the
        // real keystore root to save them into.
        let _unsent = crate::cmd::UnsentRoot::temporary();
        let refused = flow(
            "broadcasting the commitment",
            FlowError::BroadcastUncertain {
                txid: "9c1d55".into(),
                hex: "0400008085202f89".into(),
                reason: "node returned error -25: bad-txns-failed-precheck".into(),
            },
        );
        let IdError::Flow { advice, .. } = refused else {
            panic!("a flow refusal is an IdError::Flow");
        };
        assert!(advice.contains("tx explain 9c1d55"));
        assert!(!advice.contains("doctor"));
    }
}
