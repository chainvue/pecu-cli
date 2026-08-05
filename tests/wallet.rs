//! `pecu wallet …`.
//!
//! The address-resolution rules are offline and are what most of this asserts —
//! getting them wrong means reporting the wrong address's balance, which is a
//! worse failure than reporting none. The two tests that read a real balance are
//! `#[ignore]`d and run under `make testnet`.

use assert_cmd::Command;
use predicates::str::contains;
use tempfile::TempDir;

const PASSPHRASE: &str = "correct horse battery staple";

/// The VRSCTEST chain's own currency identity. It holds tokens and a set of
/// outputs the node reports as unspendable, which is exactly the shape a naive
/// balance gets wrong.
const CHAIN_IDENTITY: &str = "iJhCezBExJHvtyH3fGhNnt2NhU4Ztkf2yq";

const DEAD_NODE: &str = "https://127.0.0.1:1";

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
fn an_empty_keystore_and_no_address_says_what_to_do() {
    let home = home();
    pecu(&home)
        .args(["wallet", "balance"])
        .assert()
        .failure()
        .stderr(contains("no address to look at"))
        .stderr(contains("pecu key gen"));
}

#[test]
fn a_sole_key_is_the_default() {
    let home = home();
    let address = generate(&home, "only");
    // Reaches the node, which is dead, but only after resolving the address —
    // and the address is in the output.
    pecu(&home)
        .args(["wallet", "balance", "--node", DEAD_NODE])
        .assert()
        .failure()
        .stderr(contains(address));
}

#[test]
fn several_keys_are_refused_rather_than_guessed_between() {
    let home = home();
    generate(&home, "one");
    generate(&home, "two");
    pecu(&home)
        .args(["wallet", "balance"])
        .assert()
        .failure()
        .stderr(contains("no obvious default"))
        .stderr(contains("--key"));
}

#[test]
fn a_named_key_resolves_to_its_address() {
    let home = home();
    generate(&home, "one");
    let wanted = generate(&home, "two");
    pecu(&home)
        .args(["wallet", "balance", "--key", "two", "--node", DEAD_NODE])
        .assert()
        .failure()
        .stderr(contains(wanted));
}

#[test]
fn a_typod_address_is_caught_before_the_node_sees_it() {
    let home = home();
    // The dangerous failure this prevents: an address that is not an address
    // comes back from a node as an empty balance, which reads as "no funds".
    pecu(&home)
        .args(["wallet", "balance", "--address", "RNotARealAddressAtAll"])
        .assert()
        .failure()
        .stderr(contains("is not a Verus address"));
}

#[test]
fn address_and_key_cannot_both_be_given() {
    let home = home();
    generate(&home, "one");
    pecu(&home)
        .args([
            "wallet",
            "balance",
            "--address",
            CHAIN_IDENTITY,
            "--key",
            "one",
        ])
        .assert()
        .failure()
        .stderr(contains("cannot be used with"));
}

#[test]
#[ignore = "talks to api.verustest.net"]
fn a_real_balance_separates_spendable_withheld_and_tokens() {
    let home = home();
    let assertion = pecu(&home)
        .args(["wallet", "balance", "--address", CHAIN_IDENTITY, "--json"])
        .assert()
        .success();
    let stdout = String::from_utf8_lossy(&assertion.get_output().stdout).into_owned();
    let document: serde_json::Value = serde_json::from_str(&stdout).expect("valid json");

    assert_eq!(document["address"], CHAIN_IDENTITY);
    assert!(document["tip"].as_u64().unwrap_or(0) > 1_000_000);
    // Three separate figures, not one. The chain identity's satoshi total is
    // zero while it holds thousands in tokens — the exact case a wallet that
    // reads only the satoshi column gets wrong.
    assert!(document["spendable"]["satoshis"].is_number());
    assert!(document["withheld"]["outputs"].as_u64().unwrap_or(0) > 0);
    assert_eq!(document["tokens"]["known"], true);
    assert!(
        !document["tokens"]["balances"]
            .as_array()
            .expect("balances")
            .is_empty(),
        "the chain identity should hold tokens: {document}"
    );
}

#[test]
#[ignore = "talks to api.verustest.net"]
fn utxos_list_the_outputs_behind_the_balance() {
    let home = home();
    let assertion = pecu(&home)
        .args(["wallet", "utxos", "--address", CHAIN_IDENTITY, "--json"])
        .assert()
        .success();
    let stdout = String::from_utf8_lossy(&assertion.get_output().stdout).into_owned();
    let document: serde_json::Value = serde_json::from_str(&stdout).expect("valid json");

    let outputs = document["outputs"].as_array().expect("outputs");
    assert!(!outputs.is_empty());
    assert!(
        outputs
            .iter()
            .all(|output| output["txid"].is_string() && output["satoshis"].is_number()),
        "{document}"
    );
}

#[test]
#[ignore = "talks to api.verustest.net"]
fn an_address_with_too_many_outputs_says_which_setting_to_raise() {
    let home = home();
    // A long-lived VRSCTEST mining address: its getaddressutxos reply is ~85 MB,
    // far past the SDK's 8 MiB memory bound. The node is not broken and the URL
    // is not wrong, so the advice must not say either.
    pecu(&home)
        .args([
            "wallet",
            "balance",
            "--address",
            "iP6FybPsi3s6eLi3Sh8TNH3Pz41uoSYezv",
        ])
        .assert()
        .failure()
        // Short phrases only: miette wraps the help text, so anything longer
        // than a few words is at the mercy of where the line breaks.
        .stderr(contains("max_response_mb"))
        .stderr(contains("MiB ceiling"))
        .stderr(contains("iP6FybPsi3s6eLi3Sh8TNH3Pz41uoSYezv"));
}
