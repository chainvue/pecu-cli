//! `pecu key …` — making, importing and inspecting keys.
//!
//! The rule this command group is built around: a private key is printed to a
//! terminal exactly once, when it is created and only if you asked for a
//! recovery phrase, and otherwise only by `key export`, which says what it is
//! about to do first.

use verus_sdk::verus_keys::bip39::{mnemonic_from_entropy, mnemonic_to_seed};
use verus_sdk::verus_keys::{private_key_from_seed_phrase, PrivateKey};

use crate::cli::{Globals, KeyCommand};
use crate::config::{tildify, Settings};
use crate::keystore::{self, Envelope, Keystore, KeystoreError};
use crate::ui::{fmt, Column, Panel, Table, Text, Ui};

pub fn run(
    ui: &Ui,
    settings: &Settings,
    globals: &Globals,
    command: &KeyCommand,
) -> miette::Result<()> {
    let store = Keystore::new(&settings.paths);
    match command {
        KeyCommand::Gen {
            label,
            from_phrase,
            show_phrase,
        } => gen(ui, &store, label, *from_phrase, *show_phrase),
        KeyCommand::Import { label, phrase } => import(ui, &store, label, *phrase),
        KeyCommand::List => list(ui, &store),
        KeyCommand::Show { label } => show(ui, &store, label),
        KeyCommand::Export { label } => export(ui, &store, label, globals.yes),
        KeyCommand::Phrase => phrase(ui),
    }
}

fn gen(
    ui: &Ui,
    store: &Keystore,
    label: &str,
    from_phrase: bool,
    show_phrase: bool,
) -> miette::Result<()> {
    keystore::check_label(label)?;
    if store.exists(label) {
        return Err(KeystoreError::Exists {
            label: label.to_string(),
            path: store.path_for(label),
        }
        .into());
    }

    let entropy = keystore::entropy()?;
    // Two routes from the same 32 bytes. A raw key is unrecoverable if the
    // keystore is lost; a phrase-backed one can be typed back in from paper.
    let (key, recovery) = if from_phrase {
        let words = mnemonic_from_entropy(&entropy);
        // The transparent side does not use BIP-39 — it hashes the phrase text
        // verbatim. Same words, different key schedule from the shielded side.
        let key =
            private_key_from_seed_phrase(&words).map_err(|source| KeystoreError::Key { source })?;
        (key, Some(words))
    } else {
        let key = PrivateKey::from_bytes(&entropy, true)
            .map_err(|source| KeystoreError::Key { source })?;
        (key, None)
    };

    let secret = keystore::passphrase("passphrase for the new key", true)?;
    let envelope = store.store(label, &key, &secret)?;

    if ui.is_json() {
        emit_json(&serde_json::json!({
            "label": envelope.label,
            "address": envelope.address,
            "path": store.path_for(label),
            "recovery_phrase": recovery.as_deref().filter(|_| show_phrase),
        }));
        return Ok(());
    }

    let palette = ui.theme.palette;
    ui.panel(
        &Panel::new("KEY CREATED")
            .row("label", Text::of(&envelope.label, palette.accent))
            .row("address", Text::of(&envelope.address, palette.value))
            .path("file", &store.path_for(label)),
    );

    match (&recovery, show_phrase) {
        (Some(words), true) => {
            ui.blank();
            ui.panel(
                &Panel::new("RECOVERY PHRASE")
                    .table(word_grid(ui, words))
                    .note(Text::of(
                        "write this down, on paper, now — it is shown once and is not stored",
                        palette.warn,
                    )),
            );
        }
        (Some(_), false) => {
            ui.blank();
            ui.warn("a recovery phrase was generated but not shown");
            ui.note("re-run with --show-phrase to see it; there is no way to recover it later");
        }
        (None, _) => {
            ui.blank();
            ui.note("this key has no recovery phrase — the keystore file is the only copy");
        }
    }
    Ok(())
}

fn import(ui: &Ui, store: &Keystore, label: &str, from_phrase: bool) -> miette::Result<()> {
    keystore::check_label(label)?;
    if store.exists(label) {
        return Err(KeystoreError::Exists {
            label: label.to_string(),
            path: store.path_for(label),
        }
        .into());
    }

    // Read from a prompt or from stdin, never from an argument: a WIF or a
    // recovery phrase on the command line lands in the shell history and in the
    // process list of every other user on the machine.
    let key = if from_phrase {
        // Not trimmed. The transparent key schedule hashes the phrase text
        // verbatim, so trimming here would silently derive a different key than
        // the wallet the phrase came from.
        let words = keystore::read_secret("recovery phrase")?;
        private_key_from_seed_phrase(&words).map_err(|source| KeystoreError::Key { source })?
    } else {
        let wif = keystore::read_secret("WIF private key")?;
        PrivateKey::from_wif(wif.trim()).map_err(|source| KeystoreError::Key { source })?
    };

    let secret = keystore::passphrase("passphrase to encrypt it under", true)?;
    let envelope = store.store(label, &key, &secret)?;

    if ui.is_json() {
        emit_json(&serde_json::json!({
            "label": envelope.label,
            "address": envelope.address,
            "path": store.path_for(label),
        }));
        return Ok(());
    }

    let palette = ui.theme.palette;
    ui.panel(
        &Panel::new("KEY IMPORTED")
            .row("label", Text::of(&envelope.label, palette.accent))
            .row("address", Text::of(&envelope.address, palette.value))
            .path("file", &store.path_for(label)),
    );
    Ok(())
}

fn list(ui: &Ui, store: &Keystore) -> miette::Result<()> {
    let keys = store.list()?;

    if ui.is_json() {
        emit_json(&serde_json::json!({
            "keys": keys.iter().map(summary).collect::<Vec<_>>(),
        }));
        return Ok(());
    }

    let palette = ui.theme.palette;
    if keys.is_empty() {
        ui.note("no keys yet");
        ui.note("`pecu key gen --label demo` makes one");
        return Ok(());
    }

    let mut table = Table::new(vec![
        Column::left("label"),
        Column::left("address"),
        Column::right("created"),
    ]);
    for envelope in &keys {
        table.push(vec![
            Text::of(&envelope.label, palette.accent),
            Text::of(&envelope.address, palette.value),
            Text::of(age(envelope.created), palette.muted),
        ]);
    }
    ui.panel(
        &Panel::new("KEYS")
            .table(table)
            .note(Text::of(tildify(store.dir()), palette.muted)),
    );
    Ok(())
}

fn show(ui: &Ui, store: &Keystore, label: &str) -> miette::Result<()> {
    let envelope = store.load(label)?;

    if ui.is_json() {
        emit_json(&summary(&envelope));
        return Ok(());
    }

    let palette = ui.theme.palette;
    ui.panel(
        &Panel::new("KEY")
            .row("label", Text::of(&envelope.label, palette.accent))
            .row("address", Text::of(&envelope.address, palette.value))
            .row(
                "pubkey",
                Text::of(
                    if envelope.compressed {
                        "compressed"
                    } else {
                        "uncompressed"
                    },
                    palette.value,
                ),
            )
            .row("created", Text::of(age(envelope.created), palette.value))
            .path("file", &store.path_for(label))
            .section("ENCRYPTION")
            .row("kdf", Text::of(&envelope.kdf.algorithm, palette.value))
            .row(
                "cost",
                Text::of(
                    format!(
                        "{} MiB, {} pass(es), {} lane(s)",
                        envelope.kdf.memory_kib / 1024,
                        envelope.kdf.iterations,
                        envelope.kdf.parallelism
                    ),
                    palette.muted,
                ),
            )
            .row(
                "cipher",
                Text::of(&envelope.cipher.algorithm, palette.value),
            ),
    );
    Ok(())
}

fn export(ui: &Ui, store: &Keystore, label: &str, yes: bool) -> miette::Result<()> {
    let envelope = store.load(label)?;
    let palette = ui.theme.palette;

    if !yes {
        ui.fail(format!(
            "`key export` prints the private key for {} in the clear",
            envelope.address
        ));
        ui.note("anything that can read this terminal, its scrollback or its logs gets the key");
        ui.note("re-run with --yes if that is what you want");
        return Err(RefusedWithoutYes.into());
    }

    let secret = keystore::passphrase(&format!("passphrase for `{label}`"), false)?;
    let key = envelope.unlock(&secret)?;
    let wif = key.to_wif();

    if ui.is_json() {
        emit_json(&serde_json::json!({
            "label": envelope.label,
            "address": envelope.address,
            "wif": &*wif as &str,
        }));
        return Ok(());
    }

    ui.panel(
        &Panel::new("PRIVATE KEY")
            .row("label", Text::of(&envelope.label, palette.accent))
            .row("address", Text::of(&envelope.address, palette.value))
            .row("wif", Text::of(&*wif as &str, palette.danger))
            .note(Text::of(
                "clear your scrollback when you are done",
                palette.warn,
            )),
    );
    Ok(())
}

/// `pecu key phrase` — generate a phrase and show what it maps to, storing
/// nothing.
///
/// Worth its own command because the same 24 words drive two entirely different
/// key schedules, and that is the thing people get wrong: the shielded side goes
/// BIP-39 → seed → ZIP-32, and the transparent side ignores BIP-39 and hashes
/// the phrase text verbatim.
fn phrase(ui: &Ui) -> miette::Result<()> {
    let entropy = keystore::entropy()?;
    let words = mnemonic_from_entropy(&entropy);
    // Verus wallets use no BIP-39 passphrase. Getting it wrong is undetectable:
    // the seed is valid either way and the wallet is simply empty.
    let seed = mnemonic_to_seed(&words, "").map_err(|error| PhraseError(error.to_string()))?;
    let key =
        private_key_from_seed_phrase(&words).map_err(|source| KeystoreError::Key { source })?;

    if ui.is_json() {
        emit_json(&serde_json::json!({
            "phrase": &*words as &str,
            "bip39_seed": hex::encode(*seed),
            "transparent_address": key.address().to_string(),
        }));
        return Ok(());
    }

    let palette = ui.theme.palette;
    ui.panel(
        &Panel::new("RECOVERY PHRASE")
            .table(word_grid(ui, &words))
            .rule()
            .row(
                "transparent",
                Text::of(key.address().to_string(), palette.value),
            )
            .row(
                "bip39 seed",
                Text::of(
                    fmt::elide(&hex::encode(*seed), 16, 8, ui.theme.glyphs.ellipsis),
                    palette.muted,
                ),
            )
            .note(Text::of(
                "the transparent key hashes the phrase text; the shielded side uses the BIP-39 seed",
                palette.muted,
            ))
            .note(Text::of("nothing here was stored", palette.warn)),
    );
    Ok(())
}

/// A 24-word phrase as a numbered grid.
///
/// Numbered because the words are going onto paper and then being typed back
/// in, and a transcription that lost its place is a wallet that is gone. Four
/// columns of six keeps it inside a narrow terminal.
fn word_grid(ui: &Ui, phrase: &str) -> Table {
    const COLUMNS: usize = 4;
    let palette = ui.theme.palette;
    let words: Vec<&str> = phrase.split_whitespace().collect();
    let rows = words.len().div_ceil(COLUMNS);

    let mut table = Table::headerless(std::iter::repeat_n(crate::ui::Align::Left, COLUMNS));
    for row in 0..rows {
        let cells = (0..COLUMNS)
            .filter_map(|column| {
                // Down the columns, not across: that is how the words are read
                // back off the page.
                let index = column * rows + row;
                words.get(index).map(|word| {
                    Text::of(format!("{:>2}.", index + 1), palette.muted)
                        .space()
                        .push(*word, palette.accent)
                })
            })
            .collect();
        table.push(cells);
    }
    table
}

fn summary(envelope: &Envelope) -> serde_json::Value {
    serde_json::json!({
        "label": envelope.label,
        "address": envelope.address,
        "compressed": envelope.compressed,
        "created": envelope.created,
        "kdf": envelope.kdf.algorithm,
        "cipher": envelope.cipher.algorithm,
    })
}

fn emit_json(value: &serde_json::Value) {
    println!(
        "{}",
        serde_json::to_string_pretty(value).expect("the report is plain data")
    );
}

fn age(created: u64) -> String {
    match std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .and_then(|now| now.as_secs().checked_sub(created))
    {
        Some(seconds) => format!("{} ago", fmt::duration(seconds)),
        None => "just now".to_string(),
    }
}

#[derive(Debug, thiserror::Error, miette::Diagnostic)]
#[error("refused to print a private key without --yes")]
#[diagnostic(code(pecu::export_refused))]
struct RefusedWithoutYes;

#[derive(Debug, thiserror::Error, miette::Diagnostic)]
#[error("that is not a valid recovery phrase: {0}")]
#[diagnostic(
    code(pecu::bad_phrase),
    help("a BIP-39 phrase is 24 words from the English wordlist, with a checksum")
)]
struct PhraseError(String);
