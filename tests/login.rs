//! `pecu id login challenge|sign|verify`.
//!
//! The replay guard is what most of this asserts. The cryptography is the SDK's
//! and is tested there; what this program adds is the memory of which
//! challenges it issued, and that memory is the only thing standing between a
//! valid signature and a valid signature presented twice.

use assert_cmd::Command;
use predicates::prelude::*;
use predicates::str::contains;
use tempfile::TempDir;

const PASSPHRASE: &str = "correct horse battery staple";
const DEAD_NODE: &str = "https://127.0.0.1:1";
const AUDIENCE: &str = "https://pecu.example";

/// Registered by this project on 2026-08-05, controlled by the funded key.
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

/// Issue a challenge and hand back the hex.
fn issue(home: &TempDir) -> String {
    let assertion = pecu(home)
        .args(["id", "login", "challenge", "--audience", AUDIENCE, "--json"])
        .assert()
        .success();
    let stdout = String::from_utf8_lossy(&assertion.get_output().stdout).into_owned();
    serde_json::from_str::<serde_json::Value>(&stdout).expect("json")["challenge"]
        .as_str()
        .expect("a challenge")
        .to_string()
}

#[test]
fn a_challenge_is_recorded_so_it_can_be_spent_exactly_once() {
    let home = home();
    let challenge = issue(&home);

    // 32 bytes of entropy, hex. A short or guessable challenge is a credential
    // anyone can ask to have signed.
    assert_eq!(challenge.len(), 64, "{challenge}");
    assert!(challenge.chars().all(|c| c.is_ascii_hexdigit()));

    let record = home.path().join("logins").join(format!("{challenge}.json"));
    assert!(record.is_file(), "no record at {}", record.display());
}

#[test]
fn two_challenges_are_never_the_same() {
    let home = home();
    assert_ne!(issue(&home), issue(&home));
}

#[test]
fn the_message_binds_the_audience_and_the_challenge_with_their_lengths() {
    let home = home();
    let assertion = pecu(&home)
        .args(["id", "login", "challenge", "--audience", AUDIENCE, "--json"])
        .assert()
        .success();
    let stdout = String::from_utf8_lossy(&assertion.get_output().stdout).into_owned();
    let document: serde_json::Value = serde_json::from_str(&stdout).expect("json");
    let message = document["message"].as_str().expect("a message");

    // Length-prefixed, so an audience ending in digits cannot be confused with
    // a challenge beginning with them. The signer sees this text before
    // approving it, so it is worth it being legible.
    assert!(message.starts_with("verusid-login\n"), "{message}");
    assert!(
        message.contains(&format!("{}:{AUDIENCE}", AUDIENCE.len())),
        "{message}"
    );
}

#[test]
fn a_challenge_this_machine_never_issued_is_refused_before_the_node_is_called() {
    let home = home();
    // The node is dead, so reaching it would fail differently. This has to fail
    // on the replay check, which needs no network at all.
    pecu(&home)
        .args([
            "id",
            "login",
            "verify",
            OURS,
            "--audience",
            AUDIENCE,
            "--challenge",
            "00000000000000000000000000000000",
            "--signature",
            "Ae31EQABQR9QqnkuV1payloroaDLgV1C",
            "--node",
            DEAD_NODE,
        ])
        .assert()
        .failure()
        .stderr(contains("did not issue that challenge"))
        .stderr(contains("--stateless"));
}

#[test]
fn a_challenge_issued_for_one_audience_is_not_accepted_for_another() {
    let home = home();
    let challenge = issue(&home);
    // The audience is inside the signed bytes. Checking a signature against a
    // different one would accept a login meant for somebody else's site.
    pecu(&home)
        .args([
            "id",
            "login",
            "verify",
            OURS,
            "--audience",
            "https://evil.example",
            "--challenge",
            &challenge,
            "--signature",
            "Ae31EQABQR9QqnkuV1payloroaDLgV1C",
            "--node",
            DEAD_NODE,
        ])
        .assert()
        .failure()
        .stderr(contains("was issued for"));
}

#[test]
fn a_challenge_cannot_walk_out_of_the_login_store() {
    let home = home();
    // The challenge names a file and arrives on the command line.
    for hostile in ["../../../etc/passwd", "..", "a/b"] {
        pecu(&home)
            .args([
                "id",
                "login",
                "verify",
                OURS,
                "--audience",
                AUDIENCE,
                "--challenge",
                hostile,
                "--signature",
                "Ae31EQABQR9QqnkuV1payloroaDLgV1C",
                "--node",
                DEAD_NODE,
            ])
            .assert()
            .failure()
            .stderr(contains("a filename can hold"));
    }
}

#[test]
fn a_signature_that_is_not_a_signature_says_so_rather_than_asking_a_node() {
    let home = home();
    let challenge = issue(&home);
    pecu(&home)
        .args([
            "id",
            "login",
            "verify",
            OURS,
            "--audience",
            AUDIENCE,
            "--challenge",
            &challenge,
            "--signature",
            "not base64 at all!!",
            "--node",
            DEAD_NODE,
        ])
        .assert()
        .failure()
        .stderr(contains("not a signature this can read"));
}

#[test]
fn signing_needs_a_key() {
    let home = home();
    pecu(&home)
        .args([
            "id",
            "login",
            "sign",
            OURS,
            "--audience",
            AUDIENCE,
            "--challenge",
            "deadbeef",
            "--node",
            DEAD_NODE,
        ])
        .assert()
        .failure()
        .stderr(contains("no key to sign with"));
}

/// The whole demo, against the chain: issue, sign, verify, and fail to replay.
#[test]
#[ignore = "talks to api.verustest.net; needs the key for pecucli7@"]
fn a_login_round_trips_and_the_second_attempt_is_refused() {
    let Ok(funded) = std::env::var("PECU_FUNDED_HOME") else {
        eprintln!("PECU_FUNDED_HOME is not set — skipping");
        return;
    };

    let run = |args: &[&str]| {
        let assertion = Command::cargo_bin("pecu")
            .expect("built")
            .env("PECU_HOME", &funded)
            .args(args)
            .assert()
            .success();
        let stdout = String::from_utf8_lossy(&assertion.get_output().stdout).into_owned();
        serde_json::from_str::<serde_json::Value>(&stdout).expect("valid json")
    };

    let issued = run(&["id", "login", "challenge", "--audience", AUDIENCE, "--json"]);
    let challenge = issued["challenge"]
        .as_str()
        .expect("a challenge")
        .to_owned();

    let signed = run(&[
        "id",
        "login",
        "sign",
        OURS,
        "--audience",
        AUDIENCE,
        "--challenge",
        &challenge,
        "--json",
    ]);
    let signature = signed["signature"]
        .as_str()
        .expect("a signature")
        .to_owned();
    assert!(signed["signed_at"].as_u64().unwrap_or(0) > 1_000_000);

    let verify = [
        "id",
        "login",
        "verify",
        OURS,
        "--audience",
        AUDIENCE,
        "--challenge",
        &challenge,
        "--signature",
        &signature,
        "--json",
    ];
    let verified = run(&verify);
    assert_eq!(verified["verified"], true, "{verified:#}");
    assert_eq!(verified["name"], "pecucli7.VRSCTEST@");
    assert_eq!(verified["replay_checked"], true);
    assert_eq!(
        verified["signed_at"], signed["signed_at"],
        "the height in the signature is what the verifier read back"
    );

    // The same bytes again. They are still a perfectly valid signature — the
    // only thing that rejects them is that this challenge has been spent.
    Command::cargo_bin("pecu")
        .expect("built")
        .env("PECU_HOME", &funded)
        .args(verify)
        .assert()
        .failure()
        .stderr(contains("did not issue that challenge"));
}

#[test]
#[ignore = "talks to api.verustest.net; needs the key for pecucli7@"]
fn a_signature_made_for_one_site_does_not_verify_at_another() {
    let Ok(funded) = std::env::var("PECU_FUNDED_HOME") else {
        eprintln!("PECU_FUNDED_HOME is not set — skipping");
        return;
    };

    let sign = Command::cargo_bin("pecu")
        .expect("built")
        .env("PECU_HOME", &funded)
        .args([
            "id",
            "login",
            "sign",
            OURS,
            "--audience",
            AUDIENCE,
            "--challenge",
            "00112233445566778899aabbccddeeff",
            "--json",
        ])
        .assert()
        .success();
    let stdout = String::from_utf8_lossy(&sign.get_output().stdout).into_owned();
    let signature = serde_json::from_str::<serde_json::Value>(&stdout).expect("json")["signature"]
        .as_str()
        .expect("a signature")
        .to_string();

    // Cryptographically valid, and for a different audience. `--stateless`
    // takes the replay store out of it so this tests the signature alone.
    Command::cargo_bin("pecu")
        .expect("built")
        .env("PECU_HOME", &funded)
        .args([
            "id",
            "login",
            "verify",
            OURS,
            "--audience",
            "https://somewhere-else.example",
            "--challenge",
            "00112233445566778899aabbccddeeff",
            "--signature",
            &signature,
            "--stateless",
        ])
        .assert()
        .failure()
        .stderr(contains("does not satisfy").or(contains("authority")));
}

#[test]
#[ignore = "talks to api.verustest.net; needs the key for pecucli7@"]
fn stateless_verification_says_it_is_not_checking_replay() {
    let Ok(funded) = std::env::var("PECU_FUNDED_HOME") else {
        eprintln!("PECU_FUNDED_HOME is not set — skipping");
        return;
    };
    let challenge = "aabbccddeeff00112233445566778899";

    let sign = Command::cargo_bin("pecu")
        .expect("built")
        .env("PECU_HOME", &funded)
        .args([
            "id",
            "login",
            "sign",
            OURS,
            "--audience",
            AUDIENCE,
            "--challenge",
            challenge,
            "--json",
        ])
        .assert()
        .success();
    let stdout = String::from_utf8_lossy(&sign.get_output().stdout).into_owned();
    let signature = serde_json::from_str::<serde_json::Value>(&stdout).expect("json")["signature"]
        .as_str()
        .expect("a signature")
        .to_string();

    let args = [
        "id",
        "login",
        "verify",
        OURS,
        "--audience",
        AUDIENCE,
        "--challenge",
        challenge,
        "--signature",
        &signature,
        "--stateless",
    ];

    // Never issued here, and accepted anyway — that is what --stateless means.
    // Twice, because the point is that nothing is being spent.
    for _ in 0..2 {
        Command::cargo_bin("pecu")
            .expect("built")
            .env("PECU_HOME", &funded)
            .args(args)
            .assert()
            .success()
            .stdout(contains("VERIFIED"))
            // It must not look like a checked login.
            .stdout(contains("--stateless"));
    }
}
