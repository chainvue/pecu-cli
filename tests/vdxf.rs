//! `pecu id publish` · `pecu id read`.
//!
//! Publishing rewrites an identity on chain and pays a fee, so the offline
//! tests are all guards: the things that must fail before a key is unlocked or
//! a transaction is built. The live test reads back what this project actually
//! published.

use assert_cmd::Command;
use predicates::str::contains;
use tempfile::TempDir;

const PASSPHRASE: &str = "correct horse battery staple";
const DEAD_NODE: &str = "https://127.0.0.1:1";

/// Registered by this project, and given a `greeting` key on 2026-08-05.
const OURS: &str = "pecucli7@";
const GREETING: &str = "hello from verus-pecu-cli";

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

fn generate(home: &TempDir, label: &str) {
    pecu(home)
        .args(["key", "gen", "--label", label])
        .assert()
        .success();
}

#[test]
fn mainnet_will_not_publish_without_being_told_to() {
    let home = home();
    generate(&home, "demo");
    pecu(&home)
        .args([
            "id",
            "publish",
            OURS,
            "greeting",
            "hi",
            "--profile",
            "mainnet",
        ])
        .assert()
        .failure()
        .stderr(contains("not allowed to spend"))
        .stderr(contains("allow_spend"));
}

#[test]
fn publishing_needs_a_key() {
    let home = home();
    pecu(&home)
        .args(["id", "publish", OURS, "greeting", "hi", "--node", DEAD_NODE])
        .assert()
        .failure()
        .stderr(contains("no key to sign with"));
}

#[test]
fn an_empty_value_is_refused_rather_than_read_as_a_deletion() {
    let home = home();
    generate(&home, "demo");
    // On chain an empty value and a removed key are the same thing. Guessing
    // which was meant is how a key gets deleted by accident.
    pecu(&home)
        .args(["id", "publish", OURS, "greeting", "", "--node", DEAD_NODE])
        .assert()
        .failure()
        .stderr(contains("needs a value"))
        .stderr(contains("--remove"));
}

#[test]
fn removing_and_giving_a_value_cannot_both_be_asked_for() {
    let home = home();
    generate(&home, "demo");
    pecu(&home)
        .args([
            "id", "publish", OURS, "greeting", "hi", "--remove", "--node", DEAD_NODE,
        ])
        .assert()
        .failure()
        .stderr(contains("cannot be used with"));
}

#[test]
fn a_missing_value_file_costs_no_passphrase_prompt() {
    let home = home();
    generate(&home, "demo");
    // The value is read before the keystore is touched, so a typo'd path fails
    // as a typo rather than after asking for a secret.
    pecu(&home)
        .env_remove("PECU_PASSPHRASE")
        .args([
            "id",
            "publish",
            OURS,
            "greeting",
            "@/nowhere/at/all.txt",
            "--node",
            DEAD_NODE,
        ])
        .assert()
        .failure()
        .stderr(contains("/nowhere/at/all.txt"));
}

#[test]
fn history_needs_a_key_to_have_a_history_of() {
    let home = home();
    // `--history` without a key would silently mean something else.
    pecu(&home)
        .args(["id", "read", OURS, "--history", "--node", DEAD_NODE])
        .assert()
        .failure()
        .stderr(contains("--history"));
}

#[test]
#[ignore = "talks to api.verustest.net"]
fn what_this_project_published_reads_back() {
    let home = home();
    let assertion = pecu(&home)
        .args(["id", "read", OURS, "greeting", "--json"])
        .assert()
        .success();
    let stdout = String::from_utf8_lossy(&assertion.get_output().stdout).into_owned();
    let document: serde_json::Value = serde_json::from_str(&stdout).expect("valid json");

    // The key address is derived locally from the name and the namespace, with
    // no node involved. If the derivation ever changes, this is what catches it
    // — the value would simply stop being found.
    assert_eq!(
        document["key_address"],
        "iHKP4SMTKchkNrsueWz3wguQxgJSZC3QE7"
    );

    let values = document["values"].as_array().expect("values");
    assert_eq!(values.len(), 1, "{document:#}");
    assert_eq!(values[0]["text"], GREETING);
    assert_eq!(values[0]["hex"], hex::encode(GREETING));
}

#[test]
#[ignore = "talks to api.verustest.net"]
fn listing_every_key_finds_the_one_that_is_there() {
    let home = home();
    let assertion = pecu(&home)
        .args(["id", "read", OURS, "--json"])
        .assert()
        .success();
    let stdout = String::from_utf8_lossy(&assertion.get_output().stdout).into_owned();
    let document: serde_json::Value = serde_json::from_str(&stdout).expect("valid json");

    let keys = document["keys"].as_array().expect("keys");
    assert!(
        keys.iter()
            .any(|key| key["key_address"] == "iHKP4SMTKchkNrsueWz3wguQxgJSZC3QE7"),
        "{document:#}"
    );
}

#[test]
#[ignore = "talks to api.verustest.net"]
fn a_key_nobody_wrote_is_an_answer_not_a_crash() {
    let home = home();
    pecu(&home)
        .args(["id", "read", OURS, "nothing-is-under-this-key"])
        .assert()
        .success()
        .stdout(contains("holds nothing"));
}

#[test]
#[ignore = "talks to api.verustest.net"]
fn the_same_name_derives_the_same_key_under_a_named_namespace() {
    let home = home();
    // The default namespace *is* the identity being read, so naming it
    // explicitly must land on the same key. If these ever disagree, one of the
    // two paths is deriving something nobody can find again.
    let read = |args: &[&str]| {
        let assertion = pecu(&home).args(args).assert().success();
        let stdout = String::from_utf8_lossy(&assertion.get_output().stdout).into_owned();
        serde_json::from_str::<serde_json::Value>(&stdout).expect("json")["key_address"]
            .as_str()
            .expect("a key address")
            .to_string()
    };

    assert_eq!(
        read(&["id", "read", OURS, "greeting", "--json"]),
        read(&[
            "id",
            "read",
            OURS,
            "greeting",
            "--namespace",
            OURS,
            "--json"
        ]),
    );
}
