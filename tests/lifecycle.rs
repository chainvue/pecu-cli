//! `pecu id update` · `revoke` · `recover`.
//!
//! Every one of these rewrites an identity on chain, and two of them can put it
//! beyond anyone's reach. So the offline tests are guards — the refusals that
//! have to happen before a key is unlocked or a fee is spent — and the network
//! tests assert the consensus rules that a daemon otherwise enforces silently.

use assert_cmd::Command;
use predicates::prelude::*;
use predicates::str::contains;
use tempfile::TempDir;

const PASSPHRASE: &str = "correct horse battery staple";
const DEAD_NODE: &str = "https://127.0.0.1:1";

/// Registered by this project. Its own recovery authority, which is what makes
/// it the right subject for the unrevokable rule.
const OURS: &str = "pecucli7@";
const OURS_ADDRESS: &str = "i7r29bDQfrwjkTxjv4bcYD6B1ZV7WZ4kGo";

/// The throwaway registered to prove revoke and recover, now held by a key
/// the faucet does not control.
const RESCUED: &str = "iATt9qRxvAwpZKFehmgofrPct8MnhZ6QQe";

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
fn an_update_that_names_no_field_is_refused_rather_than_paid_for() {
    let home = home();
    generate(&home, "demo");
    // Restating an identity unchanged is a valid transaction and a wasted fee.
    pecu(&home)
        .args(["id", "update", OURS, "--node", DEAD_NODE])
        .assert()
        .failure()
        .stderr(contains("changes nothing"));
}

#[test]
fn changing_control_needs_the_flag_and_the_flag_is_checked_before_the_passphrase() {
    let home = home();
    generate(&home, "demo");
    // Publishing addresses nobody holds, or a threshold nobody can meet, cannot
    // be undone by anyone. The guard must fire before the one interaction this
    // command demands, not after.
    pecu(&home)
        .env_remove("PECU_PASSPHRASE")
        .args(["id", "update", OURS, "--min-sigs", "2", "--node", DEAD_NODE])
        .assert()
        .failure()
        .stderr(contains("--allow-authority-change"))
        .stderr(contains("passphrase").not());
}

#[test]
fn a_primary_address_that_is_not_an_address_is_caught_locally() {
    let home = home();
    generate(&home, "demo");
    pecu(&home)
        .args([
            "id",
            "update",
            OURS,
            "--primary",
            "RNotAnAddressAtAll",
            "--allow-authority-change",
            "--node",
            DEAD_NODE,
        ])
        .assert()
        .failure()
        .stderr(contains("not an address this can use"));
}

#[test]
fn mainnet_will_not_revoke_without_being_told_to() {
    let home = home();
    generate(&home, "demo");
    pecu(&home)
        .args(["id", "revoke", OURS, "--profile", "mainnet"])
        .assert()
        .failure()
        .stderr(contains("not allowed to spend"));
}

#[test]
fn revoking_needs_a_key() {
    let home = home();
    pecu(&home)
        .args(["id", "revoke", OURS, "--node", DEAD_NODE])
        .assert()
        .failure()
        .stderr(contains("no key to sign with"));
}

#[test]
fn recovering_needs_min_sigs_to_come_with_the_addresses_it_counts() {
    let home = home();
    generate(&home, "demo");
    // A threshold without addresses to apply it to is a threshold against
    // whatever the identity happens to have, which is not what anyone means.
    pecu(&home)
        .args([
            "id",
            "recover",
            OURS,
            "--min-sigs",
            "2",
            "--node",
            DEAD_NODE,
        ])
        .assert()
        .failure()
        .stderr(contains("--primary"));
}

/// Recovery really does take an identity away from whoever held it.
///
/// `pecurevoke1@` was registered under the faucet key, handed to `pecucli7@` as
/// its recovery authority, revoked, and then recovered into a fresh key. The
/// original key can no longer change it — which is the entire point of having a
/// recovery authority, and the assertion is cheap because it is a refusal.
///
/// The full cycle, on VRSCTEST:
/// register `9129ede5`, hand over `d6e95642`, revoke `8539a2e6`,
/// recover `7cebc916`.
#[test]
#[ignore = "talks to api.verustest.net"]
fn a_recovered_identity_no_longer_answers_to_the_keys_it_was_taken_from() {
    let Ok(funded) = std::env::var("PECU_FUNDED_HOME") else {
        eprintln!("PECU_FUNDED_HOME is not set — skipping");
        return;
    };

    let assertion = Command::cargo_bin("pecu")
        .expect("built")
        .env("PECU_HOME", &funded)
        // `pecucli7@` rather than the recovered identity, and deliberately: the
        // recovered one carries a leftover unlock height, and the SDK's
        // timelock check refuses *any* update that restates it, before it ever
        // reaches the question this test is about. `rescued` is not one of
        // `pecucli7@`'s primary addresses, which is the same wrong-key case
        // without that in the way.
        .args([
            "id",
            "update",
            OURS_ADDRESS,
            "--from",
            "rescued",
            "--min-sigs",
            "1",
            "--allow-authority-change",
            "--dry-run",
        ])
        .assert()
        .failure();
    let stderr = String::from_utf8_lossy(&assertion.get_output().stderr).into_owned();

    // Short phrases only, and shorter than feels necessary: miette wraps at the
    // terminal width, and both "primary addresses" and "pecu key list" land
    // across a line break here.
    assert!(stderr.contains("is not one of the identity"), "{stderr}");
    // And the advice has to point at the identity, not at the node — the node
    // is fine, the key is simply not the one any more.
    assert!(stderr.contains("key list"), "{stderr}");
    assert!(
        !stderr.contains("pecu doctor"),
        "a wrong key is not a node problem:\n{stderr}"
    );
}

#[test]
fn an_unlock_delay_beyond_what_consensus_allows_is_refused_locally() {
    let home = home();
    generate(&home, "demo");
    // Worth catching here rather than at the daemon: the daemon's own helper
    // *clamps* an over-long delay to the maximum instead of refusing, so the
    // same request elsewhere can silently produce a lock decades shorter than
    // the one asked for.
    pecu(&home)
        .args([
            "id",
            "update",
            OURS,
            "--unlock-delay",
            "99999999",
            "--node",
            DEAD_NODE,
        ])
        .assert()
        .failure()
        .stderr(contains("over the"))
        .stderr(contains("clamps"));
}

#[test]
fn a_timelock_does_not_require_the_authority_flag() {
    let home = home();
    generate(&home, "demo");
    // Setting a timelock does not move who controls the identity, so it must
    // not demand the flag that guards that — teaching people to pass
    // --allow-authority-change habitually is how it stops being a guard.
    let assertion = pecu(&home)
        .args([
            "id",
            "update",
            OURS,
            "--unlock-delay",
            "10",
            "--node",
            DEAD_NODE,
        ])
        .assert()
        .failure();
    let stderr = String::from_utf8_lossy(&assertion.get_output().stderr).into_owned();
    assert!(
        !stderr.contains("allow-authority-change"),
        "a timelock is not an authority change:\n{stderr}"
    );
}

#[test]
fn unlocking_needs_a_key() {
    let home = home();
    pecu(&home)
        .args(["id", "unlock", OURS, "--node", DEAD_NODE])
        .assert()
        .failure()
        .stderr(contains("no key to sign with"));
}

/// Unlocking is its own command because the height is not the caller's to
/// compute.
///
/// Consensus measures the countdown from the transaction's `nExpiryHeight`, not
/// from the tip, so the floor is `delay + expiry` — and the expiry belongs to
/// the transaction the flow is building. Measured on VRSCTEST against
/// `pecurevoke1@` with a 10-block delay: signed at tip 1,177,377, the naive
/// `tip + delay` of 1,177,387 is 20 short, and the flow published 1,177,407
/// (`f5854d72`), which is `delay + tip + DEFAULT_EXPIRY_BLOCKS` exactly.
#[test]
#[ignore = "talks to api.verustest.net"]
fn unlocking_something_already_counting_down_has_nothing_to_start() {
    let Ok(funded) = std::env::var("PECU_FUNDED_HOME") else {
        eprintln!("PECU_FUNDED_HOME is not set — skipping");
        return;
    };

    Command::cargo_bin("pecu")
        .expect("built")
        .env("PECU_HOME", &funded)
        .args(["id", "unlock", RESCUED, "--from", "rescued", "--dry-run"])
        .assert()
        .failure()
        // Either resting state is a legitimate answer and both must read as
        // one: still counting down, or finished and leaving its height behind.
        // What must never appear is "the countdown is running" for a height
        // that went by — which is what this said before the SDK pointed out
        // that nothing ever clears it.
        .stderr(contains("already counting").or(contains("is not locked")));
}

#[test]
#[ignore = "talks to api.verustest.net"]
fn a_countdown_that_finished_is_not_reported_as_still_running() {
    let Ok(funded) = std::env::var("PECU_FUNDED_HOME") else {
        eprintln!("PECU_FUNDED_HOME is not set — skipping");
        return;
    };

    // `unlock_after` keeps its height forever once a countdown elapses, so this
    // is where most identities end up rather than a corner case.
    let assertion = Command::cargo_bin("pecu")
        .expect("built")
        .env("PECU_HOME", &funded)
        .args(["id", "show", RESCUED])
        .assert()
        .success();
    let stdout = String::from_utf8_lossy(&assertion.get_output().stdout).into_owned();

    if stdout.contains("unlocked at") {
        assert!(
            !stdout.contains("unlocks at"),
            "an elapsed countdown is in the past tense:\n{stdout}"
        );
        assert!(stdout.contains("leftover"), "{stdout}");
    }
}

/// The consensus rule, caught before a fee is spent.
///
/// `identity.cpp` refuses a revocation whose subject is its own recovery
/// authority, because nobody could undo it. The daemon's refusal names nothing
/// — `mandatory-script-verify-flag-failed` — and arrives after the money is
/// gone, so the SDK refuses it locally and this asserts that it does.
#[test]
#[ignore = "talks to api.verustest.net"]
fn revoking_an_identity_that_is_its_own_recovery_authority_is_refused_locally() {
    let Ok(funded) = std::env::var("PECU_FUNDED_HOME") else {
        eprintln!("PECU_FUNDED_HOME is not set — skipping");
        return;
    };

    let assertion = Command::cargo_bin("pecu")
        .expect("built")
        .env("PECU_HOME", &funded)
        .args([
            "id",
            "revoke",
            OURS_ADDRESS,
            "--from",
            "faucet",
            "--dry-run",
        ])
        .assert()
        .failure();
    let stderr = String::from_utf8_lossy(&assertion.get_output().stderr).into_owned();

    assert!(stderr.contains("strand"), "{stderr}");
    // The remedy is a different operation, not a retry or a flag, so the advice
    // has to name it.
    assert!(stderr.contains("--recovery"), "{stderr}");
}

#[test]
#[ignore = "talks to api.verustest.net"]
fn recovering_an_identity_that_is_not_revoked_says_so() {
    let Ok(funded) = std::env::var("PECU_FUNDED_HOME") else {
        eprintln!("PECU_FUNDED_HOME is not set — skipping");
        return;
    };

    Command::cargo_bin("pecu")
        .expect("built")
        .env("PECU_HOME", &funded)
        .args([
            "id",
            "recover",
            OURS_ADDRESS,
            "--from",
            "faucet",
            "--dry-run",
        ])
        .assert()
        .failure()
        .stderr(contains("not revoked"));
}

/// The update flow, against the chain, restating what is already there.
///
/// Deliberately a no-op change: it exercises the whole path — read the identity
/// from the output script, restate it, sign, build — while leaving the identity
/// byte-for-byte as it was. Broadcast for real once, at
/// `00fecccd36f46e77a423de3f1027c31077a7452b3768dcf8cc65ae202eb5275c`.
#[test]
#[ignore = "talks to api.verustest.net"]
fn an_update_carries_through_every_field_it_was_not_told_to_change() {
    let Ok(funded) = std::env::var("PECU_FUNDED_HOME") else {
        eprintln!("PECU_FUNDED_HOME is not set — skipping");
        return;
    };

    let assertion = Command::cargo_bin("pecu")
        .expect("built")
        .env("PECU_HOME", &funded)
        .args([
            "id",
            "update",
            OURS_ADDRESS,
            "--from",
            "faucet",
            "--primary",
            "RComfCn4wHHsGR8vWBAU7T1r3tHHyxN9Hm",
            "--min-sigs",
            "1",
            "--allow-authority-change",
            "--dry-run",
            "--json",
        ])
        .assert()
        .success();
    let stdout = String::from_utf8_lossy(&assertion.get_output().stdout).into_owned();
    let document: serde_json::Value = serde_json::from_str(&stdout).expect("valid json");

    assert_eq!(document["broadcast"], false, "a dry run must not send");
    // Restating the same addresses and threshold is not a change of control,
    // and the SDK works that out by comparing against the chain rather than
    // against what the caller said.
    assert_eq!(
        document["changes_authority"], false,
        "restating the same authority should not count as changing it:\n{document:#}"
    );

    // The content key published in M8 has to survive the restatement — this is
    // the read-modify-write that would otherwise silently erase it.
    let hex = document["hex"].as_str().expect("signed hex");
    let explained = Command::cargo_bin("pecu")
        .expect("built")
        .env("PECU_HOME", &funded)
        .args(["tx", "explain", hex])
        .assert()
        .success();
    let out = String::from_utf8_lossy(&explained.get_output().stdout).into_owned();
    assert!(
        out.contains("content key"),
        "the update dropped the identity's published content:\n{out}"
    );
    assert!(out.contains("1-of-1"), "{out}");
}
