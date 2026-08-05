//! `pecu id show` · `pecu id register`.
//!
//! The offline tests cover the guards and the resumption logic — which is the
//! part that can lose money, since the salt in a saved registration cannot be
//! recovered from anywhere. The network tests read real identities; nothing here
//! registers one, because that burns 100 VRSCTEST per run.

use assert_cmd::Command;
use predicates::prelude::*;
use predicates::str::contains;
use tempfile::TempDir;

const PASSPHRASE: &str = "correct horse battery staple";
const DEAD_NODE: &str = "https://127.0.0.1:1";

/// The chain's own identity: always present, and its own revocation authority.
const CHAIN_IDENTITY: &str = "VRSCTEST@";

/// Registered by this project on 2026-08-05 to verify the two-phase flow.
const OURS: &str = "pecucli7@";

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
fn mainnet_will_not_register_without_being_told_to() {
    let home = home();
    generate(&home, "demo");
    pecu(&home)
        .args(["id", "register", "alice", "--profile", "mainnet"])
        .assert()
        .failure()
        .stderr(contains("not allowed to spend"))
        .stderr(contains("allow_spend"));
}

#[test]
fn a_sub_identity_is_refused_rather_than_half_supported() {
    let home = home();
    generate(&home, "demo");
    // `alice.bob@` is a different registration shape. Guessing at it would
    // spend a commitment fee on something that cannot complete.
    for name in ["alice.bob", "alice.bob@", "a@b"] {
        pecu(&home)
            .args(["id", "register", name, "--node", DEAD_NODE])
            .assert()
            .failure()
            .stderr(contains("not a name this can register"));
    }
}

#[test]
fn an_empty_name_is_refused() {
    let home = home();
    generate(&home, "demo");
    pecu(&home)
        .args(["id", "register", "@", "--node", DEAD_NODE])
        .assert()
        .failure()
        .stderr(contains("not a name this can register"));
}

#[test]
fn registering_needs_a_key() {
    let home = home();
    pecu(&home)
        .args(["id", "register", "alice", "--node", DEAD_NODE])
        .assert()
        .failure()
        .stderr(contains("no key to register with"));
}

#[test]
fn a_saved_registration_is_picked_up_rather_than_started_again() {
    let home = home();
    generate(&home, "demo");

    // A registration in progress, as `id register` would have written it. The
    // resumption path must find this and poll it — starting over would abandon
    // a commitment that has already been paid for.
    let pending = home.path().join("pending");
    std::fs::create_dir_all(&pending).expect("writable");
    std::fs::write(pending.join("alice.json"), SAVED_REGISTRATION).expect("writable");

    let assertion = pecu(&home)
        .args(["id", "register", "alice", "--node", DEAD_NODE])
        .assert()
        .failure();
    let stderr = String::from_utf8_lossy(&assertion.get_output().stderr).into_owned();

    // It reached the *poll*, not the prepare: a fresh start would have failed
    // asking for a passphrase or checking whether the name is taken.
    assert!(
        stderr.contains("checking the commitment"),
        "did not resume the saved registration:\n{stderr}"
    );
}

#[test]
fn a_corrupt_saved_registration_is_not_silently_started_over() {
    let home = home();
    generate(&home, "demo");
    let pending = home.path().join("pending");
    std::fs::create_dir_all(&pending).expect("writable");
    std::fs::write(pending.join("alice.json"), "{ not json").expect("writable");

    // Starting over here would broadcast a second commitment and pay twice.
    pecu(&home)
        .args(["id", "register", "alice", "--node", DEAD_NODE])
        .assert()
        .failure()
        .stderr(contains("not a registration this version understands"));
}

#[test]
fn the_saved_registration_is_matched_case_insensitively() {
    let home = home();
    generate(&home, "demo");
    let pending = home.path().join("pending");
    std::fs::create_dir_all(&pending).expect("writable");
    std::fs::write(pending.join("alice.json"), SAVED_REGISTRATION).expect("writable");

    // `Alice` and `alice` are the same claim; resuming must not depend on how
    // it was typed the second time.
    pecu(&home)
        .args(["id", "register", "Alice@", "--node", DEAD_NODE])
        .assert()
        .failure()
        .stderr(contains("checking the commitment"));
}

#[test]
#[ignore = "talks to api.verustest.net"]
fn an_identity_that_is_its_own_revocation_authority_is_flagged() {
    let home = home();
    let assertion = pecu(&home)
        .args(["id", "show", CHAIN_IDENTITY, "--theme", "phosphor"])
        .assert()
        .success();
    let stdout = String::from_utf8_lossy(&assertion.get_output().stdout).into_owned();

    // Unrevokable, and not obvious from the address alone — the same string
    // appears three times and the reader has to notice.
    assert!(stdout.contains("(itself)"), "{stdout}");
    assert!(stdout.contains("revocation"), "{stdout}");
    assert!(stdout.contains("1-of-1"), "{stdout}");
}

#[test]
#[ignore = "talks to api.verustest.net"]
fn the_identity_this_project_registered_is_there() {
    let home = home();
    let assertion = pecu(&home)
        .args(["id", "show", OURS, "--json"])
        .assert()
        .success();
    let stdout = String::from_utf8_lossy(&assertion.get_output().stdout).into_owned();
    let document: serde_json::Value = serde_json::from_str(&stdout).expect("valid json");

    assert_eq!(document["name"], "pecucli7.VRSCTEST@");
    assert_eq!(
        document["identity_address"],
        "i7r29bDQfrwjkTxjv4bcYD6B1ZV7WZ4kGo"
    );
    assert_eq!(document["revoked"], false);
}

#[test]
#[ignore = "talks to api.verustest.net"]
fn a_name_nobody_registered_is_an_answer_not_a_crash() {
    let home = home();
    pecu(&home)
        .args(["id", "show", "nothing-is-called-this-surely@"])
        .assert()
        .failure()
        .stderr(contains("nothing on this chain is called"));
}

#[test]
#[ignore = "talks to api.verustest.net"]
fn a_taken_name_is_refused_before_a_passphrase_is_asked_for() {
    let home = home();
    generate(&home, "demo");
    pecu(&home)
        .env_remove("PECU_PASSPHRASE")
        .args(["id", "register", "pecucli7"])
        .assert()
        .failure()
        .stderr(contains("already registered"))
        .stderr(contains("passphrase").not());
}

/// A real `Pending<AwaitingCommitment>`, captured from an actual `id register`
/// step one and then neutralised: the name is `alice` and the commitment txid
/// points at something no node has ever seen.
///
/// Captured rather than hand-written. The shape is the SDK's and it has fields a
/// reader would not guess — `system_id`, `referral_levels`, `anchored_at` — and
/// a fixture that fails to deserialise would make these tests pass for the wrong
/// reason, by taking the corrupt-file path.
const SAVED_REGISTRATION: &str = include_str!("../fixtures/pending-registration.json");
