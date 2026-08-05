//! `pecu tx explain`, against real bytes.
//!
//! Entirely offline. Every fixture is a genuine VRSCTEST artefact — two mined
//! transactions pulled off the chain, two currency-launch transactions and two
//! output scripts from the SDK's own daemon fixtures — so what these assert is
//! that the decoder says the right thing about bytes the daemon really produced.

use assert_cmd::Command;
use predicates::str::contains;

fn fixture(name: &str) -> String {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/fixtures/");
    std::fs::read_to_string(format!("{path}{name}"))
        .unwrap_or_else(|error| panic!("fixtures/{name}: {error}"))
        .trim()
        .to_string()
}

fn pecu() -> Command {
    let mut command = Command::cargo_bin("pecu").expect("the pecu binary should be built");
    command.env("PECU_HOME", tempfile::tempdir().expect("a temp dir").keep());
    command
}

fn explain(name: &str) -> String {
    let assertion = pecu()
        .args(["tx", "explain", &fixture(name), "--theme", "phosphor"])
        .env("PECU_WIDTH", "84")
        .assert()
        .success();
    String::from_utf8_lossy(&assertion.get_output().stdout).into_owned()
}

/// Strip the frame and rejoin, so an assertion about a sentence does not depend
/// on where the renderer chose to wrap it.
fn unwrapped(rendered: &str) -> String {
    rendered
        .lines()
        .map(|line| line.trim_matches(['│', ' ']))
        .collect::<Vec<_>>()
        .join(" ")
}

fn explain_json(name: &str) -> serde_json::Value {
    let assertion = pecu()
        .args(["tx", "explain", &fixture(name), "--json"])
        .assert()
        .success();
    let stdout = String::from_utf8_lossy(&assertion.get_output().stdout).into_owned();
    serde_json::from_str(&stdout).unwrap_or_else(|error| panic!("not json: {error}\n{stdout}"))
}

#[test]
fn a_coinbase_reads_as_a_bare_public_key_payment() {
    insta::assert_snapshot!("coinbase", explain("coinbase.hex"));
}

#[test]
fn a_currency_launch_shows_all_seven_outputs() {
    insta::assert_snapshot!(
        "currency_launch",
        explain("currency-launch-fractional-one-reserve.hex")
    );
}

#[test]
fn an_identity_payment_is_recognised_without_an_eval_code() {
    let rendered = explain("identity-spend.hex");
    assert!(
        rendered.contains("held for a VerusID, not a key"),
        "{rendered}"
    );
    assert!(
        rendered.contains("iP6FybPsi3s6eLi3Sh8TNH3Pz41uoSYezv"),
        "{rendered}"
    );
}

#[test]
fn an_undecodable_output_is_reported_without_failing_the_rest() {
    // The same transaction has one output this SDK reads and one it does not.
    // Refusing the whole thing over the second would throw away the first.
    let rendered = explain("identity-spend.hex");
    assert!(rendered.contains("undecodable"), "{rendered}");
    assert!(rendered.contains("6.00010000"), "{rendered}");
}

#[test]
fn an_output_that_may_hold_currency_is_shouted_about() {
    // Searched on the unwrapped text: the warning is prose and the renderer
    // wraps it, so where the line breaks depends on the width.
    let rendered = unwrapped(&explain("currency-launch-fractional-one-reserve.hex"));
    assert!(
        rendered.contains("IT MAY HOLD CURRENCY; do not treat this output as empty"),
        "the may_carry_currency warning is missing:\n{rendered}"
    );
    // And one that provably cannot is not shouted about.
    assert!(rendered.contains("it cannot hold currency"), "{rendered}");
}

#[test]
fn a_bare_output_script_is_read_as_one() {
    let rendered = explain("script-identity-payment.hex");
    assert!(
        rendered.contains("read as a single output script"),
        "{rendered}"
    );
    assert!(
        rendered.contains("iJhCezBExJHvtyH3fGhNnt2NhU4Ztkf2yq"),
        "{rendered}"
    );
}

#[test]
fn a_reserve_transfer_names_its_currencies_as_addresses() {
    let rendered = explain("script-reserve-transfer.hex");
    assert!(rendered.contains("value in flight"), "{rendered}");
    // Not raw hex: every explorer and every other line of this program says i…
    assert!(rendered.contains("iJhCezBEx"), "{rendered}");
    assert!(
        !rendered.contains("a6ef9ea235635e328124ff3429db9f9e91b64e2d"),
        "a currency id leaked as raw hex:\n{rendered}"
    );
}

#[test]
fn a_zero_expiry_is_named_rather_than_printed_as_zero() {
    // A coinbase never expires, and "0" would read as a date rather than as
    // "this stays minable forever".
    assert!(explain("coinbase.hex").contains("stays minable forever"));
}

#[test]
fn every_frame_stays_rectangular_across_all_fixtures() {
    for name in [
        "coinbase.hex",
        "currency-launch-fractional-one-reserve.hex",
        "currency-launch-token-centralized.hex",
        "identity-spend.hex",
        "script-identity-payment.hex",
        "script-reserve-transfer.hex",
    ] {
        let rendered = explain(name);
        let widths: Vec<usize> = rendered
            .lines()
            .filter(|line| line.starts_with(['┌', '│', '├', '└']))
            .map(|line| line.chars().count())
            .collect();
        assert!(!widths.is_empty(), "{name} rendered no frame");
        assert!(
            widths.windows(2).all(|pair| pair[0] == pair[1]),
            "{name} has a ragged frame, widths {widths:?}:\n{rendered}"
        );
    }
}

#[test]
fn json_carries_the_discriminant_and_the_fields() {
    let document = explain_json("currency-launch-fractional-one-reserve.hex");
    assert_eq!(document["kind"], "transaction");
    assert_eq!(document["outputs"].as_array().expect("outputs").len(), 7);

    let kinds: Vec<&str> = document["outputs"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|output| output["kind"].as_str())
        .collect();
    assert!(kinds.contains(&"identity_primary"), "{kinds:?}");
    assert!(kinds.contains(&"reserve_deposit"), "{kinds:?}");
    assert!(kinds.contains(&"identity_payment"), "{kinds:?}");
    assert!(kinds.contains(&"unsupported_cryptocondition"), "{kinds:?}");

    let risky = document["outputs"]
        .as_array()
        .unwrap()
        .iter()
        .any(|output| output["may_carry_currency"] == true);
    assert!(
        risky,
        "the may_carry_currency flag is missing from the json"
    );
}

#[test]
fn json_for_a_bare_script_says_it_is_one() {
    let document = explain_json("script-reserve-transfer.hex");
    assert_eq!(document["kind"], "output_script");
    assert_eq!(document["output"]["kind"], "reserve_transfer");
}

#[test]
fn hex_can_be_piped_in() {
    pecu()
        .args(["tx", "explain", "-"])
        .write_stdin(fixture("coinbase.hex"))
        .assert()
        .success()
        .stdout(contains("stays minable forever"));
}

#[test]
fn nothing_to_decode_says_so() {
    pecu()
        .args(["tx", "explain", "-"])
        .write_stdin("")
        .assert()
        .failure()
        .stderr(contains("nothing to decode"));
}

#[test]
fn rubbish_is_refused_as_rubbish() {
    pecu()
        .args(["tx", "explain", "zzzz"])
        .assert()
        .failure()
        .stderr(contains("not hex"));
}

#[test]
fn hex_that_is_neither_a_transaction_nor_a_script_is_refused() {
    pecu()
        .args(["tx", "explain", "00112233"])
        .assert()
        .failure()
        .stderr(contains("not a transaction"));
}

#[test]
#[ignore = "talks to api.verustest.net"]
fn a_txid_is_fetched_and_decoded_locally() {
    // The same transaction as the identity-spend fixture, by id rather than by
    // bytes. Decoding it is the SDK's job either way; the node only supplies hex.
    let assertion = pecu()
        .args([
            "tx",
            "explain",
            "2828f297d7611b2488c4e9074960006edb916fe6f8e0c70e5ebe05cab7b284d7",
            "--json",
        ])
        .assert()
        .success();
    let stdout = String::from_utf8_lossy(&assertion.get_output().stdout).into_owned();
    let document: serde_json::Value = serde_json::from_str(&stdout).expect("valid json");
    assert_eq!(
        document["txid"],
        "2828f297d7611b2488c4e9074960006edb916fe6f8e0c70e5ebe05cab7b284d7"
    );
    assert_eq!(document["outputs"][0]["kind"], "identity_payment");
}

#[test]
#[ignore = "talks to api.verustest.net"]
fn an_unknown_txid_is_an_answer_not_a_crash() {
    pecu()
        .args([
            "tx",
            "explain",
            "0000000000000000000000000000000000000000000000000000000000000001",
        ])
        .assert()
        .failure()
        .stderr(contains("knows no transaction"));
}
