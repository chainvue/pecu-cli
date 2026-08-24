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

    ui.panel(
        &Panel::new("KEYS")
            .table(key_table(ui, &keys))
            .note(Text::of(tildify(store.dir()), palette.muted)),
    );
    Ok(())
}

/// The `key list` table.
///
/// Two columns may be shortened, and the order matters: the label pays first,
/// and the address is touched only once the label is already down to its own
/// header.
///
/// The label is what the user typed. It is bounded only by the keystore's
/// sixty-four characters, so it is the column that makes this table too wide in
/// the first place, and it is the one that can be recovered without it — from
/// `key list --json`, and from the keystore filename on disk. The address
/// cannot: it is the one cryptographic identifier on the line, the thing a
/// reader is here to copy in order to be paid.
///
/// Queued the other way round the arithmetic is perverse, and the whole point
/// of this ordering is to avoid it. A column drains all the way to its own
/// header before the next one gives up anything, and column widths are the
/// maximum across rows — so one sixty-four-character label used to shrink the
/// address column to the width of the word `ADDRESS` for *every* key in the
/// list, at every width the theme can reach, while all sixty-four characters of
/// that one label survived. Keys with nothing to do with it lost their
/// addresses. Label-first costs a long label some characters it can spare and
/// keeps every address whole wherever the frame has room for it.
fn key_table(ui: &Ui, keys: &[Envelope]) -> Table {
    let palette = ui.theme.palette;
    let mut table = Table::new(vec![
        Column::left("label"),
        Column::left("address"),
        Column::right("created"),
    ])
    .elidable(0)
    .elidable(1);
    for envelope in keys {
        table.push(vec![
            Text::of(&envelope.label, palette.accent),
            Text::of(&envelope.address, palette.value),
            Text::of(age(envelope.created), palette.muted),
        ]);
    }
    table
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
/// in, and a transcription that lost its place is a wallet that is gone.
///
/// Four columns is the shape this is meant to be read in, but four columns of
/// `24. abandon` do not fit a panel at the narrow end of the theme's range, and
/// there is nothing in this table a column may give up: a BIP-39 word cut from
/// the middle is a wallet that cannot be recovered, and it would be cut into a
/// shape that still looks like a word. So the grid pays in columns instead —
/// the same words, more rows — which costs nothing but height. Built widest
/// first and measured, rather than derived from the longest word in the
/// wordlist, because the numbering and the gutters are part of the width too.
///
/// Only under a skin that draws a frame. The plain skin prints no border for
/// the grid to run out through, and its output is piped far more often than it
/// is read, so it keeps the four-column shape at every terminal width — the
/// same rule `Item::Path` and `Table::lines` already follow. Narrowing there
/// would make `key phrase | …` depend on `$COLUMNS`, which is a worse trade
/// than a long line in a window nobody is looking at.
fn word_grid(ui: &Ui, phrase: &str) -> Table {
    const WIDEST: usize = 4;
    if ui.theme.is_plain() {
        return grid(ui, phrase, WIDEST);
    }
    (1..=WIDEST)
        .rev()
        .map(|columns| grid(ui, phrase, columns))
        .find(|table| {
            table
                .lines(&ui.theme)
                .iter()
                .all(|line| line.width() <= ui.theme.width)
        })
        .unwrap_or_else(|| grid(ui, phrase, 1))
}

fn grid(ui: &Ui, phrase: &str, columns: usize) -> Table {
    let palette = ui.theme.palette;
    let words: Vec<&str> = phrase.split_whitespace().collect();
    let rows = words.len().div_ceil(columns);

    let mut table = Table::headerless(std::iter::repeat_n(crate::ui::Align::Left, columns));
    for row in 0..rows {
        let cells = (0..columns)
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
    crate::failure::document(value);
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keystore::{Cipher, Kdf};
    use crate::ui::text::strip_ansi;
    use crate::ui::theme::{Skin, Theme};

    /// The framed skin at `terminal` columns, which is the one with a border to
    /// run out through.
    fn framed(terminal: usize) -> Ui {
        let mut ui = Ui::new(crate::cli::Theme::Phosphor, false, false);
        ui.theme = Theme::with_skin(Skin::Phosphor, terminal);
        ui
    }

    fn envelope(label: &str, address: &str) -> Envelope {
        Envelope {
            version: 1,
            label: label.to_string(),
            address: address.to_string(),
            compressed: true,
            created: 0,
            kdf: Kdf {
                algorithm: "argon2id".into(),
                salt: String::new(),
                memory_kib: 0,
                iterations: 0,
                parallelism: 0,
            },
            cipher: Cipher {
                algorithm: "chacha20poly1305".into(),
                nonce: String::new(),
            },
            ciphertext: String::new(),
        }
    }

    /// Every framed line of a panel, by visible width. Escapes stripped first,
    /// then counted in terminal cells: the border is multi-byte and the content
    /// is full of colour, so bytes and `chars` both give nonsense.
    fn frame_widths(rendered: &str) -> Vec<usize> {
        rendered
            .lines()
            .map(strip_ansi)
            .filter(|line| line.starts_with(['\u{250c}', '\u{2502}', '\u{251c}', '\u{2514}']))
            .map(|line| unicode_width::UnicodeWidthStr::width(line.as_str()))
            .collect()
    }

    fn assert_square(rendered: &str, at: usize) {
        let widths = frame_widths(rendered);
        assert!(!widths.is_empty(), "nothing was framed at {at}");
        assert!(
            widths.windows(2).all(|pair| pair[0] == pair[1]),
            "ragged frame at {at} columns, widths {widths:?}:\n{rendered}"
        );
    }

    #[test]
    fn the_key_list_frame_stays_square_at_every_width_the_theme_can_reach() {
        // `Theme::with_skin` clamps to 48..=78 inner cells, so a terminal of 52
        // is the narrowest frame there is and 82 is the widest. The labels are
        // a one-character one and the longest the keystore will accept.
        let keys = [
            envelope("a", "RQC1EG3GhZ9pvT9YgCp3YvxyYBsdb4FYfH"),
            envelope("treasury", "RSUwsFmpwLh8ThGdm8tCbHenPftH6Us4sG"),
            envelope(&"l".repeat(64), "RWpmUu8uEcbgyrgqVHqXMbckR5g11HsvaD"),
        ];
        for terminal in 40..=120 {
            let ui = framed(terminal);
            let panel = Panel::new("KEYS").table(key_table(&ui, &keys));
            assert_square(&panel.render(&ui.theme), terminal);
        }
    }

    #[test]
    fn a_key_list_wide_enough_for_whole_addresses_prints_whole_addresses() {
        let address = "RQC1EG3GhZ9pvT9YgCp3YvxyYBsdb4FYfH";
        let ui = framed(120);
        let rendered = Panel::new("KEYS")
            .table(key_table(&ui, &[envelope("demo", address)]))
            .render(&ui.theme);
        assert!(
            strip_ansi(&rendered).contains(address),
            "an address the frame had room for was cut:\n{rendered}"
        );
    }

    #[test]
    fn a_long_label_pays_for_the_frame_rather_than_the_addresses() {
        // The order `key_table` queues its two elidable columns in, pinned
        // where it is decided rather than only where it is implemented.
        //
        // A short label alone fits; add a sixty-four-character one and the
        // table is far over budget at every width the theme can reach. Queued
        // id-first the address column drained to the width of the word ADDRESS
        // first — so `alice`, who has nothing to do with the long-labelled key,
        // lost her address to it, at 80, 120 and 200 columns alike. Column
        // widths are the maximum across rows, which is what makes one bad row
        // everybody's problem.
        let address = "RQC1EG3GhZ9pvT9YgCp3YvxyYBsdb4FYfH";
        let keys = [
            envelope("alice", address),
            envelope(&"l".repeat(64), "RWpmUu8uEcbgyrgqVHqXMbckR5g11HsvaD"),
        ];
        for terminal in 80..=120 {
            let ui = framed(terminal);
            let rendered = Panel::new("KEYS")
                .table(key_table(&ui, &keys))
                .render(&ui.theme);
            assert!(
                strip_ansi(&rendered).contains(address),
                "a neighbour's long label cut alice's address at {terminal} columns:\n{rendered}"
            );
        }
    }

    #[test]
    fn the_recovery_grid_keeps_every_word_whole_at_every_width() {
        // A BIP-39 word cut from the middle is a wallet that cannot be
        // recovered, and it would be cut into something that still looks like a
        // word. So the grid may drop to fewer columns but never to fewer or
        // shorter words.
        let phrase = "abandon ability able about above absent absorb abstract absurd abuse \
                      access accident account accuse achieve acid acoustic acquire across act \
                      action actor actress actual";
        let words: Vec<&str> = phrase.split_whitespace().collect();
        assert_eq!(words.len(), 24);

        for terminal in 40..=120 {
            let ui = framed(terminal);
            let panel = Panel::new("RECOVERY PHRASE").table(word_grid(&ui, phrase));
            let rendered = panel.render(&ui.theme);
            assert_square(&rendered, terminal);
            let visible = strip_ansi(&rendered);
            for (index, word) in words.iter().enumerate() {
                assert!(
                    visible.contains(&format!("{}. {word}", index + 1)),
                    "word {} went missing at {terminal} columns:\n{rendered}",
                    index + 1
                );
            }
        }
    }

    #[test]
    fn the_plain_recovery_grid_is_the_same_at_every_width() {
        // Piped output must not depend on `$COLUMNS`. The plain skin draws no
        // frame for the grid to run out through, so it keeps the four-column
        // shape everywhere — the rule `Item::Path` and `Table::lines` already
        // follow. Narrowing here would make `key phrase | …` change shape with
        // the window it happened to run in.
        let phrase = "abandon ability able about above absent absorb abstract absurd abuse \
                      access accident account accuse achieve acid acoustic acquire across act \
                      action actor actress actual";
        let widest = {
            let mut ui = Ui::new(crate::cli::Theme::Plain, false, false);
            ui.theme = Theme::with_skin(Skin::Plain, 200);
            Panel::new("RECOVERY PHRASE")
                .table(word_grid(&ui, phrase))
                .render(&ui.theme)
        };
        for terminal in 20..=200 {
            let mut ui = Ui::new(crate::cli::Theme::Plain, false, false);
            ui.theme = Theme::with_skin(Skin::Plain, terminal);
            let rendered = Panel::new("RECOVERY PHRASE")
                .table(word_grid(&ui, phrase))
                .render(&ui.theme);
            assert_eq!(
                rendered, widest,
                "the plain grid changed shape at {terminal} columns"
            );
        }
    }

    #[test]
    fn the_recovery_grid_is_four_columns_wide_wherever_four_columns_fit() {
        // Fewer columns is the price of a narrow terminal, not the new shape.
        let phrase = "abandon ability able about above absent absorb abstract absurd abuse \
                      access accident account accuse achieve acid acoustic acquire across act \
                      action actor actress actual";
        let ui = framed(80);
        assert_eq!(word_grid(&ui, phrase).lines(&ui.theme).len(), 6);
    }
}
