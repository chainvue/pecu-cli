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

/// miette hard-wraps to the terminal width, and where the wrap lands depends on
/// how long the temp directory's path is — which differs between a macOS
/// `/var/folders/…` run and a Linux `/tmp/…` one. A phrase that sits on one line
/// on the machine the test was written on straddles two on the machine that runs
/// it next, and the assertion fails for a reason that has nothing to do with the
/// behaviour under test. Flattening first makes a multi-word assertion mean what
/// it says instead of testing the wrap.
///
/// The `\u{2502}` is dropped as well as the whitespace: miette does not merely
/// break the line, it prefixes every continuation with a gutter, so a wrapped
/// sentence reads `this version \u{2502} understands` and collapsing spaces alone
/// still leaves the glyph sitting inside the phrase being matched.
///
/// The same helper, for the same reason, exists in `tests/currency.rs`.
fn flat(text: &str) -> String {
    text.replace('\u{2502}', " ")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

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
        stderr.contains("waiting for the commitment") || stderr.contains("checking the commitment"),
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
    let assertion = pecu(&home)
        .args(["id", "register", "alice", "--node", DEAD_NODE])
        .assert()
        .failure();
    let stderr = flat(&String::from_utf8_lossy(&assertion.get_output().stderr));
    assert!(
        stderr.contains("is not a registration this version understands"),
        "did not refuse the corrupt saved registration:\n{stderr}"
    );
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
        .stderr(contains("the commitment"));
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

    // Not obvious from the address alone — the same string appears three times
    // and the reader has to notice.
    assert!(stdout.contains("(itself)"), "{stdout}");
    assert!(stdout.contains("revocation"), "{stdout}");
    assert!(stdout.contains("1-of-1"), "{stdout}");

    // The consensus rule, and the fact this output was wrong about twice. An
    // identity that is its own *recovery* authority cannot be revoked at all —
    // `identity.cpp` refuses a revocation nobody could undo. This project first
    // stated that as permanent for *both* authorities, then over-corrected and
    // dropped it entirely, which traded a wrong claim for a missing one.
    assert!(
        stdout.contains("cannot be revoked"),
        "an identity that is its own recovery authority is unrevokable, and the output does \
         not say so:\n{stdout}"
    );
    // The half that really was false: the authorities are not frozen. Primary
    // keys can hand either one to another VerusID — one-way.
    assert!(
        !stdout.contains("cannot be changed"),
        "the output claims authorities are unchangeable, which is false:\n{stdout}"
    );
    assert!(stdout.contains("cannot take it back"), "{stdout}");
}

/// `flags` is a bitfield, and reading it whole would freeze currency-bearing
/// identities out of their own funds.
///
/// `VRSCTEST@` reports `flags = 1`, which is `FLAG_ACTIVE_CURRENCY` — it has
/// launched a currency — and not `FLAG_LOCKED`, which is `2`. Testing the field
/// against zero rather than masking would call it locked. Pinned to that exact
/// value on the SDK developer's warning.
#[test]
#[ignore = "talks to api.verustest.net"]
fn a_currency_bearing_identity_is_not_mistaken_for_a_locked_one() {
    let home = home();
    let assertion = pecu(&home)
        .args(["id", "show", CHAIN_IDENTITY, "--json"])
        .assert()
        .success();
    let stdout = String::from_utf8_lossy(&assertion.get_output().stdout).into_owned();
    let document: serde_json::Value = serde_json::from_str(&stdout).expect("valid json");

    assert_eq!(
        document["identity"]["flags"], 1,
        "the premise of this test has changed:\n{document:#}"
    );
    assert_eq!(document["timelock"]["kind"], "none", "{document:#}");
    assert_eq!(document["timelock"]["spendable"], true, "{document:#}");
}

/// Every identity says whether it is locked, including the ones that are not.
///
/// This was omitted for unlocked identities on the reasoning that the section's
/// presence was the signal. Silence is not a signal: it reads the same as not
/// having looked, and whether funds can move is not a question to answer only
/// sometimes.
#[test]
#[ignore = "talks to api.verustest.net"]
fn an_identity_that_was_never_locked_still_says_so() {
    let home = home();
    let assertion = pecu(&home)
        .args(["id", "show", CHAIN_IDENTITY])
        .assert()
        .success();
    let stdout = String::from_utf8_lossy(&assertion.get_output().stdout).into_owned();

    assert!(
        stdout.contains("TIMELOCK"),
        "no timelock section, so the reader cannot tell unlocked from unexamined:\n{stdout}"
    );
    // "never locked" and "unlocked" are different facts — the second leaves a
    // height behind — so whichever it is, it must be stated.
    assert!(
        stdout.contains("never locked") || stdout.contains("unlocked") || stdout.contains("locked"),
        "{stdout}"
    );
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

/// A dry run must leave nothing behind.
///
/// The saved registration is what the next run resumes. One written for a
/// commitment that was never broadcast would send that run to poll for a
/// transaction nobody made.
#[test]
#[ignore = "talks to api.verustest.net"]
fn a_dry_run_registration_saves_nothing_and_sends_nothing() {
    let Ok(funded) = std::env::var("PECU_FUNDED_HOME") else {
        eprintln!("PECU_FUNDED_HOME is not set — skipping");
        return;
    };

    let assertion = Command::cargo_bin("pecu")
        .expect("built")
        .env("PECU_HOME", &funded)
        .args([
            "id",
            "register",
            "pecudryrun1",
            "--from",
            "faucet",
            "--dry-run",
            "--json",
        ])
        .assert()
        .success();
    let stdout = String::from_utf8_lossy(&assertion.get_output().stdout).into_owned();
    let document: serde_json::Value = serde_json::from_str(&stdout).expect("valid json");

    assert_eq!(document["broadcast"], false);
    assert_eq!(document["kind"], "estimate");
    assert_eq!(document["registration_fee"], 10_000_000_000u64);

    let saved = std::path::Path::new(&funded)
        .join("pending")
        .join("pecudryrun1.json");
    assert!(
        !saved.exists(),
        "a dry run wrote {}, which the next run would try to resume",
        saved.display()
    );
}

/// A referral makes the registrant pay *less*, and the panel has to say so.
///
/// `registration_fee` is chain policy before any discount. Showing it beside a
/// referral overstated the cost by a fifth and described money as burned when
/// part of it is a payment to the referrer.
#[test]
#[ignore = "talks to api.verustest.net"]
fn a_referral_reduces_the_outlay_and_the_split_is_shown() {
    let Ok(funded) = std::env::var("PECU_FUNDED_HOME") else {
        eprintln!("PECU_FUNDED_HOME is not set — skipping");
        return;
    };

    let assertion = Command::cargo_bin("pecu")
        .expect("built")
        .env("PECU_HOME", &funded)
        .args([
            "id",
            "register",
            "pecudryrun2",
            "--from",
            "faucet",
            "--referral",
            OURS,
            "--dry-run",
        ])
        .assert()
        .success();
    let stdout = String::from_utf8_lossy(&assertion.get_output().stdout).into_owned();

    // VRSCTEST: 100 over 3 levels is 20 to each referrer, 80 paid, 60 burned.
    assert!(stdout.contains("80.00000000"), "{stdout}");
    assert!(stdout.contains("20.00000000"), "{stdout}");
    assert!(stdout.contains("60.00000000"), "{stdout}");
    assert!(stdout.contains("across 1 level"), "{stdout}");
    // And it must not still call the whole undiscounted fee burned.
    assert!(
        !stdout.contains("100.00000000 VRSCTEST  burned"),
        "the undiscounted fee is still described as burned:\n{stdout}"
    );
}

/// A referrer who was itself referred is paid too, and the outlay does not move.
///
/// Only the split does: at depth 2 the same 80 buys two 20-coin payouts and 40
/// burned, rather than one payout and 60 burned. Proven on chain by
/// `6ab375a6…`, whose registration carries two payout outputs nearest-first.
/// `pecuref9@` is referred by `pecucli7@`, which is what makes the chain two
/// deep.
#[test]
#[ignore = "talks to api.verustest.net"]
fn a_referral_chain_pays_every_level_without_changing_the_outlay() {
    let Ok(funded) = std::env::var("PECU_FUNDED_HOME") else {
        eprintln!("PECU_FUNDED_HOME is not set — skipping");
        return;
    };

    let assertion = Command::cargo_bin("pecu")
        .expect("built")
        .env("PECU_HOME", &funded)
        .args([
            "id",
            "register",
            "pecudryrun4",
            "--from",
            "faucet",
            "--referral",
            "pecuref9@",
            "--dry-run",
        ])
        .assert()
        .success();
    let stdout = String::from_utf8_lossy(&assertion.get_output().stdout).into_owned();

    assert!(stdout.contains("across 2 levels"), "{stdout}");
    // The assertion that would catch counting the levels allowed rather than
    // the referrers actually in the chain: that would show 20 and 60 here.
    assert!(stdout.contains("40.00000000"), "{stdout}");
    assert!(
        !stdout.contains("60.00000000"),
        "depth was not counted:\n{stdout}"
    );
    // Unchanged by depth.
    assert!(stdout.contains("80.00000000"), "{stdout}");
    // Below the cap, so nothing should claim anyone was dropped.
    assert!(!stdout.contains("receives nothing"), "{stdout}");
}

/// At the cap, a referrer further back is dropped and nothing else says so.
///
/// VRSCTEST allows three levels, so `pecudepth2@` — itself two deep — makes a
/// chain that reaches it. There is no fourth level to pay: a notional depth 4
/// pays the same three and drops the oldest.
#[test]
#[ignore = "talks to api.verustest.net"]
fn a_chain_at_the_cap_says_that_earlier_referrers_get_nothing() {
    let Ok(funded) = std::env::var("PECU_FUNDED_HOME") else {
        eprintln!("PECU_FUNDED_HOME is not set — skipping");
        return;
    };

    let assertion = Command::cargo_bin("pecu")
        .expect("built")
        .env("PECU_HOME", &funded)
        .args([
            "id",
            "register",
            "pecudryrun5",
            "--from",
            "faucet",
            "--referral",
            "pecudepth2@",
            "--dry-run",
        ])
        .assert()
        .success();
    let stdout = String::from_utf8_lossy(&assertion.get_output().stdout).into_owned();

    assert!(stdout.contains("across 3 levels"), "{stdout}");
    assert!(stdout.contains("60.00000000"), "{stdout}");
    assert!(stdout.contains("20.00000000"), "{stdout}");
    assert!(stdout.contains("receives nothing"), "{stdout}");
    // Still 80: depth changes the split, never the outlay.
    assert!(stdout.contains("80.00000000"), "{stdout}");
}

/// Asking for a fourth level gives the same plan a third does.
///
/// The walk truncates to `idreferrallevels` before the transaction is built, so
/// the builder never sees a chain longer than the cap — which is why a
/// registration cannot fail for being "too deep". `pecudepth3@` is three deep,
/// so referring to it asks for four.
#[test]
#[ignore = "talks to api.verustest.net"]
fn asking_for_a_depth_beyond_the_cap_builds_the_same_plan() {
    let Ok(funded) = std::env::var("PECU_FUNDED_HOME") else {
        eprintln!("PECU_FUNDED_HOME is not set — skipping");
        return;
    };

    let plan = |referrer: &str| {
        let assertion = Command::cargo_bin("pecu")
            .expect("built")
            .env("PECU_HOME", &funded)
            .args([
                "id",
                "register",
                "pecudryrun6",
                "--from",
                "faucet",
                "--referral",
                referrer,
                "--dry-run",
                "--json",
            ])
            .assert()
            .success();
        let stdout = String::from_utf8_lossy(&assertion.get_output().stdout).into_owned();
        serde_json::from_str::<serde_json::Value>(&stdout).expect("json")["registration_fee"]
            .as_u64()
            .expect("a fee")
    };

    // Both succeed, and both against the same undiscounted policy fee — the
    // point being that the deeper one builds at all rather than failing.
    assert_eq!(plan("pecudepth2@"), plan("pecudepth3@"));
}

/// Registering burns a hundred coins. `--json` is output, not consent.
/// Resuming has to honour `--from`, or a paid commitment cannot be claimed.
///
/// It did not: `resume` called `choose_key(.., None)` and never saw the flag, so
/// a keystore with more than one key hit "no obvious one to pay with" — after
/// the commitment fee was spent and the name committed. The saved file warns
/// that losing it loses the name and the fee; the tool was what could not claim
/// it. Found by registering `pecuref9@` for real.
#[test]
fn resuming_a_registration_uses_the_key_it_was_told_to() {
    let home = home();
    generate(&home, "one");
    generate(&home, "two");

    let pending = home.path().join("pending");
    std::fs::create_dir_all(&pending).expect("writable");
    std::fs::write(pending.join("alice.json"), SAVED_REGISTRATION).expect("writable");

    // Two keys, and `--from` names one. Reaching the poll rather than the
    // ambiguity is the whole assertion.
    let assertion = pecu(&home)
        .args([
            "id", "register", "alice", "--from", "two", "--node", DEAD_NODE,
        ])
        .assert()
        .failure();
    let stderr = String::from_utf8_lossy(&assertion.get_output().stderr).into_owned();
    assert!(
        !stderr.contains("no obvious one"),
        "resuming ignored --from, so a paid commitment could not be claimed:\n{stderr}"
    );
    assert!(
        stderr.contains("waiting for the commitment") || stderr.contains("checking the commitment"),
        "{stderr}"
    );
}

#[test]
#[ignore = "talks to api.verustest.net"]
fn json_output_is_not_consent_to_register() {
    let Ok(funded) = std::env::var("PECU_FUNDED_HOME") else {
        eprintln!("PECU_FUNDED_HOME is not set — skipping");
        return;
    };

    Command::cargo_bin("pecu")
        .expect("built")
        .env("PECU_HOME", &funded)
        .args([
            "id",
            "register",
            "pecudryrun3",
            "--from",
            "faucet",
            "--json",
        ])
        .assert()
        .failure()
        .stderr(contains("--yes"));
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

/// A commitment carries the expiry height it was signed at, so one that never
/// confirms becomes permanently unbroadcastable — and the saved file is still
/// enough to wedge every later attempt at the name. `--restart` is the only way
/// out, so it must discard the reservation even when what follows fails.
///
/// Offline: the node is dead, so registration cannot get past its first read.
/// The reservation still has to be gone, because deleting it is the point.
#[test]
fn restart_discards_a_saved_reservation_it_cannot_use() {
    let home = home();
    generate(&home, "demo");

    let pending = home.path().join("pending");
    std::fs::create_dir_all(&pending).expect("a pending dir");
    let reservation = pending.join("alice.json");
    std::fs::write(&reservation, "{\"not\": \"resumable\"}").expect("a saved reservation");

    pecu(&home)
        .args(["id", "register", "alice", "--restart", "--node", DEAD_NODE])
        .assert()
        .failure();

    assert!(
        !reservation.exists(),
        "--restart must discard the reservation even though the run that followed failed"
    );
}

/// Without it, the same unusable file is picked up again forever.
#[test]
fn a_saved_reservation_is_resumed_rather_than_replaced_by_default() {
    let home = home();
    generate(&home, "demo");

    let pending = home.path().join("pending");
    std::fs::create_dir_all(&pending).expect("a pending dir");
    let reservation = pending.join("alice.json");
    std::fs::write(&reservation, "{\"not\": \"resumable\"}").expect("a saved reservation");

    pecu(&home)
        .args(["id", "register", "alice", "--node", DEAD_NODE])
        .assert()
        .failure();

    assert!(
        reservation.exists(),
        "a reservation holds the salt and is never deleted without --restart"
    );
}

/// Waiting is the default now, so `--no-wait` has to keep the old behaviour:
/// report the state and stop, leaving the reservation for a later run.
#[test]
fn no_wait_reports_the_state_and_stops() {
    let home = home();
    generate(&home, "demo");
    let pending = home.path().join("pending");
    std::fs::create_dir_all(&pending).expect("a pending dir");
    std::fs::write(pending.join("alice.json"), "{\"not\": \"resumable\"}").expect("a reservation");

    // The dead node means this cannot get far, but --no-wait must not sleep on
    // the way to finding that out.
    let start = std::time::Instant::now();
    pecu(&home)
        .args(["id", "register", "alice", "--no-wait", "--node", DEAD_NODE])
        .assert()
        .failure();
    assert!(
        start.elapsed() < std::time::Duration::from_secs(20),
        "--no-wait must not poll"
    );
}

#[test]
fn the_wait_timeout_is_configurable() {
    let home = home();
    generate(&home, "demo");
    pecu(&home)
        .args(["id", "register", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("--timeout"))
        .stdout(predicate::str::contains("--no-wait"));
}
