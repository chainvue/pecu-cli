//! The air-gap ceremony: `plan send` · `sign` · `broadcast`.
//!
//! Every test here is offline, including the ones that sign — which is the
//! point. The plan is constructed with the SDK directly against a key derived
//! from fixed bytes, so the whole thing is deterministic and needs neither a
//! node nor funds.

use assert_cmd::Command;
use predicates::prelude::*;
use predicates::str::contains;
use tempfile::TempDir;
use verus_sdk::cosign::{InputKind, PartialTransaction};
use verus_sdk::money::{Amount, Expiry, Txid, Utxo};
use verus_sdk::verus_keys::{Address, AddressKind, PrivateKey};
use verus_sdk::verus_wire::TxOut;

const PASSPHRASE: &str = "correct horse battery staple";

/// An address nothing in these tests controls, used as the recipient.
const ELSEWHERE: &str = "RJ7gsKDjUjPS8XZzENqmQMmWJRLuTnw5hp";

/// Unreachable on purpose. Any test that still succeeds with this configured
/// has proved it never opened a socket.
const DEAD_NODE: &str = "https://127.0.0.1:1";

/// A finished, signed transaction pulled off VRSCTEST. Decodes to a real txid
/// with no node, which is what lets these tests reach the guards rather than
/// dying at the local decode the way a plan or a pile of rubbish would.
const FINISHED: &str = include_str!("../fixtures/identity-spend.hex");

/// Two outputs of `u64::MAX`, hand-built: bytes no daemon would emit, which is
/// exactly the kind `broadcast` is handed by a counterparty.
const OUTPUTS_THAT_DO_NOT_TOTAL: &str = include_str!("../fixtures/outputs-that-do-not-total.hex");

/// The txid `FINISHED` decodes to. Fixed, because the bytes are.
const FINISHED_TXID: &str = "2828f297d7611b2488c4e9074960006edb916fe6f8e0c70e5ebe05cab7b284d7";

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

/// The key these plans pay from. Fixed bytes, so the whole fixture is stable.
fn signer(seed: u8) -> PrivateKey {
    PrivateKey::from_bytes(&[seed; 32], true).expect("a valid key")
}

/// Put `key` in the keystore under `label`, the way a user would.
fn import(home: &TempDir, label: &str, key: &PrivateKey) {
    pecu(home)
        .args(["key", "import", "--label", label])
        .write_stdin(format!("{}\n", *key.to_wif()))
        .assert()
        .success();
}

/// A plan spending one P2PKH output belonging to `key`.
///
/// Built through the SDK's own `PartialTransaction::start`, which is what
/// `prepare_unsigned_send` ends with — so this is the same shape a real
/// `pecu plan send` produces, without needing a node to find the coins.
fn plan_paying_from(key: &PrivateKey) -> String {
    let from = key.address();
    let funding = Utxo {
        txid: Txid::from_internal([9u8; 32]),
        vout: 0,
        satoshis: Amount::from_sat(500_000_000),
        script_pubkey: p2pkh(&from),
    };
    let outputs = vec![
        TxOut {
            value: 100_000_000,
            script_pubkey: p2pkh(&ELSEWHERE.parse::<Address>().expect("a valid address")),
        },
        TxOut {
            value: 399_990_000,
            script_pubkey: p2pkh(&from),
        },
    ];
    let partial = PartialTransaction::start(
        &[funding],
        &[InputKind::PubKeyHash],
        outputs,
        Expiry::from_height(1_200_000),
        0,
    )
    .expect("a valid partial");
    hex::encode(partial.to_bytes().expect("serialisable"))
}

/// A plan spending one output from each of two owners, so one signature
/// finishes nothing. The shape a multisig hand-off has: `sign` signs what it
/// can, prints the partial for the next signer, and fails.
fn plan_paying_from_both(first: &PrivateKey, second: &PrivateKey) -> String {
    let funding = [
        Utxo {
            txid: Txid::from_internal([9u8; 32]),
            vout: 0,
            satoshis: Amount::from_sat(300_000_000),
            script_pubkey: p2pkh(&first.address()),
        },
        Utxo {
            txid: Txid::from_internal([10u8; 32]),
            vout: 1,
            satoshis: Amount::from_sat(200_000_000),
            script_pubkey: p2pkh(&second.address()),
        },
    ];
    let outputs = vec![TxOut {
        value: 499_990_000,
        script_pubkey: p2pkh(&ELSEWHERE.parse::<Address>().expect("a valid address")),
    }];
    let partial = PartialTransaction::start(
        &funding,
        &[InputKind::PubKeyHash, InputKind::PubKeyHash],
        outputs,
        Expiry::from_height(1_200_000),
        0,
    )
    .expect("a valid partial");
    hex::encode(partial.to_bytes().expect("serialisable"))
}

fn p2pkh(address: &Address) -> Vec<u8> {
    assert_eq!(address.kind(), AddressKind::PubKeyHash);
    let mut script = vec![0x76, 0xa9, 0x14];
    script.extend_from_slice(&address.hash());
    script.extend_from_slice(&[0x88, 0xac]);
    script
}

#[test]
fn signing_never_opens_a_socket() {
    // The whole reason the ceremony is split. If this can sign with an
    // unreachable node configured, the signing machine genuinely does not need
    // a network — and that is a property, not a hope.
    let home = home();
    let key = signer(7);
    import(&home, "cold", &key);

    let assertion = pecu(&home)
        .args([
            "sign",
            &plan_paying_from(&key),
            "--key",
            "cold",
            "--yes",
            "--node",
            DEAD_NODE,
            "--json",
        ])
        .assert()
        .success();
    let stdout = String::from_utf8_lossy(&assertion.get_output().stdout).into_owned();
    let document: serde_json::Value = serde_json::from_str(&stdout).expect("valid json");

    assert_eq!(document["kind"], "signed");
    assert_eq!(document["complete"], true);
    assert_eq!(document["txid"].as_str().expect("a txid").len(), 64);
    assert!(!document["hex"].as_str().expect("hex").is_empty());
}

#[test]
fn the_wrong_key_signs_nothing_and_says_so() {
    let home = home();
    let plan = plan_paying_from(&signer(7));
    import(&home, "other", &signer(8));

    pecu(&home)
        .args([
            "sign", &plan, "--key", "other", "--yes", "--node", DEAD_NODE,
        ])
        .assert()
        .failure()
        .stderr(contains("signed nothing"));
}

#[test]
fn the_summary_is_shown_before_anything_is_signed() {
    let home = home();
    let key = signer(7);
    import(&home, "cold", &key);

    let assertion = pecu(&home)
        .args([
            "sign",
            &plan_paying_from(&key),
            "--key",
            "cold",
            "--yes",
            "--node",
            DEAD_NODE,
            "--theme",
            "phosphor",
        ])
        .assert()
        .success();
    let stdout = String::from_utf8_lossy(&assertion.get_output().stdout).into_owned();

    assert!(stdout.contains("ABOUT TO SIGN"), "{stdout}");
    assert!(
        stdout.contains(ELSEWHERE),
        "the recipient is not shown:\n{stdout}"
    );
    assert!(
        stdout.contains("5.00000000"),
        "the input value is not shown:\n{stdout}"
    );
    // The check a co-signer cannot make by eye.
    assert!(stdout.contains("SIGHASH_ALL"), "{stdout}");
}

#[test]
fn a_signed_transaction_round_trips_through_a_qr_png() {
    let home = home();
    let key = signer(7);
    import(&home, "cold", &key);
    let out = home.path().join("frames.png");

    let assertion = pecu(&home)
        .args([
            "sign",
            &plan_paying_from(&key),
            "--key",
            "cold",
            "--yes",
            "--node",
            DEAD_NODE,
            "--qr-out",
            out.to_str().expect("utf-8"),
            "--json",
        ])
        .assert()
        .success();
    let stdout = String::from_utf8_lossy(&assertion.get_output().stdout).into_owned();
    let signed: serde_json::Value = serde_json::from_str(&stdout).expect("valid json");

    let frame = home.path().join("frames-1.png");
    assert!(frame.exists(), "no QR frame was written");

    // Reading it back has to reproduce exactly the bytes that went in — a QR
    // channel that quietly drops a nibble is a transaction that fails at the
    // daemon with no explanation.
    let readback = pecu(&home)
        .args([
            "broadcast",
            "--qr-in",
            frame.to_str().expect("utf-8"),
            "--node",
            DEAD_NODE,
            "--yes",
            "--theme",
            "phosphor",
        ])
        .assert()
        .failure();
    let shown = String::from_utf8_lossy(&readback.get_output().stdout).into_owned();
    assert!(
        shown.contains(signed["txid"].as_str().expect("a txid")),
        "the txid read back from the QR differs:\n{shown}"
    );
}

#[test]
fn broadcast_decodes_before_it_sends() {
    let home = home();
    // Not a transaction. It must be refused locally rather than handed to a
    // node to reject.
    pecu(&home)
        .args(["broadcast", "00112233", "--yes", "--node", DEAD_NODE])
        .assert()
        .failure()
        .stderr(contains("not a finished transaction"))
        .stderr(contains("127.0.0.1").not());
}

#[test]
fn a_plan_is_not_a_transaction_and_broadcast_says_so() {
    let home = home();
    pecu(&home)
        .args([
            "broadcast",
            &plan_paying_from(&signer(7)),
            "--yes",
            "--node",
            DEAD_NODE,
        ])
        .assert()
        .failure()
        .stderr(contains("not a finished transaction"));
}

#[test]
fn a_plan_can_be_read_from_a_file() {
    let home = home();
    let key = signer(7);
    import(&home, "cold", &key);
    let path = home.path().join("plan.hex");
    std::fs::write(&path, format!("{}\n", plan_paying_from(&key))).expect("writable");

    pecu(&home)
        .args([
            "sign",
            &format!("@{}", path.display()),
            "--key",
            "cold",
            "--yes",
            "--node",
            DEAD_NODE,
            "--json",
        ])
        .assert()
        .success()
        .stdout(contains("\"complete\": true"));
}

#[test]
fn a_plan_can_be_piped_in() {
    let home = home();
    let key = signer(7);
    import(&home, "cold", &key);

    pecu(&home)
        .args([
            "sign", "--key", "cold", "--yes", "--node", DEAD_NODE, "--json",
        ])
        .write_stdin(plan_paying_from(&key))
        .assert()
        .success()
        .stdout(contains("\"complete\": true"));
}

#[test]
fn rubbish_is_not_mistaken_for_a_plan() {
    let home = home();
    import(&home, "cold", &signer(7));
    pecu(&home)
        .args([
            "sign", "00112233", "--key", "cold", "--yes", "--node", DEAD_NODE,
        ])
        .assert()
        .failure()
        .stderr(contains("not a partial transaction"));
}

#[test]
fn planning_needs_no_key_at_all() {
    let home = home();
    // An empty keystore, an address given outright: this reaches the node and
    // dies there, having never asked for a passphrase.
    pecu(&home)
        .env_remove("PECU_PASSPHRASE")
        .args([
            "plan",
            "send",
            "--address",
            ELSEWHERE,
            "--to",
            ELSEWHERE,
            "--amount",
            "0.1",
            "--node",
            DEAD_NODE,
        ])
        .assert()
        .failure()
        .stderr(contains("planning the payment"))
        .stderr(contains("passphrase").not());
}

// ── the guards ──────────────────────────────────────────────────────────────

#[test]
fn mainnet_will_not_broadcast_without_being_told_to() {
    let home = home();
    // `--yes` is load-bearing here: consent is not a substitute for a profile
    // that is allowed to spend, and it must not buy past the guard.
    pecu(&home)
        .args([
            "--profile",
            "mainnet",
            "broadcast",
            FINISHED.trim(),
            "--yes",
            "--node",
            DEAD_NODE,
        ])
        .assert()
        .failure()
        .stderr(contains("not allowed to spend"))
        .stderr(contains("allow_spend"))
        .stderr(contains("127.0.0.1").not());
}

#[test]
fn mainnet_will_not_plan_a_spend_without_being_told_to() {
    let home = home();
    // Planning chooses the coins. That it never reaches `prepare_unsigned_send`
    // is what the missing "planning the payment" proves — otherwise this would
    // have failed at the node instead of at the profile.
    pecu(&home)
        .args([
            "--profile",
            "mainnet",
            "plan",
            "send",
            "--address",
            ELSEWHERE,
            "--to",
            ELSEWHERE,
            "--amount",
            "0.1",
            "--node",
            DEAD_NODE,
        ])
        .assert()
        .failure()
        .stderr(contains("not allowed to spend"))
        .stderr(contains("planning the payment").not())
        .stderr(contains("127.0.0.1").not());
}

#[test]
fn the_broadcast_panel_never_shows_a_wrapped_total() {
    // The panel before the confirm prompt carries the txid, the output *count*
    // and this total — no per-output values — so nothing on screen would
    // contradict a wrapped figure. It says it cannot be represented instead.
    let home = home();
    let assertion = pecu(&home)
        .args([
            "broadcast",
            OUTPUTS_THAT_DO_NOT_TOTAL.trim(),
            "--dry-run",
            "--node",
            DEAD_NODE,
            "--theme",
            "phosphor",
        ])
        .assert()
        .success();
    let stdout = String::from_utf8_lossy(&assertion.get_output().stdout).into_owned();

    assert!(stdout.contains("WOULD BROADCAST"), "{stdout}");
    assert!(
        stdout.contains("more than can be represented"),
        "the total is not named:\n{stdout}"
    );
    assert!(
        !stdout.contains("184467440737.09551614"),
        "a wrapped total reached the panel:\n{stdout}"
    );
    assertion.stderr(contains("127.0.0.1").not());
}

#[test]
fn a_dry_run_broadcast_stops_at_the_summary() {
    let home = home();
    // Deliberately without `--yes`: a dry run is not a spend and must not ask
    // for consent to do nothing.
    let assertion = pecu(&home)
        .args([
            "broadcast",
            FINISHED.trim(),
            "--dry-run",
            "--node",
            DEAD_NODE,
            "--theme",
            "phosphor",
        ])
        .assert()
        .success();
    let stdout = String::from_utf8_lossy(&assertion.get_output().stdout).into_owned();

    assert!(stdout.contains("WOULD BROADCAST"), "{stdout}");
    assert!(
        stdout.contains(FINISHED_TXID),
        "no txid was shown:\n{stdout}"
    );
    assert!(
        stdout.contains("nothing was sent"),
        "it does not say it stopped:\n{stdout}"
    );
    assertion.stderr(contains("127.0.0.1").not());
}

#[test]
fn a_dry_run_broadcast_says_so_in_json() {
    let home = home();
    let assertion = pecu(&home)
        .args([
            "broadcast",
            FINISHED.trim(),
            "--dry-run",
            "--json",
            "--node",
            DEAD_NODE,
        ])
        .assert()
        .success();
    let stdout = String::from_utf8_lossy(&assertion.get_output().stdout).into_owned();
    let document: serde_json::Value = serde_json::from_str(&stdout).expect("valid json");

    // The shape stays one shape: a dry run is told apart by a field, not by
    // becoming a different `kind`.
    assert_eq!(document["kind"], "broadcast", "{stdout}");
    assert_eq!(document["broadcast"], false, "{stdout}");
    assert_eq!(document["txid"], FINISHED_TXID, "{stdout}");
}

#[test]
fn json_is_not_consent_to_broadcast() {
    let home = home();
    pecu(&home)
        .args(["broadcast", FINISHED.trim(), "--json", "--node", DEAD_NODE])
        .assert()
        .failure()
        .stderr(contains("will not broadcast without --yes"))
        .stderr(contains("pecu::needs_yes"))
        .stderr(contains("127.0.0.1").not());
}

#[test]
fn a_finished_transaction_still_goes_to_the_node_when_told_to() {
    let home = home();
    // The over-blocking guard. Told to, on a profile allowed to spend, this
    // must get all the way to the transport and fail there — the three new
    // gates are not allowed to stand in the ordinary path's way.
    pecu(&home)
        .args(["broadcast", FINISHED.trim(), "--yes", "--node", DEAD_NODE])
        .assert()
        .failure()
        .stderr(contains("broadcasting"))
        .stderr(contains(FINISHED_TXID));
}

/// The third command that prints a document and *then* fails — and the one
/// whose output a script has to feed back in, since the `partial` is what the
/// co-signer is handed. It printed the document and then a second one carrying
/// the error, which `json.loads` and `JSON.parse` refuse outright (#49).
///
/// Exactly one document, with the failure inside it, and the partial intact.
#[test]
fn a_partial_signature_prints_one_document_with_the_failure_inside_it() {
    let home = home();
    let key = signer(7);
    import(&home, "cold", &key);

    let assertion = pecu(&home)
        .args([
            "sign",
            &plan_paying_from_both(&key, &signer(8)),
            "--key",
            "cold",
            "--yes",
            "--node",
            DEAD_NODE,
            "--json",
        ])
        .assert()
        // A partial is not a failure of the node's: nothing was asked of it.
        .code(1);
    let output = assertion.get_output();
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();

    let documents: Vec<serde_json::Value> = serde_json::Deserializer::from_str(&stdout)
        .into_iter::<serde_json::Value>()
        .map(|document| document.unwrap_or_else(|error| panic!("not json: {error}\n{stdout}")))
        .collect();
    assert_eq!(documents.len(), 1, "one document, not two:\n{stdout}");

    let document = &documents[0];
    assert_eq!(document["kind"], "partially_signed");
    assert_eq!(document["signed_inputs"], 1);
    // The thing the next signer needs. Losing it to the failure would make the
    // hand-off impossible in the mode built for hand-offs.
    assert_eq!(
        document["partial"].as_str().expect("the partial hex").len() % 2,
        0
    );
    assert!(!document["partial"].as_str().expect("hex").is_empty());
    assert_eq!(document["error"]["code"], "pecu::incomplete");
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("pecu::incomplete"),
        "the report is still on stderr"
    );
}

/// The exit code #49 added for a broadcast whose outcome nobody knows. The
/// bytes went out and the connection did not come back, so the transaction may
/// or may not be propagating — `3` would invite a blind resend, and `1` would
/// claim it was refused.
#[test]
fn a_broadcast_that_did_not_come_back_exits_four() {
    let home = home();
    let assertion = pecu(&home)
        .args([
            "broadcast",
            FINISHED,
            "--yes",
            "--json",
            "--node",
            DEAD_NODE,
        ])
        .assert()
        .code(4);
    let stdout = String::from_utf8_lossy(&assertion.get_output().stdout).into_owned();
    let document: serde_json::Value =
        serde_json::from_str(&stdout).unwrap_or_else(|error| panic!("not json: {error}\n{stdout}"));
    assert_eq!(document["error"]["code"], "pecu::flow_failed");
    // What the help tells you to check with, and what says why this is not a 3.
    assert!(
        document["error"]["help"]
            .as_str()
            .expect("help")
            .contains(FINISHED_TXID),
        "{document:#}"
    );
}

// ── what the gap cannot carry ───────────────────────────────────────────────
//
// `pecu send` has three modes and the gap carries one of them. The other two
// used to die in clap as `unexpected argument '--currency' found`, which reads
// as a misspelling: the wrong sentence, on the wrong stream, with no code for a
// script and no JSON document under `--json`. These pin the replacement.

/// miette hard-wraps help to the terminal width, and that width differs between
/// an interactive run and a captured one — so a phrase that sits on one line by
/// hand straddles two under `cargo test`. Flatten first, so a multi-word
/// assertion means what it says rather than testing the wrap.
fn flat(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[test]
fn a_planned_token_is_refused_by_name() {
    let home = home();
    let assertion = pecu(&home)
        .args([
            "plan",
            "send",
            "--address",
            ELSEWHERE,
            "--to",
            ELSEWHERE,
            "--amount",
            "1",
            "--currency",
            "TST",
            "--node",
            DEAD_NODE,
        ])
        .assert()
        // A local refusal, not clap's usage error: the flag is understood and
        // the answer is no.
        .code(1);
    let stderr = flat(&String::from_utf8_lossy(&assertion.get_output().stderr));

    assert!(stderr.contains("pecu::plan_has_no_token_path"), "{stderr}");
    // The reason, not just the refusal. Without this the message is a longer
    // way of saying "unexpected argument".
    assert!(stderr.contains("no unsigned form"), "{stderr}");
    assert!(
        stderr.contains("token builders each sign as they build"),
        "{stderr}"
    );
    // Which layer the gap is in. Without this the reader can conclude the
    // partial *format* cannot hold a token and file the follow-up upstream
    // against the wrong thing.
    assert!(
        stderr.contains("The partial format is not the blocker"),
        "{stderr}"
    );
    // The remedy is a command, not a flag: typed out against a keystore holding
    // more than one key, `pecu send --currency` alone stops on the ambiguous
    // key instead (#45), so `--from` is named here rather than discovered.
    assert!(
        stderr.contains("`pecu send --currency NAME@ --amount N --to ADDRESS --from LABEL`"),
        "{stderr}"
    );
    // The honest caveat on it: the command that moves a token signs where the
    // node is, which is the property being given up.
    assert!(stderr.contains("the air gap exists to avoid"), "{stderr}");
    // Names the flag to drop, the way the --contribute and --conversion
    // refusals do, rather than implying it. `-c VRSCTEST@` — the chain's own
    // coin, named — lands here too, and "plan the chain's own coins instead"
    // would read as a contradiction to whoever typed it.
    assert!(stderr.contains("Plan without --currency"), "{stderr}");
    // Refused ahead of the node: nothing was asked of the endpoint.
    assert!(!stderr.contains("127.0.0.1"), "{stderr}");
}

#[test]
fn planning_from_an_identity_is_refused_by_name() {
    let home = home();
    let assertion = pecu(&home)
        .args([
            "plan",
            "send",
            "--address",
            ELSEWHERE,
            "--to",
            ELSEWHERE,
            "--amount",
            "1",
            "--from-identity",
            "bob@",
            "--node",
            DEAD_NODE,
        ])
        .assert()
        .code(1);
    let stderr = flat(&String::from_utf8_lossy(&assertion.get_output().stderr));

    assert!(
        stderr.contains("pecu::plan_has_no_identity_path"),
        "{stderr}"
    );
    // What the identity was, so the sentence is about the thing that was asked
    // for rather than about the flag in the abstract.
    assert!(stderr.contains("bob@"), "{stderr}");
    // The mechanism: this is where the funds are, and why a plan cannot see
    // them. It is the same sentence the shortfall on an identity --address
    // reports, which is why it is written once.
    assert!(stderr.contains("pay-to-identity outputs"), "{stderr}");
    assert!(stderr.contains("only the plain P2PKH kind"), "{stderr}");
    // A CryptoCondition input still has a scriptSig — the fulfillment goes
    // inside it — so the contrast is with what a P2PKH input carries, not with
    // having a scriptSig at all.
    assert!(
        stderr.contains("unlocked by a fulfillment rather than by a signature and public key"),
        "{stderr}"
    );
    assert!(
        stderr.contains("`pecu send --from-identity bob@ --amount N --to ADDRESS --from LABEL`"),
        "{stderr}"
    );
    // A command the reader can paste, which is the whole point of naming it.
    assert!(
        stderr.contains("`pecu wallet balance --address bob@`"),
        "{stderr}"
    );
    assert!(stderr.contains("Plan without --from-identity"), "{stderr}");
    assert!(!stderr.contains("127.0.0.1"), "{stderr}");
}

/// `--from-identity ""` is the same refusal, and it must not quote an empty
/// value back or print a remedy with its argument missing.
#[test]
fn an_empty_identity_is_refused_without_quoting_nothing() {
    let home = home();
    let assertion = pecu(&home)
        .args([
            "plan",
            "send",
            "--address",
            ELSEWHERE,
            "--to",
            ELSEWHERE,
            "--amount",
            "1",
            "--from-identity",
            "   ",
            "--node",
            DEAD_NODE,
        ])
        .assert()
        .code(1);
    let stderr = flat(&String::from_utf8_lossy(&assertion.get_output().stderr));

    assert!(
        stderr.contains("pecu::plan_has_no_identity_path"),
        "{stderr}"
    );
    // No empty backticks anywhere, in the title or in a remedy.
    assert!(!stderr.contains("``"), "{stderr}");
    assert!(stderr.contains("spends what a VerusID holds"), "{stderr}");
    // The balance remedy names an identity or it is not printed at all: `pecu
    // wallet balance --address` with nothing after it does not run.
    assert!(!stderr.contains("wallet balance"), "{stderr}");
    assert!(
        stderr.contains("`pecu send --from-identity NAME@ --amount N --to ADDRESS --from LABEL`"),
        "{stderr}"
    );
}

/// Under `--json` a refusal is a document a script can branch on, not clap text
/// on stderr and an empty stdout (#49).
#[test]
fn a_refused_plan_is_one_json_document_with_a_code() {
    let home = home();
    let assertion = pecu(&home)
        .args([
            "plan",
            "send",
            "--address",
            ELSEWHERE,
            "--to",
            ELSEWHERE,
            "--amount",
            "1",
            "--currency",
            "TST",
            "--node",
            DEAD_NODE,
            "--json",
        ])
        .assert()
        .code(1);
    let stdout = String::from_utf8_lossy(&assertion.get_output().stdout).into_owned();

    let documents: Vec<serde_json::Value> = serde_json::Deserializer::from_str(&stdout)
        .into_iter::<serde_json::Value>()
        .map(|document| document.unwrap_or_else(|error| panic!("not json: {error}\n{stdout}")))
        .collect();
    assert_eq!(documents.len(), 1, "one document, not two:\n{stdout}");
    assert_eq!(
        documents[0]["error"]["code"],
        "pecu::plan_has_no_token_path"
    );
    assert!(
        documents[0]["error"]["help"]
            .as_str()
            .expect("help")
            .contains("`pecu send --currency NAME@ --amount N --to ADDRESS --from LABEL`"),
        "{stdout}"
    );
}

#[test]
fn a_refused_identity_plan_is_one_json_document_with_a_code() {
    let home = home();
    let assertion = pecu(&home)
        .args([
            "plan",
            "send",
            "--address",
            ELSEWHERE,
            "--to",
            ELSEWHERE,
            "--amount",
            "1",
            "--from-identity",
            "bob@",
            "--node",
            DEAD_NODE,
            "--json",
        ])
        .assert()
        .code(1);
    let stdout = String::from_utf8_lossy(&assertion.get_output().stdout).into_owned();

    let documents: Vec<serde_json::Value> = serde_json::Deserializer::from_str(&stdout)
        .into_iter::<serde_json::Value>()
        .map(|document| document.unwrap_or_else(|error| panic!("not json: {error}\n{stdout}")))
        .collect();
    assert_eq!(documents.len(), 1, "one document, not two:\n{stdout}");
    assert_eq!(
        documents[0]["error"]["code"],
        "pecu::plan_has_no_identity_path"
    );
    assert!(
        documents[0]["error"]["message"]
            .as_str()
            .expect("message")
            .contains("bob@"),
        "{stdout}"
    );
}

/// The refusal a reader should never have to trigger. `--help` says which two
/// flags the gap does not carry before either is typed, the way the launch
/// guards do.
#[test]
fn plan_send_help_says_which_flags_are_refused() {
    let home = home();
    let assertion = pecu(&home)
        .args(["plan", "send", "--help"])
        .assert()
        .success();
    let stdout = flat(&String::from_utf8_lossy(&assertion.get_output().stdout));

    assert!(stdout.contains("--currency"), "{stdout}");
    assert!(stdout.contains("--from-identity"), "{stdout}");
    // The word alone would pass on a flag documented as working, so pin the
    // clause that carries the reason: softening either back into a description
    // of what the flag would do is the defect this issue was about.
    assert!(
        stdout.contains("Refused: a token rides in an output's script"),
        "{stdout}"
    );
    assert!(
        stdout.contains("Refused: what a VerusID holds sits in pay-to-identity outputs"),
        "{stdout}"
    );
}
