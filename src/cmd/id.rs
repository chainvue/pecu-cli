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

    if ui.is_json() {
        emit(&serde_json::json!({
            "name": record.fully_qualified_name,
            "identity_address": record.identity_address,
            "status": record.status,
            "revoked": record.is_revoked(),
            "block_height": record.block_height,
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
        .row(
            "registered",
            Text::of(
                format!("block {}", fmt::height(record.block_height.into())),
                palette.value,
            ),
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
    for (label, field) in [
        ("revocation", "revocationauthority"),
        ("recovery", "recoveryauthority"),
    ] {
        if let Some(authority) = identity.get(field).and_then(|v| v.as_str()) {
            let mut row = Text::of(authority, palette.value);
            // An identity that is its own revocation authority cannot be
            // revoked by anyone else — including you, if you lose the keys.
            if authority == record.identity_address {
                row = row.push("  (itself)", palette.warn);
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
        // Not a detail to discover later. A fresh identity is its own
        // revocation and recovery authority, which makes it unrevokable and
        // unrecoverable, and the SDK is explicit that pointing them elsewhere
        // is a decision at registration time rather than a later refinement.
        // `RegistrationOptions` has no field for it, so this build cannot offer
        // the choice -- but it can refuse to let it pass unmentioned.
        .note(Text::of(
            "this identity will be its own revocation and recovery authority: it cannot be \
             revoked or recovered afterwards, and that cannot be changed later",
            palette.warn,
        ))
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
