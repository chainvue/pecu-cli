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
