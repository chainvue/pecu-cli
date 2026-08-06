//! `pecu currency show` · `pecu currency launch`.
//!
//! A launch is one-way — the defining identity can never define another — and
//! costs 200 VRSCTEST, so the offline tests are the refusals that must happen
//! before any of that, and the network tests read rather than write.

use assert_cmd::Command;
use predicates::prelude::*;
use predicates::str::contains;
use tempfile::TempDir;

const PASSPHRASE: &str = "correct horse battery staple";
const DEAD_NODE: &str = "https://127.0.0.1:1";

/// A real token on VRSCTEST: `options = 32`, `proofprotocol = 2`.
const TOKEN: &str = "iK2k8YH1jfR7RLmEZ3zac2Mkx5rxSgbMqg";

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
fn mainnet_will_not_launch_without_being_told_to() {
    let home = home();
    generate(&home, "demo");
    pecu(&home)
        .args(["currency", "launch", "alice@", "--profile", "mainnet"])
        .assert()
        .failure()
        .stderr(contains("not allowed to spend"));
}

#[test]
fn launching_needs_a_key() {
    let home = home();
    pecu(&home)
        .args(["currency", "launch", "alice@", "--node", DEAD_NODE])
        .assert()
        .failure()
        .stderr(contains("no key to sign with"));
}

#[test]
fn a_preallocation_to_a_transparent_address_is_refused_before_anything_else() {
    let home = home();
    generate(&home, "demo");
    // The supply is held by whoever controls an identity, so a preallocation
    // names one. Caught before the keystore is touched.
    pecu(&home)
        .env_remove("PECU_PASSPHRASE")
        .args([
            "currency",
            "launch",
            "alice@",
            "--preallocate",
            "RComfCn4wHHsGR8vWBAU7T1r3tHHyxN9Hm:100",
            "--node",
            DEAD_NODE,
        ])
        .assert()
        .failure()
        .stderr(contains("is not `address:amount`"))
        .stderr(contains("passphrase").not());
}

#[test]
fn a_start_block_and_a_start_offset_cannot_both_be_given() {
    let home = home();
    generate(&home, "demo");
    pecu(&home)
        .args([
            "currency",
            "launch",
            "alice@",
            "--start-block",
            "1200000",
            "--start-in",
            "50",
            "--node",
            DEAD_NODE,
        ])
        .assert()
        .failure()
        .stderr(contains("cannot be used with"));
}

/// The bitfield and the proof protocol are what a currency *is*, and neither is
/// inferable from its name. Reading `options: 32` off a panel tells nobody
/// anything.
#[test]
#[ignore = "talks to api.verustest.net"]
fn a_currency_reads_back_decoded_rather_than_as_a_number() {
    let home = home();
    let assertion = pecu(&home)
        .args(["currency", "show", TOKEN, "--json"])
        .assert()
        .success();
    let stdout = String::from_utf8_lossy(&assertion.get_output().stdout).into_owned();
    let document: serde_json::Value = serde_json::from_str(&stdout).expect("valid json");

    assert_eq!(document["currency_id"], TOKEN);
    assert_eq!(document["options"], 32);
    // Raw *and* decoded: a consumer should not need the bit values to use this.
    assert_eq!(document["kinds"][0], "token");
    assert_eq!(document["proof_protocol"], 2);
    assert!(
        document["control"]
            .as_str()
            .unwrap_or_default()
            .contains("can mint"),
        "{document:#}"
    );
    // The raw definition is carried through for anything this build does not
    // decode yet.
    assert!(document["definition"].is_object(), "{document:#}");
}

/// `getcurrency` takes a bare name; every other command here takes the `@` form.
#[test]
#[ignore = "talks to api.verustest.net"]
fn a_currency_is_findable_with_or_without_the_at_sign() {
    let home = home();
    let id = |args: &[&str]| {
        let assertion = pecu(&home).args(args).assert().success();
        let stdout = String::from_utf8_lossy(&assertion.get_output().stdout).into_owned();
        serde_json::from_str::<serde_json::Value>(&stdout).expect("json")["currency_id"]
            .as_str()
            .expect("an id")
            .to_string()
    };
    assert_eq!(
        id(&["currency", "show", "TST", "--json"]),
        id(&["currency", "show", "TST@", "--json"])
    );
}

#[test]
#[ignore = "talks to api.verustest.net"]
fn a_currency_nobody_defined_is_an_answer_not_a_crash() {
    let home = home();
    pecu(&home)
        .args(["currency", "show", "nothing-is-called-this-surely@"])
        .assert()
        .failure()
        .stderr(contains("nothing on this chain"));
}

/// `--supply` has to become a preallocation, because a token's supply is the
/// sum of its preallocations and `initial_supply` is read only for a fractional
/// currency. Setting the wrong field launches a token with no supply at all.
#[test]
#[ignore = "talks to api.verustest.net"]
fn supply_lands_where_the_chain_actually_reads_it() {
    let Ok(funded) = std::env::var("PECU_FUNDED_HOME") else {
        eprintln!("PECU_FUNDED_HOME is not set — skipping");
        return;
    };

    let assertion = Command::cargo_bin("pecu")
        .expect("built")
        .env("PECU_HOME", &funded)
        // `pecudepth2@` rather than `pecudepth3@`: the latter defined a
        // currency at `2fecffbb…` and so can never define another, which
        // refuses before this test reaches its question. A dry run consumes
        // nothing, so the slot stays free for whoever wants it.
        .args([
            "currency",
            "launch",
            "pecudepth2@",
            "--from",
            "faucet",
            "--supply",
            "1000000",
            "--dry-run",
            "--json",
        ])
        .assert()
        .success();
    let stdout = String::from_utf8_lossy(&assertion.get_output().stdout).into_owned();
    let document: serde_json::Value = serde_json::from_str(&stdout).expect("valid json");

    assert_eq!(document["broadcast"], false);
    assert_eq!(document["supply"], 100_000_000_000_000u64);
    // The currency's id is the defining identity's own i-address — this one is
    // `pecudepth2@`'s, which is the whole of what "a currency is something an
    // identity becomes" means in practice.
    assert_eq!(
        document["currency_id"],
        "iSHPgvF7f4huHK5WZ52tURDkZxbkCvsYke"
    );
    assert_eq!(document["mintable"], false);
}

/// The one-way rule, and the advice that goes with it.
///
/// `pecudepth3@` defined a currency at `2fecffbb…`, so it can never define
/// another. The refusal arrives as `FlowError::NotReady`, not `Content` —
/// matching the wrong variant sent this to "run `pecu doctor`", which is wrong
/// twice over: the node is fine and no retry helps.
#[test]
#[ignore = "talks to api.verustest.net"]
fn an_identity_that_already_defines_a_currency_is_refused_by_name() {
    let Ok(funded) = std::env::var("PECU_FUNDED_HOME") else {
        eprintln!("PECU_FUNDED_HOME is not set — skipping");
        return;
    };

    let assertion = Command::cargo_bin("pecu")
        .expect("built")
        .env("PECU_HOME", &funded)
        .args([
            "currency",
            "launch",
            "pecudepth3@",
            "--from",
            "faucet",
            "--supply",
            "5",
            "--dry-run",
        ])
        .assert()
        .failure();
    let stderr = String::from_utf8_lossy(&assertion.get_output().stderr).into_owned();

    assert!(stderr.contains("already defines a currency"), "{stderr}");
    assert!(stderr.contains("Register another"), "{stderr}");
    assert!(
        !stderr.contains("pecu doctor"),
        "a one-way rule is not a node problem:\n{stderr}"
    );
}

/// What this project launched, read back through its own command.
#[test]
#[ignore = "talks to api.verustest.net"]
fn the_currency_this_project_launched_reads_back() {
    let home = home();
    let assertion = pecu(&home)
        .args(["currency", "show", "pecudepth3@", "--json"])
        .assert()
        .success();
    let stdout = String::from_utf8_lossy(&assertion.get_output().stdout).into_owned();
    let document: serde_json::Value = serde_json::from_str(&stdout).expect("valid json");

    // The currency's id is the defining identity's own i-address.
    assert_eq!(
        document["currency_id"],
        "i7kDJurgpZA63cjPTuyK49CeCKihB5ryDB"
    );
    assert_eq!(document["kinds"][0], "token");
    // Decentralized: `--mintable` was not passed, so the supply is fixed.
    assert_eq!(document["proof_protocol"], 1);

    // The supply is the preallocation, which is the trap `--supply` exists to
    // avoid: setting `initial_supply` here would have launched it empty.
    let preallocations = document["definition"]["preallocations"]
        .as_array()
        .expect("preallocations");
    assert_eq!(preallocations.len(), 1, "{document:#}");
}
