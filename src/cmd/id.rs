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

use std::path::PathBuf;

use miette::Diagnostic;
use thiserror::Error;
use verus_sdk::network::{
    prepare_registration, AwaitingCommitment, ChainReader, CommitmentStatus, FlowError, Pending,
    RegistrationOptions,
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

    #[error("the node has never seen the commitment for `{name}`")]
    #[diagnostic(
        code(pecu::commitment_gone),
        help("it may not have propagated, or it may have been dropped. The saved registration is still at {}", path.display())
    )]
    CommitmentGone { name: String, path: PathBuf },

    #[error("cancelled")]
    #[diagnostic(code(pecu::cancelled), help("nothing was broadcast"))]
    Cancelled,

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
    let record = node.identity(name).map_err(|_| IdError::NotFound {
        name: name.to_string(),
    })?;
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

    match load_pending(&path)? {
        Some(pending) => resume(ui, settings, globals, &node, pending, &path),
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
        if !globals.yes {
            confirm(ui)?;
        }
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
                "run the same command again once the commitment confirms — a block or so",
                palette.muted,
            )),
    );
    Ok(())
}

/// Step two: wait for the confirmation, then reveal and pay.
fn resume(
    ui: &Ui,
    settings: &Settings,
    globals: &Globals,
    node: &Node,
    pending: Pending<AwaitingCommitment>,
    path: &PathBuf,
) -> miette::Result<()> {
    let palette = ui.theme.palette;
    let name = pending.name().to_string();

    ui.sdk("pending.poll(&node)");
    let status = pending
        .poll(node)
        .map_err(|source| flow("checking the commitment", source))?;

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
                        "run the same command again in a minute",
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
        CommitmentStatus::CommitmentGone => {
            return Err(IdError::CommitmentGone {
                name,
                path: path.clone(),
            }
            .into())
        }
    };

    let store = Keystore::new(&settings.paths);
    let envelope = choose_key(&store, None)?;
    let secret = keystore::passphrase(&format!("passphrase for `{}`", envelope.label), false)?;
    let key = envelope.unlock(&secret)?;

    if !ui.is_json() && !globals.yes {
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
            "fee_paid": registered.fee_paid.to_sat(),
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
                Text::of(fmt::amount(registered.fee_paid), palette.value)
                    .space()
                    .push(&settings.profile.currency, palette.muted),
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
        .row(
            "fee",
            Text::of(fmt::amount(pending.registration_fee), palette.accent)
                .space()
                .push(currency, palette.muted)
                .push("  burned, not recoverable", palette.warn),
        )
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
        panel = panel.row("referral", Text::of(referral, palette.value));
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

/// The TIMELOCK section, when there is one to show.
///
/// Omitted entirely for an unlocked identity: a row saying "not locked" on
/// every identity is noise, and this section appearing at all is the signal.
fn timelock_panel(ui: &Ui, panel: Panel, timelock: Timelock, tip: Option<u32>) -> Panel {
    let palette = ui.theme.palette;
    let glyphs = ui.theme.glyphs;
    match timelock {
        Timelock::None => panel,
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
fn timelock_of(identity: &serde_json::Value) -> Timelock {
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
