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

use std::path::{Path, PathBuf};

use miette::Diagnostic;
use thiserror::Error;
use verus_sdk::money::Amount;
use verus_sdk::network::{
    prepare_registration, AwaitingCommitment, ChainReader, CommitmentStatus, FlowError, Pending,
    RegistrationOptions, WaitPolicy,
};
use verus_sdk::verus_keys::{Address, AddressKind};
use verus_sdk::verus_tx::{Timelock, FLAG_LOCKED};

use crate::cli::{Globals, IdRegisterArgs};
use crate::config::Settings;
use crate::keystore::{self, Envelope, Keystore};
use crate::node::{self, Node};
use crate::ui::{fmt, Panel, Text, Ui};

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
    IdError::Flow {
        what,
        advice: "run `pecu doctor`, or point somewhere else with --node".to_string(),
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
            .push(&record.status, palette.value)
    };

    let mut panel = Panel::new("IDENTITY")
        .row(
            "name",
            Text::of(
                fmt::untrusted(&record.fully_qualified_name, NAME_BUDGET, glyphs.ellipsis),
                palette.accent,
            ),
        )
        .row(
            "i-address",
            Text::of(&record.identity_address, palette.value),
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
                .push(address, palette.value),
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
            let mut row = Text::of(authority, palette.value);
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

    ui.panel(&panel);
    ui.explain_panel();
    Ok(())
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
        pin_fee: None,
    };

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
    println!(
        "{}",
        serde_json::to_string_pretty(value).expect("plain data")
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Paths;

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
}
