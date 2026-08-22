//! `pecu send`.
//!
//! The guards are what these mostly assert, because the guards are the whole
//! point of the command: a wrong send cannot be undone. Everything that needs a
//! funded address is `#[ignore]`d.

use assert_cmd::Command;
use predicates::prelude::*;
use predicates::str::contains;
use tempfile::TempDir;

const PASSPHRASE: &str = "correct horse battery staple";
const DEAD_NODE: &str = "https://127.0.0.1:1";
const CHAIN_IDENTITY: &str = "iJhCezBExJHvtyH3fGhNnt2NhU4Ztkf2yq";

fn home() -> TempDir {
    tempfile::tempdir().expect("a temp dir")
}

fn pecu(home: &TempDir) -> Command {
    let mut command = Command::cargo_bin("pecu").expect("the pecu binary should be built");
    command
        .env("PECU_HOME", home.path())
        .env("PECU_PASSPHRASE", PASSPHRASE)
        .env_remove("NO_COLOR");
    command
}

fn generate(home: &TempDir, label: &str) -> String {
    let assertion = pecu(home)
        .args(["key", "gen", "--label", label, "--json"])
        .assert()
        .success();
    let stdout = String::from_utf8_lossy(&assertion.get_output().stdout).into_owned();
    serde_json::from_str::<serde_json::Value>(&stdout).expect("json")["address"]
        .as_str()
        .expect("an address")
        .to_string()
}

#[test]
fn mainnet_will_not_spend_without_being_told_to() {
    let home = home();
    generate(&home, "demo");
    pecu(&home)
        .args([
            "send",
            "--profile",
            "mainnet",
            "--to",
            CHAIN_IDENTITY,
            "--amount",
            "1",
        ])
        .assert()
        .failure()
        .stderr(contains("not allowed to spend"))
        .stderr(contains("allow_spend"));
}

#[test]
fn a_config_file_can_turn_mainnet_spending_on() {
    let home = home();
    generate(&home, "demo");
    std::fs::write(
        home.path().join("config.toml"),
        "[profiles.mainnet]\nallow_spend = true\n",
    )
    .expect("writable");
    // Gets past the guard and dies at the node instead, which is the proof.
    pecu(&home)
        .args([
            "send",
            "--profile",
            "mainnet",
            "--node",
            DEAD_NODE,
            "--to",
            CHAIN_IDENTITY,
            "--amount",
            "1",
        ])
        .assert()
        .failure()
        .stderr(contains("failed").and(contains("not allowed to spend").not()));
}

#[test]
fn an_amount_with_too_many_places_is_refused_rather_than_rounded() {
    let home = home();
    generate(&home, "demo");
    // Nine decimal places. Silently dropping the ninth would be a satoshi
    // vanishing, which the SDK refuses and so does this.
    pecu(&home)
        .args([
            "send",
            "--to",
            CHAIN_IDENTITY,
            "--amount",
            "1.999999999",
            "--node",
            DEAD_NODE,
        ])
        .assert()
        .failure()
        .stderr(contains("is not an amount"));
}

#[test]
fn an_empty_keystore_says_what_to_do() {
    let home = home();
    pecu(&home)
        .args(["send", "--to", CHAIN_IDENTITY, "--amount", "1"])
        .assert()
        .failure()
        .stderr(contains("no key to spend from"))
        .stderr(contains("pecu key gen"));
}

#[test]
fn several_keys_are_refused_rather_than_guessed_between() {
    let home = home();
    generate(&home, "one");
    generate(&home, "two");
    pecu(&home)
        .args(["send", "--to", CHAIN_IDENTITY, "--amount", "1"])
        .assert()
        .failure()
        .stderr(contains("no obvious one to spend from"))
        .stderr(contains("--from"));
}

#[test]
fn the_named_key_is_the_one_asked_for() {
    let home = home();
    generate(&home, "one");
    generate(&home, "two");
    pecu(&home)
        .args([
            "send",
            "--from",
            "nope",
            "--to",
            CHAIN_IDENTITY,
            "--amount",
            "1",
        ])
        .assert()
        .failure()
        .stderr(contains("no key called `nope`"));
}

#[test]
fn explain_prints_the_calls_even_when_the_command_fails() {
    let home = home();
    generate(&home, "demo");
    // The record is most useful on the path that went wrong.
    pecu(&home)
        .args([
            "send",
            "--to",
            CHAIN_IDENTITY,
            "--amount",
            "1",
            "--node",
            DEAD_NODE,
            "--explain",
        ])
        .assert()
        .failure()
        .stdout(contains("SDK CALLS"))
        .stdout(contains("verus_sdk::network::prepare_send"));
}

#[test]
fn nothing_is_recorded_without_the_flag() {
    let home = home();
    generate(&home, "demo");
    pecu(&home)
        .args([
            "send",
            "--to",
            CHAIN_IDENTITY,
            "--amount",
            "1",
            "--node",
            DEAD_NODE,
        ])
        .assert()
        .failure()
        .stdout(contains("SDK CALLS").not());
}

#[test]
#[ignore = "talks to api.verustest.net"]
fn an_unfunded_address_says_how_much_it_is_short() {
    let home = home();
    let address = generate(&home, "demo");
    pecu(&home)
        .args(["send", "--to", CHAIN_IDENTITY, "--amount", "1"])
        .assert()
        .failure()
        .stderr(contains("not enough spendable funds"))
        .stderr(contains(address))
        // Short phrases: miette wraps the help text, so anything longer is at
        // the mercy of where the line breaks.
        .stderr(contains("withheld"))
        .stderr(contains("VerusID"));
}

#[test]
#[ignore = "talks to api.verustest.net"]
fn a_name_nobody_registered_is_refused_before_a_key_is_unlocked() {
    let home = home();
    generate(&home, "demo");
    pecu(&home)
        .args([
            "send",
            "--to",
            "nothing-is-called-this-surely@",
            "--amount",
            "1",
        ])
        .assert()
        .failure()
        .stderr(contains("nothing on this chain is called"));
}

#[test]
fn a_token_cannot_be_sent_out_of_an_identity_yet() {
    let home = home();
    generate(&home, "demo");
    // `prepare_send_from_identity` takes no currency. Accepting both flags and
    // silently ignoring one would send the native coin when a token was asked
    // for, which is the wrong asset rather than a failure.
    pecu(&home)
        .args([
            "send",
            "--from-identity",
            "alice@",
            "--currency",
            "bridge@",
            "--to",
            CHAIN_IDENTITY,
            "--amount",
            "1",
            "--node",
            DEAD_NODE,
        ])
        .assert()
        .failure()
        .stderr(contains("cannot be used with"));
}

#[test]
fn spending_from_an_identity_still_needs_a_key_to_sign_with() {
    let home = home();
    // The identity owns the money; a key still proves the authority.
    pecu(&home)
        .args([
            "send",
            "--from-identity",
            "alice@",
            "--to",
            CHAIN_IDENTITY,
            "--amount",
            "1",
            "--node",
            DEAD_NODE,
        ])
        .assert()
        .failure()
        .stderr(contains("no key to spend from"));
}

/// Paying out of a VerusID's own funds, which is the other half of the
/// `HELD BY ID` row in `wallet balance`.
#[test]
#[ignore = "talks to api.verustest.net; needs the key for pecucli7@"]
fn an_identity_funded_payment_returns_its_change_to_the_identity() {
    let Ok(funded) = std::env::var("PECU_FUNDED_HOME") else {
        eprintln!("PECU_FUNDED_HOME is not set — skipping");
        return;
    };

    let assertion = Command::cargo_bin("pecu")
        .expect("built")
        .env("PECU_HOME", &funded)
        .args([
            "send",
            "--from",
            "faucet",
            "--from-identity",
            "pecucli7@",
            "--to",
            "RComfCn4wHHsGR8vWBAU7T1r3tHHyxN9Hm",
            "--amount",
            "0.01",
            "--dry-run",
            "--json",
        ])
        .assert()
        .success();
    let stdout = String::from_utf8_lossy(&assertion.get_output().stdout).into_owned();
    let document: serde_json::Value = serde_json::from_str(&stdout).expect("valid json");

    // The payer is the identity, not the signing key. A consumer reconciling
    // balances against `from` would otherwise debit the wrong address.
    assert_eq!(document["from"], "pecucli7@", "{document:#}");
    assert_eq!(document["from_identity"], "pecucli7@");
    assert_eq!(document["broadcast"], false);

    // Two outputs: the payment, and the change going back to the identity as a
    // pay-to-identity output rather than to the key that signed.
    assert_eq!(document["outputs"], 2, "{document:#}");
    let hex = document["hex"].as_str().expect("signed hex");

    let explained = Command::cargo_bin("pecu")
        .expect("built")
        .env("PECU_HOME", &funded)
        .args(["tx", "explain", hex, "--json"])
        .assert()
        .success();
    let out = String::from_utf8_lossy(&explained.get_output().stdout).into_owned();
    assert!(
        out.contains("i7r29bDQfrwjkTxjv4bcYD6B1ZV7WZ4kGo"),
        "the change did not go back to the identity:\n{out}"
    );
}

/// A timelocked identity cannot spend, and it should not take consensus to say so.
///
/// `prepare_send_from_identity` does not check the timelock: it builds and signs
/// happily, and the daemon answers `mandatory-script-verify-flag-failed`, naming
/// neither the identity nor the height. Measured against a locked
/// `pecurevoke1@` on VRSCTEST before this guard existed.
///
/// Skipped when the identity happens to be unlocked, because it releases itself
/// — this asserts the guard when there is something to guard against, rather
/// than depending on a chain state that expires.
#[test]
#[ignore = "talks to api.verustest.net"]
fn a_timelocked_identity_is_refused_before_the_transaction_is_built() {
    let Ok(funded) = std::env::var("PECU_FUNDED_HOME") else {
        eprintln!("PECU_FUNDED_HOME is not set — skipping");
        return;
    };

    let shown = Command::cargo_bin("pecu")
        .expect("built")
        .env("PECU_HOME", &funded)
        .args(["id", "show", "pecurevoke1@"])
        .assert()
        .success();
    let state = String::from_utf8_lossy(&shown.get_output().stdout).into_owned();
    if !state.contains("locked for") && !state.contains("no unlock requested") {
        eprintln!("pecurevoke1@ is not locked right now — skipping");
        return;
    }

    let assertion = Command::cargo_bin("pecu")
        .expect("built")
        .env("PECU_HOME", &funded)
        .args([
            "send",
            "--from-identity",
            "pecurevoke1@",
            "--from",
            "rescued",
            "--to",
            CHAIN_IDENTITY,
            "--amount",
            "0.1",
            "--dry-run",
        ])
        .assert()
        .failure();
    let stderr = String::from_utf8_lossy(&assertion.get_output().stderr).into_owned();

    // Either lock form is a legitimate answer: a height to wait for, or a
    // delay nobody has started, which has no height at all.
    assert!(
        stderr.contains("locked") || stderr.contains("no unlock has been started"),
        "{stderr}"
    );
    // Named, and not blamed on the node.
    assert!(stderr.contains("pecurevoke1@"), "{stderr}");
    assert!(
        !stderr.contains("pecu doctor"),
        "a locked identity is not a node problem:\n{stderr}"
    );
}

/// The funded path. Set `PECU_FUNDED_HOME` to a config root holding a key with
/// a little VRSCTEST in it — the Discord faucet is the way to get some — and
/// this will build, sign and *not* broadcast a real payment.
///
/// Skipped rather than failed when the variable is absent, so `make testnet`
/// stays green for anyone who has not funded anything.
#[test]
#[ignore = "needs a funded key; set PECU_FUNDED_HOME"]
fn a_dry_run_builds_real_signed_bytes_without_sending_them() {
    let Ok(funded) = std::env::var("PECU_FUNDED_HOME") else {
        eprintln!("PECU_FUNDED_HOME is not set — skipping");
        return;
    };

    let mut command = Command::cargo_bin("pecu").expect("built");
    let assertion = command
        .env("PECU_HOME", &funded)
        .args([
            "send",
            "--from",
            "faucet",
            "--to",
            CHAIN_IDENTITY,
            "--amount",
            "0.001",
            "--dry-run",
            "--json",
        ])
        .assert()
        .success();
    let stdout = String::from_utf8_lossy(&assertion.get_output().stdout).into_owned();
    let document: serde_json::Value = serde_json::from_str(&stdout).expect("valid json");

    assert_eq!(document["broadcast"], false, "a dry run must not send");
    assert_eq!(document["outcome"], "not_broadcast");
    let hex = document["hex"].as_str().expect("signed hex");
    assert!(!hex.is_empty());
    assert!(document["fee"].as_u64().unwrap_or(0) > 0, "no fee was paid");

    // The loop worth closing: what `send` built, `tx explain` reads back.
    Command::cargo_bin("pecu")
        .expect("built")
        .env("PECU_HOME", &funded)
        .args(["tx", "explain", hex, "--json"])
        .assert()
        .success()
        .stdout(contains("\"kind\": \"transaction\""));
}

/// `--json` is a request for machine-readable output. It used to also be a
/// silent `--yes`, because the confirmation prompt writes to stdout and was
/// skipped rather than moved — so `pecu send --json` spent money without asking.
#[test]
#[ignore = "needs a funded key; set PECU_FUNDED_HOME"]
fn json_output_is_not_consent_to_spend() {
    let Ok(funded) = std::env::var("PECU_FUNDED_HOME") else {
        eprintln!("PECU_FUNDED_HOME is not set — skipping");
        return;
    };

    let assertion = Command::cargo_bin("pecu")
        .expect("built")
        .env("PECU_HOME", &funded)
        .args([
            "send",
            "--from",
            "faucet",
            "--to",
            CHAIN_IDENTITY,
            "--amount",
            "0.001",
            "--json",
        ])
        .assert()
        .failure()
        .stderr(contains("--yes"));

    // Nothing was built and nothing was printed to be parsed: this has to fail
    // before the money moves, not after.
    let stdout = String::from_utf8_lossy(&assertion.get_output().stdout).into_owned();
    assert!(stdout.trim().is_empty(), "unexpected output:\n{stdout}");
}

/// Exactly one JSON document on the dry-run path, which is the one reachable
/// without a node. The success path is covered end-to-end in `tests/wallet.rs`,
/// and every outcome including the failures is covered exhaustively by the unit
/// tests on `delivery_json`.
#[test]
#[ignore = "needs a funded key; set PECU_FUNDED_HOME"]
fn json_output_is_a_single_document() {
    let Ok(funded) = std::env::var("PECU_FUNDED_HOME") else {
        eprintln!("PECU_FUNDED_HOME is not set — skipping");
        return;
    };

    let assertion = Command::cargo_bin("pecu")
        .expect("built")
        .env("PECU_HOME", &funded)
        .args([
            "send",
            "--from",
            "faucet",
            "--to",
            CHAIN_IDENTITY,
            "--amount",
            "0.001",
            "--dry-run",
            "--json",
        ])
        .assert()
        .success();
    let stdout = String::from_utf8_lossy(&assertion.get_output().stdout).into_owned();

    let documents: Vec<_> = serde_json::Deserializer::from_str(&stdout)
        .into_iter::<serde_json::Value>()
        .collect();
    assert_eq!(
        documents.len(),
        1,
        "`send --json` must print one document, not {}:\n{stdout}",
        documents.len()
    );
}

/// `--to <name@>` works for native coins, so the same command with `--currency`
/// reads as though it should work too. It does not, and the refusal has to
/// explain the difference rather than blame the node — an earlier version sent
/// the reader to `pecu doctor` for a node that was answering perfectly.
#[test]
#[ignore = "talks to api.verustest.net"]
fn a_token_payment_to_a_verusid_explains_itself_rather_than_blaming_the_node() {
    let Ok(funded) = std::env::var("PECU_FUNDED_HOME") else {
        eprintln!("PECU_FUNDED_HOME is not set — skipping");
        return;
    };
    let assertion = Command::cargo_bin("pecu")
        .expect("built")
        .env("PECU_HOME", &funded)
        .args([
            "send",
            "--to",
            "pecunft1@",
            "--amount",
            "1",
            "--currency",
            "pecuref9@",
            "--from",
            "faucet",
            "--dry-run",
        ])
        .assert()
        .failure();
    let stderr = String::from_utf8_lossy(&assertion.get_output().stderr).to_string();
    assert!(
        !stderr.contains("pecu doctor"),
        "the node is fine; this is a builder limit:\n{stderr}"
    );
    // Short phrases: miette wraps the help text.
    for expected in ["only name an R-address", "not the same thing"] {
        assert!(stderr.contains(expected), "missing {expected}:\n{stderr}");
    }
}
