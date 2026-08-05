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
fn the_total_reconciles_with_the_node_satoshi_for_satoshi() {
    use verus_sdk::network::{ChainReader, HttpTransport, RpcClient};

    // The bug this guards: an identity's funds live in pay-to-identity outputs,
    // which the SDK keeps out of the spendable bucket because no transparent key
    // can move them. Counting them and not summing them made this wallet report
    // zero for an address holding millions.
    let home = home();
    let assertion = pecu(&home)
        .args(["wallet", "balance", "--address", CHAIN_IDENTITY, "--json"])
        .assert()
        .success();
    let stdout = String::from_utf8_lossy(&assertion.get_output().stdout).into_owned();
    let ours: serde_json::Value = serde_json::from_str(&stdout).expect("valid json");

    let node = RpcClient::new(HttpTransport::new("https://api.verustest.net").expect("endpoint"));
    let theirs = node
        .address_balance(&[CHAIN_IDENTITY])
        .expect("the node should answer");

    assert_eq!(
        ours["total_satoshis"].as_u64().expect("a total"),
        theirs.balance.to_sat(),
        "our total disagrees with getaddressbalance:\n{ours:#}"
    );
    assert!(
        ours["held_for_identity"]["satoshis"].as_u64().unwrap_or(0) > 0,
        "an i-address holds its funds in identity payments; none were counted:\n{ours:#}"
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
fn the_mempool_is_actually_readable_through_the_public_node() {
    // The guard that matters most here, and the one an assertion on a *pending*
    // payment could never give: the read has to have happened. `getaddressmempool`
    // sits behind the same method allowlist as everything else on this endpoint,
    // and it is arity-sensitive — a second positional argument comes back -32601.
    // If either changes, `known` goes false and every balance quietly starts
    // reporting nothing in flight.
    let home = home();
    let assertion = pecu(&home)
        .args(["wallet", "balance", "--address", CHAIN_IDENTITY, "--json"])
        .assert()
        .success();
    let stdout = String::from_utf8_lossy(&assertion.get_output().stdout).into_owned();
    let document: serde_json::Value = serde_json::from_str(&stdout).expect("valid json");

    assert_eq!(
        document["pending"]["known"], true,
        "the mempool read failed, so this balance cannot say anything is pending:\n{document:#}"
    );
    // Whatever is in flight, the confirmed total must not have absorbed it.
    assert!(document["pending"]["transactions"].is_number());
    assert!(document["pending"]["net_satoshis"].is_number());
}

#[test]
#[ignore = "talks to api.verustest.net"]
fn utxos_say_whether_the_mempool_was_readable_at_all() {
    // `spent_in_mempool` reads `false` on every output when the read failed,
    // which is indistinguishable from "nothing is being spent". This flag is the
    // only thing that tells the two apart.
    let home = home();
    let assertion = pecu(&home)
        .args(["wallet", "utxos", "--address", CHAIN_IDENTITY, "--json"])
        .assert()
        .success();
    let stdout = String::from_utf8_lossy(&assertion.get_output().stdout).into_owned();
    let document: serde_json::Value = serde_json::from_str(&stdout).expect("valid json");

    assert_eq!(document["mempool_known"], true, "{document:#}");
}

/// The whole point of the feature, against a real transaction: broadcast a small
/// payment and read it back before any block contains it.
///
/// Costs a fee per run, so it needs `PECU_FUNDED_HOME` — a config root with a
/// funded key — and is skipped rather than failed without one.
#[test]
#[ignore = "spends real VRSCTEST; set PECU_FUNDED_HOME"]
fn a_payment_in_flight_is_visible_before_it_confirms() {
    let Ok(funded) = std::env::var("PECU_FUNDED_HOME") else {
        eprintln!("PECU_FUNDED_HOME is not set — skipping");
        return;
    };

    // One document per invocation, and the assertion says so: `send --json`
    // used to print two — the plan before broadcasting, the result after — so
    // parsing it as one document failed on the very command that spends money.
    let run = |args: &[&str]| {
        let assertion = Command::cargo_bin("pecu")
            .expect("built")
            .env("PECU_HOME", &funded)
            .args(args)
            .assert()
            .success();
        let stdout = String::from_utf8_lossy(&assertion.get_output().stdout).into_owned();
        let documents: Vec<serde_json::Value> = serde_json::Deserializer::from_str(&stdout)
            .into_iter()
            .map(|document| document.expect("valid json"))
            .collect();
        assert_eq!(
            documents.len(),
            1,
            "`pecu {}` printed {} json documents:\n{stdout}",
            args.join(" "),
            documents.len()
        );
        documents.into_iter().next().expect("one document")
    };

    let before = run(&["wallet", "balance", "--json"]);
    let address = before["address"].as_str().expect("an address").to_string();
    let confirmed = before["total_satoshis"].as_u64().expect("a total");

    // Refuse to start from a mempool that already has this address in it. Coin
    // selection reads the chain, the chain still shows the pending payment's
    // input as unspent, and the send below would be funded from it — a double
    // spend the node refuses with `bad-txns-inputs-spent`. Which is the failure
    // this feature exists to make visible, so hitting it here is not a reason to
    // work around it; it is a reason to wait for a block.
    if before["pending"]["transactions"].as_u64().unwrap_or(0) > 0 {
        eprintln!("{address} already has a payment in flight — skipping until it confirms");
        return;
    }

    // To itself, so one address sees the input being spent, the payment and the
    // change — and so the test costs a fee rather than a payment.
    let sent = run(&[
        "send", "--to", &address, "--amount", "0.001", "--yes", "--json",
    ]);
    let txid = sent["txid"].as_str().expect("a txid").to_string();

    let during = run(&["wallet", "balance", "--json"]);
    let pending = &during["pending"];
    assert_eq!(pending["known"], true, "{during:#}");
    assert!(
        pending["transactions"].as_u64().unwrap_or(0) >= 1,
        "the payment just broadcast is not in the mempool view:\n{during:#}"
    );
    // A self-send costs exactly its fee, and the fee leaves the address.
    assert!(
        pending["net_satoshis"].as_i64().unwrap_or(0) < 0,
        "a self-send should net out negative by the fee:\n{during:#}"
    );
    assert!(
        !pending["spending"].as_array().expect("spending").is_empty(),
        "the output being consumed was not reported:\n{during:#}"
    );
    // The confirmed figure is the chain's, and no block has this yet.
    assert_eq!(
        during["total_satoshis"].as_u64().expect("a total"),
        confirmed,
        "pending value leaked into the confirmed total:\n{during:#}"
    );

    let outputs = run(&["wallet", "utxos", "--json"]);
    let listed = outputs["outputs"].as_array().expect("outputs");
    assert!(
        listed
            .iter()
            .any(|output| output["status"] == "pending" && output["txid"] == txid.as_str()),
        "the unconfirmed outputs are missing from `wallet utxos`:\n{outputs:#}"
    );
    assert!(
        listed
            .iter()
            .any(|output| output["spent_in_mempool"] == true),
        "no confirmed output is marked as being spent:\n{outputs:#}"
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
