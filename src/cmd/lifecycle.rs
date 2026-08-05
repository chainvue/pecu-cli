//! `pecu id update` · `revoke` · `recover` — changing an identity after it exists.
//!
//! # Who may change what
//!
//! The identity output's condition is `1-of-3`, and consensus validates the
//! three branches **independently**, each guarding its own fields:
//!
//! | changing | needs |
//! |---|---|
//! | `primary_addresses`, `min_sigs` | the primary condition |
//! | `revocation_authority` | the revocation condition |
//! | `recovery_authority` | the recovery condition |
//!
//! A freshly registered identity is all three at once, so its own primary keys
//! satisfy every branch and can point either authority elsewhere. Once an
//! authority names **another** identity, those keys can no longer move it, and
//! there is no way to take it back. That is the direction with no undo.
//!
//! # The rule people get wrong
//!
//! An identity that is its **own recovery authority cannot be revoked at all**.
//! Consensus refuses it, because nobody could undo it afterwards. The trigger is
//! recovery, not revocation: an identity may revoke itself perfectly well as
//! long as somebody else can recover it. The SDK refuses this before a signature
//! exists, as `TxError::RevocationWouldStrand`.
//!
//! # Why every failure here is worth catching locally
//!
//! Consensus does not say which condition went unsatisfied. A revocation signed
//! by the wrong authority comes back as:
//!
//! ```text
//! -26: 16: mandatory-script-verify-flag-failed
//! ```
//!
//! after the fee is spent, naming nothing. So the flows check the named
//! authority's primary addresses and threshold before signing, and this module
//! surfaces that check rather than the daemon's silence.
//!
//! That pre-check is **advisory whenever the authority is a different
//! identity**, because every fact in it comes from the node. A lying node can
//! fail a valid revocation or pass an invalid one. When the identity is still
//! its own authority the check is offline — decoded from the output script — and
//! that is the common case.

use miette::Diagnostic;
use thiserror::Error;
use verus_sdk::identity::{Timelock, MAX_UNLOCK_DELAY};
use verus_sdk::network::{
    current_identity, prepare_identity_recovery, prepare_identity_revocation,
    prepare_identity_unlock, prepare_identity_update, FlowError, Held, IdentityChange, Unsent,
};
use verus_sdk::verus_keys::{Address, AddressKind, PrivateKey};
use verus_sdk::verus_tx::Destination;

use crate::cli::{Globals, IdAuthorityArgs, IdRecoverArgs, IdUnlockArgs, IdUpdateArgs};
use crate::config::Settings;
use crate::keystore::{self, Envelope, Keystore};
use crate::node::{self, Node};
use crate::ui::{fmt, Panel, Text, Ui};

/// How much of an identity name is ever printed.
const NAME_BUDGET: usize = 40;

#[derive(Debug, Error, Diagnostic)]
pub enum LifecycleError {
    #[error("no key to sign with")]
    #[diagnostic(
        code(pecu::no_key),
        help("pass --from <label>, or make a key with `pecu key gen`")
    )]
    NoKey,

    #[error("the keystore holds {count} keys, so there is no obvious one to sign with")]
    #[diagnostic(
        code(pecu::ambiguous_key),
        help("name one with --from <label>; `pecu key list` shows them")
    )]
    AmbiguousKey { count: usize },

    #[error("the `{profile}` profile is not allowed to spend")]
    #[diagnostic(
        code(pecu::spending_disabled),
        help("this rewrites an identity on chain and pays a miner fee. Set `allow_spend = true` under [profiles.{profile}] in config.toml")
    )]
    SpendingDisabled { profile: String },

    #[error("that update changes nothing")]
    #[diagnostic(
        code(pecu::empty_update),
        help("name at least one field: --primary, --min-sigs, --revocation, --recovery. Publishing an identity unchanged still costs a fee")
    )]
    EmptyUpdate,

    #[error("changing who controls `{name}` needs --allow-authority-change")]
    #[diagnostic(
        code(pecu::authority_change),
        help("publishing primary addresses nobody holds, or a threshold nobody can meet, is the one mistake with no remedy — not for the holder, not for the recovery authority, not for anyone. Say --allow-authority-change to mean it")
    )]
    NeedsAuthorityFlag { name: String },

    #[error("`{value}` is not an address this can use")]
    #[diagnostic(
        code(pecu::bad_address),
        help("transparent addresses start with R; identities are i-addresses")
    )]
    BadAddress { value: String },

    #[error("`{name}` has no i-address to point an authority at")]
    #[diagnostic(
        code(pecu::bad_authority),
        help("an authority is an identity: pass a name like `guardian@` or its i-address")
    )]
    BadAuthority { name: String },

    #[error("an unlock delay of {blocks} blocks is over the {max} consensus allows")]
    #[diagnostic(
        code(pecu::delay_too_long),
        help("that is roughly {years} years at a block a minute. Worth knowing: the daemon's own helper silently clamps an over-long delay to the maximum instead of refusing, so asking elsewhere for more than this can return a lock decades shorter than the one requested, with no error")
    )]
    DelayTooLong { blocks: u32, max: u32, years: u32 },

    #[error("cancelled")]
    #[diagnostic(code(pecu::cancelled), help("nothing was broadcast"))]
    Cancelled,

    #[error("cannot ask for confirmation")]
    #[diagnostic(
        code(pecu::no_tty),
        help("there is no terminal to confirm on — pass --yes to go ahead without asking, or --dry-run to stop before broadcasting")
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

fn flow(what: &'static str, source: FlowError) -> LifecycleError {
    use verus_sdk::verus_tx::TxError;
    let advice = match &source {
        // The consensus rule, refused before a signature exists. Worth its own
        // advice because the remedy is a *different operation* — point recovery
        // somewhere else first — not a retry or a flag.
        FlowError::Tx(TxError::RevocationWouldStrand) => {
            "an identity that is its own recovery authority cannot be revoked: nobody could \
             undo it. Point recovery at another VerusID first with `pecu id update --recovery \
             <name@> --allow-authority-change`, and then it becomes revocable"
                .to_string()
        }
        FlowError::Tx(TxError::AlreadyRevoked) => {
            "it is already revoked — `pecu id recover` is the way back, signed by its recovery \
             authority"
                .to_string()
        }
        FlowError::NoSuchIdentity(_) => {
            "check the name; `pecu id show <name@>` reads it off the chain".to_string()
        }
        // Caught before signing, and the commonest way to get these wrong.
        // Falling through to "run `pecu doctor`" would send someone looking at
        // the node when the node is fine and the key simply is not the one.
        // The four rules in `CIdentity::IsInvalidMutation`, now transcribed by
        // the SDK instead of arriving as `mandatory-script-verify-flag-failed`
        // once the fee is gone. The reason names the values, so this only has
        // to say what to do about the one nobody can work out by hand.
        FlowError::Tx(TxError::TimelockRefused { .. }) => {
            "an unlock height is measured from the transaction's own expiry, not from the tip, \
             so it cannot be computed by hand — `pecu id unlock` reads the delay and works it \
             out. A lock can also only ever be moved later, never shortened"
                .to_string()
        }
        FlowError::Tx(TxError::NotAPrimaryAddress { .. }) => {
            "`pecu id show <name@>` lists the addresses that control it, and `pecu key list` \
             shows what you hold. A recovery may have handed the identity to different keys"
                .to_string()
        }
        FlowError::Tx(TxError::NotEnoughSigners { supplied, required }) => format!(
            "{supplied} of the {required} required signatures. These commands sign with one \
             key, so an identity needing more than that cannot be changed from here yet"
        ),
        // The fee, not the identity. `--from` names the key that pays as well
        // as the key that signs, so a correct authority with an empty address
        // still cannot go anywhere.
        FlowError::Tx(TxError::InsufficientFunds { required, .. }) => format!(
            "the signing key pays the fee as well, and it holds nothing. Send it at least {} \
             — `pecu key list` shows its address",
            fmt::sats(*required)
        ),
        // The pre-check the SDK does so consensus does not have to refuse
        // anonymously. Its message already names the authority and the
        // threshold, so this only has to say where to look.
        FlowError::Content(message) if message.contains("authority") => {
            "`pecu id show <name@>` names the authority, and `pecu key list` shows which keys \
             you hold. Consensus refuses this without saying why, so it is caught here instead"
                .to_string()
        }
        // Not failures to build: answers. Saying "the change could not be
        // built" for either would send someone looking for a fault.
        FlowError::Content(message) if message.contains("already counting down") => {
            "nothing to do — the countdown is running. `pecu id show <name@>` reports the \
             height it opens at, and nothing can bring that forward"
                .to_string()
        }
        FlowError::Content(message) if message.contains("is not locked") => {
            "nothing to do — there is no timelock on it. `pecu id update --unlock-delay` or \
             `--lock-until` is how one is set"
                .to_string()
        }
        FlowError::Content(_) => {
            "the identity was read, but the change could not be built from it".to_string()
        }
        _ => "run `pecu doctor`, or point somewhere else with --node".to_string(),
    };
    LifecycleError::Flow {
        what,
        advice,
        source: Box::new(source),
    }
}

/// Everything the three commands share: the guard, the key and the node.
struct Session {
    node: Node,
    envelope: Envelope,
    key: PrivateKey,
}

fn open(settings: &Settings, label: Option<&str>) -> Result<Session, miette::Report> {
    if !settings.profile.allow_spend {
        return Err(LifecycleError::SpendingDisabled {
            profile: settings.profile.name.clone(),
        }
        .into());
    }
    let store = Keystore::new(&settings.paths);
    let envelope = choose_key(&store, label)?;
    let secret = keystore::passphrase(&format!("passphrase for `{}`", envelope.label), false)?;
    let key = envelope.unlock(&secret)?;
    let node = node::connect(&settings.profile)?;
    Ok(Session {
        node,
        envelope,
        key,
    })
}

/// Resolve a name or i-address to the 20 bytes an authority field holds.
fn authority_id(ui: &Ui, node: &Node, given: &str) -> Result<[u8; 20], miette::Report> {
    // An i-address is already the answer and needs no node. Anything else has
    // to be looked up, and the lookup is the node's word for it.
    if let Ok(parsed) = given.parse::<Address>() {
        return Ok(parsed.hash());
    }
    ui.sdk(format!("node.identity({given:?})"));
    let record = verus_sdk::network::ChainReader::identity(node, given)
        .map_err(|source| flow("reading the authority identity", FlowError::Rpc(source)))?;
    ui.sdk_result(format!("identity_address: {}", record.identity_address));
    record
        .identity_address
        .parse::<Address>()
        .map(|parsed| parsed.hash())
        .map_err(|_| {
            LifecycleError::BadAuthority {
                name: given.to_string(),
            }
            .into()
        })
}

/// `pecu id update`.
pub fn update(
    ui: &Ui,
    settings: &Settings,
    globals: &Globals,
    args: &IdUpdateArgs,
) -> miette::Result<()> {
    let touches_authority = !args.primary.is_empty()
        || args.min_sigs.is_some()
        || args.revocation.is_some()
        || args.recovery.is_some();
    // A timelock is deliberately not an authority change: it does not move who
    // controls the identity, so it does not demand the flag that guards that.
    let timelock = match (args.lock_until, args.unlock_delay, args.clear_timelock) {
        (Some(height), _, _) => Some(Timelock::UntilBlock(height)),
        (_, Some(blocks), _) => Some(Timelock::DelayAfterUnlock(blocks)),
        (_, _, true) => Some(Timelock::None),
        _ => None,
    };
    if !touches_authority && timelock.is_none() {
        return Err(LifecycleError::EmptyUpdate.into());
    }
    // Checked here as well as by the SDK so it costs nothing: no key unlocked,
    // no node asked. The number is unguessable enough to be worth naming.
    if let Some(blocks) = args.unlock_delay {
        if blocks > MAX_UNLOCK_DELAY {
            return Err(LifecycleError::DelayTooLong {
                blocks,
                max: MAX_UNLOCK_DELAY,
                // A block a minute, which is what Verus targets.
                years: MAX_UNLOCK_DELAY / (60 * 24 * 365),
            }
            .into());
        }
    }
    // Checked before the keystore is opened: a refusal that costs a passphrase
    // prompt is a refusal that wasted the one interaction this command needs.
    if touches_authority && !args.allow_authority_change {
        return Err(LifecycleError::NeedsAuthorityFlag {
            name: args.name.clone(),
        }
        .into());
    }

    let session = open(settings, args.from.as_deref())?;

    let mut change = IdentityChange::new();
    if touches_authority {
        change = change.allowing_authority_change();
    }
    if let Some(timelock) = timelock {
        change = change.with_timelock(timelock);
    }
    if !args.primary.is_empty() {
        let addresses = args
            .primary
            .iter()
            .map(|given| destination(given))
            .collect::<Result<Vec<_>, _>>()?;
        change = change.with_primary_addresses(addresses);
    }
    if let Some(min_sigs) = args.min_sigs {
        change = change.with_min_sigs(min_sigs);
    }
    if let Some(given) = &args.revocation {
        change = change.with_revocation_authority(authority_id(ui, &session.node, given)?);
    }
    if let Some(given) = &args.recovery {
        change = change.with_recovery_authority(authority_id(ui, &session.node, given)?);
    }

    ui.sdk(format!(
        "verus_sdk::network::prepare_identity_update(&node, &key, &[&key], {:?}, &change)",
        args.name
    ));
    let unsent = prepare_identity_update(
        &session.node,
        &session.key,
        &[&session.key],
        &args.name,
        &change,
    )
    .map_err(|source| flow("building the update", source))?;
    ui.sdk_result(format!(
        "Unsent<Updated> {{ txid: {}, changes_authority: {} }}",
        unsent.outcome.txid, unsent.outcome.changes_authority
    ));

    let panel = Panel::new(if globals.dry_run {
        "WOULD UPDATE"
    } else {
        "UPDATE"
    })
    .row("identity", name_row(ui, &args.name))
    .row(
        "txid",
        Text::of(&unsent.outcome.txid, ui.theme.palette.value),
    )
    .row(
        "fee",
        amount_row(ui, unsent.outcome.fee, &settings.profile.currency),
    );
    let panel = describe_update(ui, panel, args);

    finish(ui, settings, globals, panel, unsent, "update", |outcome| {
        serde_json::json!({
            "identity": outcome.identity,
            "txid": outcome.txid,
            "changes_authority": outcome.changes_authority,
            "fee": outcome.fee.to_sat(),
            "change": outcome.change.to_sat(),
        })
    })
}

fn describe_update(ui: &Ui, mut panel: Panel, args: &IdUpdateArgs) -> Panel {
    let palette = ui.theme.palette;
    panel = panel.rule();
    if !args.primary.is_empty() {
        for (index, address) in args.primary.iter().enumerate() {
            let label = if index == 0 { "primary" } else { "" };
            panel = panel.row(label, Text::of(address, palette.value));
        }
    }
    if let Some(min_sigs) = args.min_sigs {
        panel = panel.row("min sigs", Text::of(min_sigs.to_string(), palette.value));
    }
    if let Some(revocation) = &args.revocation {
        panel = panel.row("revocation", Text::of(revocation, palette.accent));
    }
    if let Some(recovery) = &args.recovery {
        panel = panel.row("recovery", Text::of(recovery, palette.accent));
    }
    if let Some(height) = args.lock_until {
        panel = panel.row(
            "locked until",
            Text::of(
                format!("block {}", fmt::height(height.into())),
                palette.warn,
            ),
        );
    }
    if let Some(blocks) = args.unlock_delay {
        panel = panel.row(
            "unlock delay",
            Text::of(
                fmt::plural(blocks as usize, "block", "blocks"),
                palette.warn,
            ),
        );
    }
    if args.clear_timelock {
        panel = panel.row("timelock", Text::of("removed", palette.warn));
    }

    panel = panel.note(Text::of(
        "everything not named above is carried through untouched — the identity is restated \
         from the output script consensus reads, not from a rendering of it",
        palette.muted,
    ));
    if !args.primary.is_empty() || args.min_sigs.is_some() {
        panel = panel.note(Text::of(
            "addresses nobody holds, or a threshold nobody can meet, cannot be undone by \
             anyone — not the holder, not the recovery authority",
            palette.warn,
        ));
    }
    if args.revocation.is_some() || args.recovery.is_some() {
        panel = panel.note(Text::of(
            "pointing an authority at another VerusID is one-way: these keys cannot take it \
             back afterwards",
            palette.warn,
        ));
    }
    if args.recovery.is_some() {
        panel = panel.note(Text::of(
            "moving recovery off the identity is also what makes it revocable at all",
            palette.muted,
        ));
    }
    if args.unlock_delay.is_some() {
        panel = panel.note(Text::of(
            "a delay locks the identity indefinitely: nothing counts down until an unlock is \
             asked for, and only the revocation and recovery authorities can act meanwhile",
            palette.warn,
        ));
    }
    if args.lock_until.is_some() {
        panel = panel.note(Text::of(
            "an absolute height starts counting as soon as this is mined and cannot be paused",
            palette.muted,
        ));
    }
    panel
}

/// `pecu id revoke`.
pub fn revoke(
    ui: &Ui,
    settings: &Settings,
    globals: &Globals,
    args: &IdAuthorityArgs,
) -> miette::Result<()> {
    let session = open(settings, args.from.as_deref())?;

    ui.sdk(format!(
        "verus_sdk::network::prepare_identity_revocation(&node, &key, &[&key], {:?})",
        args.name
    ));
    let unsent =
        prepare_identity_revocation(&session.node, &session.key, &[&session.key], &args.name)
            .map_err(|source| flow("building the revocation", source))?;
    ui.sdk_result(format!(
        "Unsent<Revoked> {{ txid: {}, authority: {} }}",
        unsent.outcome.txid, unsent.outcome.authority
    ));

    let palette = ui.theme.palette;
    let panel = Panel::new(if globals.dry_run {
        "WOULD REVOKE"
    } else {
        "REVOKE"
    })
    .row("identity", name_row(ui, &args.name))
    .row(
        "authority",
        Text::of(&unsent.outcome.authority, palette.accent),
    )
    .row(
        "signed by",
        Text::of(&session.envelope.address, palette.value),
    )
    .row("txid", Text::of(&unsent.outcome.txid, palette.value))
    .row(
        "fee",
        amount_row(ui, unsent.outcome.fee, &settings.profile.currency),
    )
    .note(Text::of(
        "a revoked identity cannot sign, cannot spend what it holds, and cannot be updated. \
         Only its recovery authority can bring it back",
        palette.warn,
    ))
    .note(Text::of(
        "revocation is retroactive for logins: a signature made before this still verifies \
         against the chain as it was, and `pecu id login verify` rejects it anyway",
        palette.muted,
    ));

    finish(ui, settings, globals, panel, unsent, "revoke", |outcome| {
        serde_json::json!({
            "identity": outcome.identity,
            "txid": outcome.txid,
            "authority": outcome.authority,
            "fee": outcome.fee.to_sat(),
            "change": outcome.change.to_sat(),
        })
    })
}

/// `pecu id recover`.
pub fn recover(
    ui: &Ui,
    settings: &Settings,
    globals: &Globals,
    args: &IdRecoverArgs,
) -> miette::Result<()> {
    let session = open(settings, args.from.as_deref())?;

    // A recovery that names no new keys brings the identity back exactly as it
    // was. Naming them is how a compromised identity is taken away from
    // whoever holds the old ones, which is the whole point of the authority.
    let mut restore = IdentityChange::new();
    if !args.primary.is_empty() {
        let addresses = args
            .primary
            .iter()
            .map(|given| destination(given))
            .collect::<Result<Vec<_>, _>>()?;
        restore = restore
            .allowing_authority_change()
            .with_primary_addresses(addresses);
        if let Some(min_sigs) = args.min_sigs {
            restore = restore.with_min_sigs(min_sigs);
        }
    }

    ui.sdk(format!(
        "verus_sdk::network::prepare_identity_recovery(&node, &key, &[&key], {:?}, &restore)",
        args.name
    ));
    let unsent = prepare_identity_recovery(
        &session.node,
        &session.key,
        &[&session.key],
        &args.name,
        &restore,
    )
    .map_err(|source| flow("building the recovery", source))?;
    ui.sdk_result(format!(
        "Unsent<Recovered> {{ txid: {}, replaces_primary_addresses: {} }}",
        unsent.outcome.txid, unsent.outcome.replaces_primary_addresses
    ));

    let palette = ui.theme.palette;
    let mut panel = Panel::new(if globals.dry_run {
        "WOULD RECOVER"
    } else {
        "RECOVER"
    })
    .row("identity", name_row(ui, &args.name))
    .row(
        "authority",
        Text::of(&unsent.outcome.authority, palette.accent),
    )
    .row(
        "signed by",
        Text::of(&session.envelope.address, palette.value),
    )
    .row("txid", Text::of(&unsent.outcome.txid, palette.value))
    .row(
        "fee",
        amount_row(ui, unsent.outcome.fee, &settings.profile.currency),
    );

    if unsent.outcome.replaces_primary_addresses {
        panel = panel.rule();
        for (index, address) in args.primary.iter().enumerate() {
            let label = if index == 0 { "primary" } else { "" };
            panel = panel.row(label, Text::of(address, palette.value));
        }
        panel = panel.note(Text::of(
            "this hands the identity to those addresses. Whoever held the old ones loses it, \
             which is what a recovery authority is for — and getting them wrong loses it to \
             nobody",
            palette.warn,
        ));
    } else {
        panel = panel.note(Text::of(
            "no new primary addresses, so it comes back under exactly the keys it had when it \
             was revoked — including any that were compromised. `--primary` replaces them",
            palette.warn,
        ));
    }

    finish(ui, settings, globals, panel, unsent, "recover", |outcome| {
        serde_json::json!({
            "identity": outcome.identity,
            "txid": outcome.txid,
            "authority": outcome.authority,
            "replaces_primary_addresses": outcome.replaces_primary_addresses,
            "fee": outcome.fee.to_sat(),
            "change": outcome.change.to_sat(),
        })
    })
}

/// `pecu id unlock` — start the countdown on a delay-locked identity.
///
/// Its own command rather than a flag on `id update`, because the height cannot
/// be computed by the caller. Consensus measures the countdown from the
/// transaction's `nExpiryHeight`, not from the tip, and the expiry belongs to
/// the transaction the flow is building — so the floor is a number the caller
/// never sees. The obvious guess, tip plus delay, is below it, and consensus
/// answers that with an unexplained script failure.
///
/// This starts the clock. It does not stop the lock: the identity opens when
/// the chain reaches the published height, and nothing can bring that forward.
pub fn unlock(
    ui: &Ui,
    settings: &Settings,
    globals: &Globals,
    args: &IdUnlockArgs,
) -> miette::Result<()> {
    if !settings.profile.allow_spend {
        return Err(LifecycleError::SpendingDisabled {
            profile: settings.profile.name.clone(),
        }
        .into());
    }
    // Cheapest refusals first, then the network, then the one interaction.
    // Naming the key costs nothing and needs no secret, so "there is no key"
    // should not arrive behind a node timeout.
    let store = Keystore::new(&settings.paths);
    let envelope = choose_key(&store, args.from.as_deref())?;

    // The state read comes before the passphrase prompt. The flow checks it
    // too, but only once the key is unlocked — and asking for a secret to start
    // an unlock that was never going to happen wastes the one interaction this
    // command needs. It also buys the panel the delay it reports, which would
    // otherwise be a number appearing from nowhere.
    let node = node::connect(&settings.profile)?;
    let before = held(ui, &node, &args.name)?;
    let delay = match before.identity.timelock() {
        Timelock::DelayAfterUnlock(delay) => Some(delay),
        Timelock::UntilBlock(height) => {
            return Err(flow(
                "starting the unlock",
                FlowError::Content(format!(
                    "{} is already counting down to block {height}; there is nothing to start",
                    args.name
                )),
            )
            .into())
        }
        Timelock::None => {
            return Err(flow(
                "starting the unlock",
                FlowError::Content(format!("{} is not locked", args.name)),
            )
            .into())
        }
    };

    let secret = keystore::passphrase(&format!("passphrase for `{}`", envelope.label), false)?;
    let key = envelope.unlock(&secret)?;
    let session = Session {
        node,
        envelope,
        key,
    };

    ui.sdk(format!(
        "verus_sdk::network::prepare_identity_unlock(&node, &key, &[&key], {:?}, {})",
        args.name, args.extra_blocks
    ));
    let unsent = prepare_identity_unlock(
        &session.node,
        &session.key,
        &[&session.key],
        &args.name,
        args.extra_blocks,
    )
    .map_err(|source| flow("starting the unlock", source))?;
    ui.sdk_result(format!(
        "Unsent<Updated> {{ txid: {} }}",
        unsent.outcome.txid
    ));

    let palette = ui.theme.palette;
    let mut panel = Panel::new(if globals.dry_run {
        "WOULD UNLOCK"
    } else {
        "UNLOCK"
    })
    .row("identity", name_row(ui, &args.name));
    if let Some(delay) = delay {
        panel = panel.row(
            "delay",
            Text::of(
                fmt::plural(delay as usize, "block", "blocks"),
                palette.value,
            ),
        );
    }
    panel = panel
        .row(
            "signed by",
            Text::of(&session.envelope.address, palette.value),
        )
        .row("txid", Text::of(&unsent.outcome.txid, palette.value))
        .row(
            "fee",
            amount_row(ui, unsent.outcome.fee, &settings.profile.currency),
        )
        .note(Text::of(
            "this starts the countdown; it does not stop the lock. The identity opens when the \
             chain reaches the published height, and nothing can bring that forward",
            palette.muted,
        ))
        .note(Text::of(
            "the height is the delay plus this transaction's own expiry, which is why it \
             cannot be worked out by hand — `pecu id show` will report it once this confirms",
            palette.muted,
        ));

    finish(ui, settings, globals, panel, unsent, "unlock", |outcome| {
        serde_json::json!({
            "identity": outcome.identity,
            "txid": outcome.txid,
            "fee": outcome.fee.to_sat(),
            "change": outcome.change.to_sat(),
        })
    })
}

/// Show it, ask, send it. The same shape for all three, because all three are
/// one signed transaction with an outcome worth reading first.
fn finish<T, F>(
    ui: &Ui,
    settings: &Settings,
    globals: &Globals,
    panel: Panel,
    unsent: Unsent<T>,
    what: &'static str,
    to_json: F,
) -> miette::Result<()>
where
    F: Fn(&T) -> serde_json::Value,
{
    if ui.is_json() {
        let mut document = to_json(&unsent.outcome);
        if let Some(object) = document.as_object_mut() {
            object.insert("broadcast".into(), serde_json::json!(false));
            object.insert("hex".into(), serde_json::json!(unsent.hex));
        }
        if globals.dry_run {
            emit(&document);
            return Ok(());
        }
        // `--json` is output, not consent — the same rule `pecu send` follows,
        // and these are less reversible than a payment.
        if !globals.yes {
            return Err(LifecycleError::CannotConfirm.into());
        }
        let node = node::connect(&settings.profile)?;
        ui.sdk("unsent.broadcast(&node)");
        let outcome = unsent
            .broadcast(&node)
            .map_err(|source| flow("broadcasting", source))?;
        let mut document = to_json(&outcome);
        if let Some(object) = document.as_object_mut() {
            object.insert("broadcast".into(), serde_json::json!(true));
        }
        emit(&document);
        return Ok(());
    }

    ui.panel(&panel);

    if globals.dry_run {
        ui.blank();
        ui.note("nothing was sent. Drop --dry-run to go ahead");
        ui.explain_panel();
        return Ok(());
    }
    if !globals.yes {
        confirm(ui, what)?;
    }

    let node = node::connect(&settings.profile)?;
    ui.sdk("unsent.broadcast(&node)");
    let outcome = unsent
        .broadcast(&node)
        .map_err(|source| flow("broadcasting", source))?;
    let json = to_json(&outcome);
    let txid = json["txid"].as_str().unwrap_or_default().to_string();

    ui.blank();
    ui.ok(format!("broadcast — txid {txid}"));
    ui.note(format!(
        "{}/tx/{txid}",
        settings.profile.explorer.trim_end_matches('/')
    ));
    ui.explain_panel();
    Ok(())
}

/// An address as an identity's `primary_addresses` entry.
///
/// The variant follows the address kind rather than defaulting to a pubkey
/// hash: a VerusID listed as a primary address is `Destination::Identity`, and
/// writing its 20 bytes under the wrong variant would publish an identity
/// controlled by a key nobody has.
fn destination(given: &str) -> Result<Destination, LifecycleError> {
    let parsed = given
        .parse::<Address>()
        .map_err(|_| LifecycleError::BadAddress {
            value: given.to_string(),
        })?;
    Ok(match parsed.kind() {
        AddressKind::PubKeyHash => Destination::PubKeyHash(parsed.hash()),
        AddressKind::Identity => Destination::Identity(parsed.hash()),
        AddressKind::ScriptHash => Destination::ScriptHash(parsed.hash()),
    })
}

fn name_row(ui: &Ui, name: &str) -> Text {
    let palette = ui.theme.palette;
    let glyphs = ui.theme.glyphs;
    let mut row = Text::of(
        fmt::untrusted(name, NAME_BUDGET, glyphs.ellipsis),
        palette.accent,
    );
    // Naming by i-address is checked against the decoded object with no node
    // involved; a `name@` can only be checked against what the node reported.
    // Worth saying which one is in play before something irreversible.
    if name.parse::<Address>().is_err() {
        row = row.push("  (named, not verified offline)", palette.muted);
    }
    row
}

fn amount_row(ui: &Ui, amount: verus_sdk::money::Amount, currency: &str) -> Text {
    Text::of(fmt::amount(amount), ui.theme.palette.value)
        .space()
        .push(currency, ui.theme.palette.muted)
}

/// Read the current identity the way consensus reads it.
fn held(ui: &Ui, node: &Node, identity: &str) -> Result<Held, miette::Report> {
    ui.sdk(format!(
        "verus_sdk::network::current_identity(&node, {identity:?})"
    ));
    let held =
        current_identity(node, identity).map_err(|source| flow("reading the identity", source))?;
    ui.sdk_result(format!(
        "Held {{ output: {}:{} }}",
        held.output.txid, held.output.vout
    ));
    Ok(held)
}

fn confirm(ui: &Ui, what: &str) -> Result<(), LifecycleError> {
    use std::io::{IsTerminal, Write};
    if !std::io::stdin().is_terminal() {
        return Err(LifecycleError::CannotConfirm);
    }
    ui.blank();
    print!("  type `{what}` to go ahead: ");
    let _ = std::io::stdout().flush();

    let mut answer = String::new();
    std::io::stdin()
        .read_line(&mut answer)
        .map_err(|_| LifecycleError::CannotConfirm)?;
    if answer.trim() != what {
        return Err(LifecycleError::Cancelled);
    }
    Ok(())
}

fn choose_key(store: &Keystore, label: Option<&str>) -> Result<Envelope, miette::Report> {
    if let Some(label) = label {
        return Ok(store.load(label)?);
    }
    let keys = store.list()?;
    match keys.len() {
        0 => Err(LifecycleError::NoKey.into()),
        1 => Ok(keys.into_iter().next().expect("just checked")),
        count => Err(LifecycleError::AmbiguousKey { count }.into()),
    }
}

fn emit(value: &serde_json::Value) {
    println!(
        "{}",
        serde_json::to_string_pretty(value).expect("the report is plain data")
    );
}
