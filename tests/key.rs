//! `pecu key …`, end to end.
//!
//! All offline. Every test gets its own `PECU_HOME`, so none of them can see or
//! damage a real keystore, and `PECU_PASSPHRASE` stands in for the prompt.
//!
//! Argon2id is deliberately slow — that is what it is for — so these are
//! noticeably heavier than the other suites. Each `key gen` is one derivation.

use assert_cmd::Command;
use predicates::str::contains;
use tempfile::TempDir;

const PASSPHRASE: &str = "correct horse battery staple";

fn home() -> TempDir {
    tempfile::tempdir().expect("a temp dir")
}

fn pecu(home: &TempDir) -> Command {
    let mut command = Command::cargo_bin("pecu").expect("the pecu binary should be built");
    command
        .env("PECU_HOME", home.path())
        .env("PECU_PASSPHRASE", PASSPHRASE)
        .env_remove("NO_COLOR")
        .env_remove("PECU_THEME");
    command
}

fn json(command: &mut Command) -> serde_json::Value {
    let assertion = command.arg("--json").assert().success();
    let stdout = String::from_utf8_lossy(&assertion.get_output().stdout).into_owned();
    serde_json::from_str(&stdout).unwrap_or_else(|error| panic!("not json: {error}\n{stdout}"))
}

fn generate(home: &TempDir, label: &str) -> serde_json::Value {
    json(pecu(home).args(["key", "gen", "--label", label]))
}

#[test]
fn a_generated_key_is_listed_and_shown() {
    let home = home();
    let created = generate(&home, "demo");
    let address = created["address"].as_str().expect("an address").to_string();
    assert!(
        address.starts_with('R'),
        "not a transparent address: {address}"
    );

    let listed = json(pecu(&home).args(["key", "list"]));
    assert_eq!(listed["keys"][0]["label"], "demo");
    assert_eq!(listed["keys"][0]["address"], address.as_str());

    let shown = json(pecu(&home).args(["key", "show", "demo"]));
    assert_eq!(shown["address"], address.as_str());
    assert_eq!(shown["kdf"], "argon2id");
    assert_eq!(shown["cipher"], "chacha20poly1305");
}

#[test]
fn the_key_file_holds_no_plaintext_key_material() {
    let home = home();
    generate(&home, "demo");
    let exported = json(pecu(&home).args(["key", "export", "demo", "--yes"]));
    let wif = exported["wif"].as_str().expect("a wif");

    let written = std::fs::read_to_string(home.path().join("keys/demo.json")).expect("readable");
    assert!(!written.contains(wif), "the WIF is sitting in the key file");
    assert!(written.contains("argon2id"), "no kdf recorded:\n{written}");
}

#[test]
fn export_refuses_to_print_a_key_without_yes() {
    let home = home();
    generate(&home, "demo");
    pecu(&home)
        .args(["key", "export", "demo"])
        .assert()
        .failure()
        .stderr(contains("pecu::export_refused"))
        .stdout(contains("in the clear"));
}

/// The refusal above writes its warning to **stdout**, which under `--json` put
/// 221 bytes of prose in front of the document and broke `| jq` on garbage
/// rather than on emptiness (#49). The warning is worth having and stdout is
/// the wrong place for it in machine-readable mode, so it is not written there
/// at all — the refusal arrives as the same error object every other failing
/// `--json` run prints.
#[test]
fn export_refused_under_json_prints_json_and_no_prose() {
    let home = home();
    generate(&home, "demo");
    let assertion = pecu(&home)
        .args(["key", "export", "demo", "--json"])
        .assert()
        .code(1);
    let output = assertion.get_output();
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();

    let document: serde_json::Value =
        serde_json::from_str(&stdout).unwrap_or_else(|error| panic!("not json: {error}\n{stdout}"));
    assert_eq!(document["error"]["code"], "pecu::export_refused");
    assert!(
        !stdout.contains("in the clear"),
        "prose leaked onto the parsed stream:\n{stdout}"
    );

    // The warning is not lost — the reader still gets the refusal, on the
    // stream that is not being parsed.
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("pecu::export_refused"), "{stderr}");
}

#[test]
fn the_wrong_passphrase_cannot_export() {
    let home = home();
    generate(&home, "demo");
    pecu(&home)
        .env("PECU_PASSPHRASE", "not the passphrase")
        .args(["key", "export", "demo", "--yes"])
        .assert()
        .failure()
        .stderr(contains("wrong passphrase"));
}

#[test]
fn an_exported_wif_can_be_imported_back() {
    let home = home();
    let created = generate(&home, "demo");
    let exported = json(pecu(&home).args(["key", "export", "demo", "--yes"]));

    // Piped in rather than passed as an argument: PECU_PASSPHRASE covers the
    // encryption passphrase only, and a WIF on the command line would be
    // visible in the process list.
    let imported = json(
        pecu(&home)
            .args(["key", "import", "--label", "copy"])
            .write_stdin(format!("{}\n", exported["wif"].as_str().expect("a wif"))),
    );
    assert_eq!(imported["address"], created["address"]);
}

#[test]
fn a_recovery_phrase_really_does_restore_the_key() {
    let home = home();
    let created = json(pecu(&home).args([
        "key",
        "gen",
        "--label",
        "paper",
        "--from-phrase",
        "--show-phrase",
    ]));
    let phrase = created["recovery_phrase"].as_str().expect("a phrase");
    assert_eq!(phrase.split_whitespace().count(), 24);

    let restored = json(
        pecu(&home)
            .args(["key", "import", "--label", "restored", "--phrase"])
            .write_stdin(format!("{phrase}\n")),
    );
    assert_eq!(
        restored["address"], created["address"],
        "the phrase did not reproduce the key it was generated with"
    );
}

#[test]
fn a_phrase_is_not_stored_and_not_shown_unless_asked() {
    let home = home();
    let created = json(pecu(&home).args(["key", "gen", "--label", "paper", "--from-phrase"]));
    assert!(
        created["recovery_phrase"].is_null(),
        "the phrase leaked without --show-phrase"
    );

    let written = std::fs::read_to_string(home.path().join("keys/paper.json")).expect("readable");
    // Any real English word would do; "the" is not in the BIP-39 wordlist but a
    // stored phrase would put two dozen lowercase words in this file.
    let words = written.matches(' ').count();
    assert!(words < 100, "suspiciously wordy key file:\n{written}");
}

#[test]
fn show_phrase_requires_from_phrase() {
    let home = home();
    pecu(&home)
        .args(["key", "gen", "--label", "x", "--show-phrase"])
        .assert()
        .failure()
        .stderr(contains("--from-phrase"));
}

#[test]
fn key_phrase_stores_nothing() {
    let home = home();
    let generated = json(pecu(&home).args(["key", "phrase"]));

    assert_eq!(
        generated["phrase"]
            .as_str()
            .expect("a phrase")
            .split_whitespace()
            .count(),
        24
    );
    // 64 bytes of BIP-39 seed, hex.
    assert_eq!(generated["bip39_seed"].as_str().expect("a seed").len(), 128);
    assert!(generated["transparent_address"]
        .as_str()
        .expect("an address")
        .starts_with('R'));

    assert!(
        !home.path().join("keys").exists(),
        "`key phrase` created a keystore"
    );
}

#[test]
fn a_label_is_never_silently_overwritten() {
    let home = home();
    generate(&home, "demo");
    pecu(&home)
        .args(["key", "gen", "--label", "demo"])
        .assert()
        .failure()
        .stderr(contains("already a key called `demo`"));
}

#[test]
fn a_label_cannot_escape_the_keystore() {
    let home = home();
    pecu(&home)
        .args(["key", "gen", "--label", "../escape"])
        .assert()
        .failure()
        .stderr(contains("not a usable key name"));
}

#[test]
fn a_missing_key_says_what_does_exist() {
    let home = home();
    generate(&home, "demo");
    pecu(&home)
        .args(["key", "show", "nope"])
        .assert()
        .failure()
        .stderr(contains("no key called `nope`"))
        .stderr(contains("demo"));
}

#[test]
fn an_empty_keystore_says_so_rather_than_printing_an_empty_table() {
    let home = home();
    pecu(&home)
        .args(["key", "list"])
        .assert()
        .success()
        .stdout(contains("no keys yet"));
}

#[cfg(unix)]
#[test]
fn key_files_are_not_readable_by_other_users() {
    use std::os::unix::fs::PermissionsExt;

    let home = home();
    generate(&home, "demo");
    let mode = std::fs::metadata(home.path().join("keys/demo.json"))
        .expect("stat")
        .permissions()
        .mode();
    assert_eq!(mode & 0o077, 0, "group or other can read the key file");
}
