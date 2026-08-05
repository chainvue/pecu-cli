//! `pecu id publish` · `pecu id read` — data hung off a VerusID.
//!
//! A VerusID carries a `contentmultimap`: named slots, each holding values, all
//! on chain and all readable by anyone. This writes one slot and reads them
//! back.
//!
//! # Names are hashed, and the namespace is what keeps them apart
//!
//! `profile` is not stored as `profile`. It is hashed, with a **namespace**, to
//! a 20-byte VDXF key — so two applications that both pick the name `profile`
//! do not write over each other, provided they namespace under identities they
//! control. The default here is the identity being written to, which is the
//! self-consistent choice: what `pecu id publish alice@ profile` writes,
//! `pecu id read alice@ profile` finds. `--namespace` is for reading what
//! somebody else's application published.
//!
//! The hashing is one-way, so `pecu id read alice@` with no key lists what is
//! there as raw i-addresses. Nothing can turn those back into names.
//!
//! # Publishing is an identity update, and updates restate everything
//!
//! There is no "append". The whole identity is rewritten every time, so the SDK
//! reads the current one out of the output script consensus reads, changes the
//! one entry, and puts the rest back untouched. What that means here: writing a
//! key replaces whatever stood under it, and `--remove` deletes it.
//!
//! It also costs a miner fee, paid by the signing key, and the key must be one
//! of the identity's own primary addresses — an identity's data is changed by
//! its controller, not by whoever is passing by.

use miette::Diagnostic;
use thiserror::Error;
use verus_sdk::network::{
    key_address, prepare_publish, read, read_all, read_history, ChainReader, ContentValue,
    FlowError, Namespace, Published,
};
use verus_sdk::verus_keys::Address;

use crate::cli::{Globals, IdPublishArgs, IdReadArgs};
use crate::config::Settings;
use crate::keystore::{self, Envelope, Keystore};
use crate::node::{self, Node};
use crate::payload;
use crate::ui::{fmt, Column, Panel, Table, Text, Ui};

/// How much of an identity name is ever printed.
///
/// These arrive on the command line rather than from the node, but that makes
/// them no safer to print inside a frame: an argument can hold a newline or an
/// escape just as a currency name can.
const NAME_BUDGET: usize = 40;

/// How much of a published value is shown before it is cut. This is arbitrary
/// bytes somebody else put on a public chain, so it is elided rather than
/// trusted to be a sensible length.
const VALUE_BUDGET: usize = 60;

#[derive(Debug, Error, Diagnostic)]
pub enum VdxfError {
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
        help("publishing rewrites the identity on chain and pays a miner fee. Set `allow_spend = true` under [profiles.{profile}] in config.toml")
    )]
    SpendingDisabled { profile: String },

    #[error("publishing needs a value")]
    #[diagnostic(
        code(pecu::no_value),
        help("give one as text, as `@file`, or as `-` to read stdin — or pass --remove to delete the key")
    )]
    NoValue,

    #[error("`{name}` has no i-address to hang keys under")]
    #[diagnostic(
        code(pecu::bad_namespace),
        help("a namespace is an identity: pass a name like `bob@` or an i-address")
    )]
    BadNamespace { name: String },

    #[error("{what} failed")]
    #[diagnostic(code(pecu::flow_failed), help("{advice}"))]
    Flow {
        what: &'static str,
        advice: String,
        #[source]
        source: Box<FlowError>,
    },
}

fn flow(what: &'static str, source: FlowError) -> VdxfError {
    let advice = match &source {
        FlowError::NoSuchIdentity(_) => {
            "check the name; `pecu id show <name@>` reads it off the chain".to_string()
        }
        // The commonest way to get this wrong, and the SDK names both sides of
        // the mismatch in its own message, so this only has to point at the fix.
        FlowError::Content(message) if message.contains("first key controls") => {
            "the signing key must be one of the identity's primary addresses — `pecu id show \
             <name@>` lists them, and `pecu key list` shows what you hold"
                .to_string()
        }
        FlowError::Content(_) => {
            "the identity was read but the update could not be built from it".to_string()
        }
        _ => "run `pecu doctor`, or point somewhere else with --node".to_string(),
    };
    VdxfError::Flow {
        what,
        advice,
        source: Box::new(source),
    }
}

/// Resolve the namespace an application's keys hang under.
///
/// Defaults to the identity being read or written, so a key published under a
/// name is found again under the same name. Anything else has to be asked for.
fn namespace(
    ui: &Ui,
    node: &Node,
    identity: &str,
    chain: &str,
    given: Option<&str>,
) -> Result<Namespace, miette::Report> {
    let name = given.unwrap_or(identity);
    ui.sdk(format!("node.identity({name:?})"));
    let record = node
        .identity(name)
        .map_err(|source| flow("reading the namespace identity", FlowError::Rpc(source)))?;
    ui.sdk_result(format!("identity_address: {}", record.identity_address));

    let address: Address =
        record
            .identity_address
            .parse()
            .map_err(|_| VdxfError::BadNamespace {
                name: name.to_string(),
            })?;
    Ok(Namespace::of_identity(address.hash(), chain))
}

/// `pecu id publish` — write one key.
pub fn publish(
    ui: &Ui,
    settings: &Settings,
    globals: &Globals,
    args: &IdPublishArgs,
) -> miette::Result<()> {
    if !settings.profile.allow_spend {
        return Err(VdxfError::SpendingDisabled {
            profile: settings.profile.name.clone(),
        }
        .into());
    }

    // Read the value before anything is unlocked or any node is called: a
    // missing file should not cost a passphrase prompt.
    let values: Vec<Vec<u8>> = if args.remove {
        Vec::new()
    } else {
        let raw = payload::read_bytes(args.value.as_deref())?;
        if raw.is_empty() {
            // An empty value and a removal are the same thing on chain, and
            // guessing which was meant is how a key gets deleted by accident.
            return Err(VdxfError::NoValue.into());
        }
        vec![raw]
    };

    let store = Keystore::new(&settings.paths);
    let envelope = choose_key(&store, args.from.as_deref())?;
    let secret = keystore::passphrase(&format!("passphrase for `{}`", envelope.label), false)?;
    let key = envelope.unlock(&secret)?;

    let node = node::connect(&settings.profile)?;
    let space = namespace(
        ui,
        &node,
        &args.name,
        &settings.profile.currency,
        args.namespace.as_deref(),
    )?;
    let vdxf_key = space
        .key(&args.key)
        .map_err(|source| flow("deriving the key", source))?;

    ui.sdk(format!(
        "verus_sdk::network::prepare_publish(&node, &keys, {:?}, {:?}, {}, {} value(s))",
        args.name,
        envelope.address,
        key_address(vdxf_key),
        values.len()
    ));
    let unsent = prepare_publish(
        &node,
        &[&key],
        &args.name,
        &envelope.address,
        vdxf_key,
        values.clone(),
    )
    .map_err(|source| flow("building the update", source))?;
    ui.sdk_result(format!(
        "Unsent<Published> {{ txid: {}, fee: {} }}",
        unsent.outcome.txid,
        fmt::amount(unsent.outcome.fee)
    ));

    if globals.dry_run {
        report(ui, settings, args, &unsent.outcome, vdxf_key, false);
        ui.explain_panel();
        return Ok(());
    }

    ui.sdk("unsent.broadcast(&node)");
    let published = unsent
        .broadcast(&node)
        .map_err(|source| flow("broadcasting the update", source))?;
    ui.sdk_result(format!("Published {{ txid: {} }}", published.txid));

    report(ui, settings, args, &published, vdxf_key, true);
    ui.explain_panel();
    Ok(())
}

fn report(
    ui: &Ui,
    settings: &Settings,
    args: &IdPublishArgs,
    published: &Published,
    vdxf_key: [u8; 20],
    broadcast: bool,
) {
    if ui.is_json() {
        emit(&serde_json::json!({
            "identity": args.name,
            "key": args.key,
            "key_address": key_address(vdxf_key),
            "values": published.values,
            "removed": published.values == 0,
            "txid": published.txid,
            "fee": published.fee.to_sat(),
            "change": published.change.to_sat(),
            "broadcast": broadcast,
        }));
        return;
    }

    let palette = ui.theme.palette;
    let glyphs = ui.theme.glyphs;
    let removed = published.values == 0;

    // The title states what happened, not what was asked for. A dry run headed
    // "PUBLISHED" is the panel contradicting its own last line.
    let title = match (broadcast, removed) {
        (true, false) => "PUBLISHED",
        (true, true) => "REMOVED",
        (false, false) => "WOULD PUBLISH",
        (false, true) => "WOULD REMOVE",
    };
    let mut panel = Panel::new(title)
        .row(
            "identity",
            Text::of(safe(&args.name, glyphs), palette.accent),
        )
        .row("key", Text::of(safe(&args.key, glyphs), palette.value))
        // The name is what you typed; this is what is actually on chain, and
        // it is the only one of the two a reader can look up.
        .row(
            "key address",
            Text::of(key_address(vdxf_key), palette.muted),
        )
        .row(
            "values",
            Text::of(published.values.to_string(), palette.value),
        )
        .rule()
        .row("txid", Text::of(&published.txid, palette.value))
        .row(
            "fee",
            Text::of(fmt::amount(published.fee), palette.value)
                .space()
                .push(&settings.profile.currency, palette.muted),
        );

    if broadcast {
        panel = panel
            .line(
                Text::of(glyphs.ok, palette.ok)
                    .space()
                    .push("broadcast", palette.ok),
            )
            .note(Text::of(
                format!(
                    "{}/tx/{}",
                    settings.profile.explorer.trim_end_matches('/'),
                    published.txid
                ),
                palette.muted,
            ))
            .note(Text::of(
                "it is on chain once this confirms — `pecu id read` will show it then",
                palette.muted,
            ));
    } else {
        panel = panel.note(Text::of(
            "nothing was sent. Drop --dry-run to publish it",
            palette.warn,
        ));
    }

    panel = panel.note(Text::of(
        "an identity update restates the whole identity, so this replaced whatever stood under \
         the key rather than adding to it",
        palette.muted,
    ));
    ui.panel(&panel);
}

/// `pecu id read` — read one key, or list them all.
pub fn read_command(ui: &Ui, settings: &Settings, args: &IdReadArgs) -> miette::Result<()> {
    let node = node::connect(&settings.profile)?;

    let Some(name) = args.key.as_deref() else {
        return list_all(ui, &node, &args.name);
    };

    let space = namespace(
        ui,
        &node,
        &args.name,
        &settings.profile.currency,
        args.namespace.as_deref(),
    )?;
    let vdxf_key = space
        .key(name)
        .map_err(|source| flow("deriving the key", source))?;

    let what = if args.history { "read_history" } else { "read" };
    ui.sdk(format!(
        "verus_sdk::network::{what}(&node, {:?}, {})",
        args.name,
        key_address(vdxf_key)
    ));
    let values = if args.history {
        read_history(&node, &args.name, vdxf_key)
    } else {
        read(&node, &args.name, vdxf_key)
    }
    .map_err(|source| flow("reading the key", source))?;
    ui.sdk_result(format!("{} value(s)", values.len()));

    if ui.is_json() {
        emit(&serde_json::json!({
            "identity": args.name,
            "key": name,
            "key_address": key_address(vdxf_key),
            "history": args.history,
            "values": values.iter().map(value_json).collect::<Vec<_>>(),
        }));
        return Ok(());
    }

    let palette = ui.theme.palette;
    let glyphs = ui.theme.glyphs;

    if values.is_empty() {
        ui.note(format!("{} holds nothing under `{name}`", args.name));
        ui.explain_panel();
        return Ok(());
    }

    let mut panel = Panel::new(if args.history { "HISTORY" } else { "VALUE" })
        .row(
            "identity",
            Text::of(safe(&args.name, glyphs), palette.accent),
        )
        .row("key", Text::of(safe(name, glyphs), palette.value))
        .row(
            "key address",
            Text::of(key_address(vdxf_key), palette.muted),
        )
        .rule();

    for (index, value) in values.iter().enumerate() {
        if index > 0 {
            panel = panel.blank();
        }
        panel = panel.wrapped(0, render(ui, value));
    }

    if args.history {
        panel = panel.note(Text::of(
            "every value ever published under this key, oldest first. An update that carried a \
             key forward unchanged still appears, so a repeated value is a restatement rather \
             than a rewrite",
            palette.muted,
        ));
    }
    panel = panel.note(Text::of(
        format!(
            "{} published by whoever controls this identity. It is public, and it is not \
             checked by anything",
            glyphs.warn
        ),
        palette.muted,
    ));

    ui.panel(&panel);
    ui.explain_panel();
    Ok(())
}

/// Every key an identity holds, when no key was named.
fn list_all(ui: &Ui, node: &Node, identity: &str) -> miette::Result<()> {
    ui.sdk(format!("verus_sdk::network::read_all(&node, {identity:?})"));
    let all = read_all(node, identity).map_err(|source| flow("reading the identity", source))?;
    ui.sdk_result(format!("{} key(s)", all.len()));

    if ui.is_json() {
        emit(&serde_json::json!({
            "identity": identity,
            "keys": all.iter().map(|(key, values)| serde_json::json!({
                "key_address": key,
                "values": values.iter().map(value_json).collect::<Vec<_>>(),
            })).collect::<Vec<_>>(),
        }));
        return Ok(());
    }

    if all.is_empty() {
        ui.note(format!("{identity} has published nothing"));
        ui.explain_panel();
        return Ok(());
    }

    let palette = ui.theme.palette;
    let mut table = Table::new(vec![
        Column::left("key address"),
        Column::right("values"),
        Column::left("first value"),
    ]);
    for (key, values) in &all {
        table.push(vec![
            Text::of(key, palette.value),
            Text::of(values.len().to_string(), palette.muted),
            values
                .first()
                .map(|value| render(ui, value))
                .unwrap_or_default(),
        ]);
    }

    ui.panel(
        &Panel::new("PUBLISHED KEYS")
            .row(
                "identity",
                Text::of(safe(identity, ui.theme.glyphs), palette.accent),
            )
            .rule()
            .table(table)
            .note(Text::of(
                "keys are hashes, so there is no way back to the name that made one. \
                 `pecu id read <name@> <key>` derives it forwards and looks it up",
                palette.muted,
            )),
    );
    ui.explain_panel();
    Ok(())
}

/// Cap and clean a name before it goes inside a frame.
fn safe(name: &str, glyphs: crate::ui::theme::Glyphs) -> String {
    fmt::untrusted(name, NAME_BUDGET, glyphs.ellipsis)
}

/// A published value, made safe to print.
///
/// Bytes on a public chain, written by someone else. Text is shown as text
/// because that is what most of it is, but through the same untrusted-text
/// filter as a currency name — and anything that is not valid UTF-8 is shown as
/// hex rather than mangled into replacement characters.
fn render(ui: &Ui, value: &ContentValue) -> Text {
    let palette = ui.theme.palette;
    let glyphs = ui.theme.glyphs;
    match value {
        ContentValue::Bytes(bytes) => match std::str::from_utf8(bytes) {
            Ok(text) => Text::of(
                fmt::untrusted(text, VALUE_BUDGET, glyphs.ellipsis),
                palette.value,
            ),
            Err(_) => Text::of(
                fmt::elide(&hex::encode(bytes), 24, 8, glyphs.ellipsis),
                palette.muted,
            )
            .space()
            .push(format!("({} bytes)", bytes.len()), palette.muted),
        },
        // The daemon recognised the key and decoded it for us. There are no
        // original bytes in the reply to show instead.
        ContentValue::Structured(json) => Text::of(
            fmt::untrusted(&json.to_string(), VALUE_BUDGET, glyphs.ellipsis),
            palette.value,
        ),
    }
}

/// Both renderings, because a consumer cannot ask for the other one later.
fn value_json(value: &ContentValue) -> serde_json::Value {
    match value {
        ContentValue::Bytes(bytes) => serde_json::json!({
            "kind": "bytes",
            "hex": hex::encode(bytes),
            // Absent rather than lossy when the bytes are not text.
            "text": std::str::from_utf8(bytes).ok(),
        }),
        ContentValue::Structured(json) => serde_json::json!({
            "kind": "structured",
            "value": json,
        }),
    }
}

fn choose_key(store: &Keystore, label: Option<&str>) -> Result<Envelope, miette::Report> {
    if let Some(label) = label {
        return Ok(store.load(label)?);
    }
    let keys = store.list()?;
    match keys.len() {
        0 => Err(VdxfError::NoKey.into()),
        1 => Ok(store.load(&keys[0].label)?),
        count => Err(VdxfError::AmbiguousKey { count }.into()),
    }
}

fn emit(value: &serde_json::Value) {
    println!(
        "{}",
        serde_json::to_string_pretty(value).expect("the report is plain data")
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_is_shown_as_text_and_binary_as_hex() {
        let ui = Ui::new(crate::cli::Theme::Plain, false, false);

        let text = render(&ui, &ContentValue::Bytes(b"hello".to_vec()));
        assert_eq!(text.render(), "hello");

        // Invalid UTF-8. Rendering it as a string would print replacement
        // characters and lose what the bytes actually were.
        let binary = render(&ui, &ContentValue::Bytes(vec![0xff, 0xfe, 0x00]));
        assert!(binary.render().contains("fffe00"), "{}", binary.render());
        assert!(binary.render().contains("3 bytes"), "{}", binary.render());
    }

    #[test]
    fn a_published_value_cannot_smuggle_control_characters_into_the_frame() {
        let ui = Ui::new(crate::cli::Theme::Plain, false, false);
        // Arbitrary bytes from a public chain. A newline or an escape would
        // otherwise break out of the panel it is being printed inside.
        let hostile = ContentValue::Bytes(b"ok\x1b[31m\nSPENDABLE 999.0\r".to_vec());
        let shown = render(&ui, &hostile).render();
        assert!(!shown.contains('\n'), "{shown:?}");
        assert!(!shown.contains('\r'), "{shown:?}");
        assert!(!shown.contains('\u{1b}'), "{shown:?}");
    }

    #[test]
    fn json_carries_hex_always_and_text_only_when_it_is_text() {
        let text = value_json(&ContentValue::Bytes(b"hi".to_vec()));
        assert_eq!(text["hex"], "6869");
        assert_eq!(text["text"], "hi");

        let binary = value_json(&ContentValue::Bytes(vec![0xff]));
        assert_eq!(binary["hex"], "ff");
        assert!(
            binary["text"].is_null(),
            "bytes that are not text must not be reported as text: {binary}"
        );
    }
}
