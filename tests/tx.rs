//! `pecu tx explain`, against real bytes.
//!
//! Offline unless a test says otherwise. Two tests point the binary at a
//! scripted loopback node, because naming a currency needs a node and the txid
//! path is the only one that has one; two more point it at an unreachable
//! address on purpose, because "hex needs no network" is a property and not a
//! hope. Every fixture but one is a genuine VRSCTEST artefact — two
//! mined transactions pulled off the chain, two currency-launch transactions and
//! two output scripts from the SDK's own daemon fixtures — so what these assert
//! is that the decoder says the right thing about bytes the daemon really
//! produced. The exception is `outputs-that-do-not-total.hex`, hand-built
//! because no daemon would ever emit it: a decoder has to be right about bytes
//! nobody mined either, since it is handed them by a counterparty.

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
    command
        .env("PECU_HOME", tempfile::tempdir().expect("a temp dir").keep())
        .env_remove("PECU_THEME");
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
fn a_total_larger_than_a_u64_is_said_in_words_not_wrapped() {
    // Two outputs of `u64::MAX`. Nothing in the wire format rules that out, and
    // a bare `sum::<u64>()` met it with a panic in a debug build and a total
    // *smaller than one of the outputs* in a release one.
    let rendered = explain("outputs-that-do-not-total.hex");
    let flat = unwrapped(&rendered);
    assert!(
        flat.contains("2 outputs — more than can be represented in native satoshis"),
        "{rendered}"
    );
    // Each output still says what it is worth; only the total is missing.
    assert!(rendered.contains("184467440737.09551615"), "{rendered}");
    assert!(
        !rendered.contains("184467440737.09551614"),
        "a wrapped total reached the panel:\n{rendered}"
    );
}

#[test]
fn json_gives_no_total_rather_than_a_wrapped_one() {
    let document = explain_json("outputs-that-do-not-total.hex");
    assert_eq!(document["kind"], "transaction");
    let outputs = document["outputs"].as_array().expect("an outputs array");
    assert_eq!(outputs.len(), 2);
    for output in outputs {
        assert_eq!(output["satoshis"], serde_json::json!(u64::MAX));
    }
    assert!(
        document["total_satoshis"].is_null(),
        "a wrapped total reached the document: {document}"
    );
}

#[test]
fn every_frame_stays_rectangular_across_all_fixtures() {
    for name in [
        "coinbase.hex",
        "currency-launch-fractional-one-reserve.hex",
        "currency-launch-token-centralized.hex",
        "identity-spend.hex",
        "outputs-that-do-not-total.hex",
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

/// Unreachable on purpose, the way `tests/airgap.rs` uses one. Any run that
/// still succeeds with this configured has proved it never opened a socket.
const DEAD_NODE: &str = "https://127.0.0.1:1";

#[test]
fn hex_still_decodes_with_no_reachable_node_at_all() {
    // The property, not the hope. `pecu tx explain <hex>` is what still answers
    // after a broadcast the node was unsure about — it is the recovery step the
    // `-25` advice tells a caller to run — so it has to work when the node is
    // the thing that just failed. Naming a currency needs a node, and this is
    // what keeps that from quietly becoming true of every path.
    pecu()
        .args([
            "tx",
            "explain",
            &fixture("currency-launch-fractional-one-reserve.hex"),
            "--node",
            DEAD_NODE,
        ])
        .assert()
        .success()
        .stdout(contains("iJhCezBExJHvtyH3fGhNnt2NhU4Ztkf2yq"));
}

#[test]
fn piped_hex_still_decodes_with_no_reachable_node_at_all() {
    // Same property through stdin, because stdin is not its own path: the input
    // is classified by what it *is*, and `pecu tx explain -` fed a txid does
    // connect. What is offline is hex, wherever it arrived from.
    pecu()
        .args(["tx", "explain", "-", "--node", DEAD_NODE])
        .write_stdin(fixture("coinbase.hex"))
        .assert()
        .success()
        .stdout(contains("stays minable forever"));
}

#[test]
fn a_currency_id_survives_the_narrowest_frame_whole() {
    // The bug as filed: a fixed nine-and-four elision cut every token id to
    // `iJhCezBEx…f2yq` at any width, and `pecu currency show` on that cannot
    // work — base58check over a truncated address does not decode. Asked at a
    // width narrower than the narrowest panel this tool will draw.
    let assertion = pecu()
        .args([
            "tx",
            "explain",
            &fixture("currency-launch-fractional-one-reserve.hex"),
            "--theme",
            "phosphor",
        ])
        .env("PECU_WIDTH", "1")
        .assert()
        .success();
    let rendered = String::from_utf8_lossy(&assertion.get_output().stdout).into_owned();
    assert!(
        rendered.contains("iJhCezBExJHvtyH3fGhNnt2NhU4Ztkf2yq"),
        "{rendered}"
    );
    let widths: Vec<usize> = rendered
        .lines()
        .filter(|line| line.starts_with(['┌', '│', '├', '└']))
        .map(|line| line.chars().count())
        .collect();
    assert!(
        widths.windows(2).all(|pair| pair[0] == pair[1]),
        "a whole id pushed the frame open, widths {widths:?}:\n{rendered}"
    );
}

#[test]
fn json_says_a_name_was_never_looked_up_rather_than_that_there_is_none() {
    // Three states, not two. `"name": null` would say this currency has no
    // name, which is a confident answer to a question an offline run never
    // asked — and the id beside it is unchanged and still whole, so nothing a
    // consumer already reads has moved.
    let document = explain_json("currency-launch-fractional-one-reserve.hex");
    let deposit = document["outputs"]
        .as_array()
        .expect("outputs")
        .iter()
        .find(|output| output["kind"] == "reserve_deposit")
        .expect("a reserve deposit");
    assert_eq!(
        deposit["controlling_currency_name"],
        serde_json::json!({ "known": false, "error": "the name was not looked up" }),
        "{deposit}"
    );
    let token = &deposit["tokens"][0];
    assert_eq!(token["currency"], "iJhCezBExJHvtyH3fGhNnt2NhU4Ztkf2yq");
    assert_eq!(token["name"]["known"], false, "{token}");
}

/// A loopback node that answers each request with whichever scripted reply
/// names something the request asked about.
///
/// The one way to exercise the txid path without a public daemon, and the txid
/// path is the only one that names anything. Plaintext is accepted because
/// loopback is the one place `pecu` does not refuse it. One reply per
/// connection and `connection: close`: the point is to answer several
/// *different* requests, and a handler that served one and hung up would strand
/// the next lookup on a pooled dead socket. The accept loop is not shut down —
/// it owns nothing but a port and is reaped at process exit.
fn scripted_node(replies: Vec<(String, String)>) -> String {
    use std::io::{Read, Write};

    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("a loopback port");
    let url = format!("http://{}", listener.local_addr().expect("a bound address"));
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { return };
            let mut request = Vec::new();
            let mut chunk = [0u8; 4096];
            // Drained until the body has arrived, not after one read: headers
            // and body can land in separate segments, and answering half a
            // request reads as a transport failure rather than a reply.
            while !request.windows(4).any(|window| window == b"\r\n\r\n")
                || !request.ends_with(b"}")
            {
                match stream.read(&mut chunk) {
                    Ok(0) | Err(_) => break,
                    Ok(read) => request.extend_from_slice(&chunk[..read]),
                }
            }
            let request = String::from_utf8_lossy(&request).into_owned();
            let body = replies
                .iter()
                .find(|(asked, _)| request.contains(asked.as_str()))
                .map(|(_, reply)| reply.clone())
                .unwrap_or_else(|| {
                    r#"{"error":{"code":-1,"message":"nothing scripted"},"id":1}"#.to_string()
                });
            let reply = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\nconnection: close\r\n\
                 content-length: {}\r\n\r\n{body}",
                body.len()
            );
            let _ = stream.write_all(reply.as_bytes());
            let _ = stream.flush();
        }
    });
    url
}

/// The txid of the fractional launch, and the `getrawtransaction` reply that
/// hands its real bytes back.
const LAUNCH_TXID: &str = "df69640e4cfafe7cbe9cabd3c790ed3c556f7ee340e5f10ce73dd1b590f0556d";

fn raw_transaction_reply() -> String {
    format!(
        r#"{{"result":{{"txid":"{LAUNCH_TXID}","hex":"{}"}},"id":1}}"#,
        fixture("currency-launch-fractional-one-reserve.hex")
    )
}

/// A `getcurrency` reply, trimmed to the fields the SDK reads plus
/// `idimportfees`, which the daemon really does print as `1e-8`.
fn definition(id: &str, name: &str) -> String {
    format!(
        r#"{{"result":{{"currencyid":"{id}","name":"{name}","fullyqualifiedname":"{name}",
           "parent":"{id}","systemid":"{id}","startblock":0,"endblock":0,
           "options":33,"proofprotocol":1,"idimportfees":1e-8}},"id":1}}"#
    )
}

#[test]
fn a_txid_run_names_the_currencies_it_prints() {
    // The issue as filed: this command never named a currency, not even the
    // chain's own. It has a node in hand on this path and had not been asking.
    let node = scripted_node(vec![
        (LAUNCH_TXID.into(), raw_transaction_reply()),
        (
            "iJhCezBExJHvtyH3fGhNnt2NhU4Ztkf2yq".into(),
            definition("iJhCezBExJHvtyH3fGhNnt2NhU4Ztkf2yq", "VRSCTEST"),
        ),
        (
            "i9G2QgG74f7tErEyF3cWp2x1exBGbFa19t".into(),
            definition("i9G2QgG74f7tErEyF3cWp2x1exBGbFa19t", "verusrpc-test"),
        ),
    ]);

    let assertion = pecu()
        .args([
            "tx",
            "explain",
            LAUNCH_TXID,
            "--node",
            &node,
            "--theme",
            "phosphor",
        ])
        .env("PECU_WIDTH", "84")
        .assert()
        .success();
    let rendered = String::from_utf8_lossy(&assertion.get_output().stdout).into_owned();

    assert!(rendered.contains("VRSCTEST@"), "{rendered}");
    // And the id is still there, whole. A node can hand back a lookalike name;
    // the id is the half of the pair that settles which currency this is.
    assert!(
        rendered.contains("iJhCezBExJHvtyH3fGhNnt2NhU4Ztkf2yq"),
        "{rendered}"
    );
}

#[test]
fn a_name_lookup_that_fails_costs_a_name_and_not_the_explain() {
    // The node answers for the transaction and then refuses every `getcurrency`
    // with `-1`, which is a transport-shaped failure and not a statement about
    // any currency. The command still answers, and says what it does not know
    // instead of printing a currency that has no name.
    let node = scripted_node(vec![(LAUNCH_TXID.into(), raw_transaction_reply())]);

    let assertion = pecu()
        .args([
            "tx",
            "explain",
            LAUNCH_TXID,
            "--node",
            &node,
            "--theme",
            "phosphor",
        ])
        .env("PECU_WIDTH", "84")
        .assert()
        .success();
    let rendered = String::from_utf8_lossy(&assertion.get_output().stdout).into_owned();

    assert!(rendered.contains("(name unknown)"), "{rendered}");
    assert!(
        rendered.contains("iJhCezBExJHvtyH3fGhNnt2NhU4Ztkf2yq"),
        "{rendered}"
    );
}

#[test]
fn the_explain_record_outlives_the_run_that_failed() {
    // `--explain` is a record of the calls a command made, and it was printed
    // only on the two arms that succeeded — so the one run where the record is
    // worth having, the one where a call blew up, printed nothing about it.
    // Here the node refuses the txid and the command exits non-zero; the panel
    // still names the call that was made and what it came back with.
    let node = scripted_node(vec![(
        LAUNCH_TXID.into(),
        r#"{"error":{"code":-5,"message":"No information available about transaction"},"id":1}"#
            .into(),
    )]);

    pecu()
        .args(["tx", "explain", LAUNCH_TXID, "--node", &node, "--explain"])
        .assert()
        .failure()
        .stderr(contains("knows no transaction"))
        .stdout(contains("SDK CALLS"))
        .stdout(contains("node.raw_transaction("));
}

#[test]
fn a_currency_the_node_has_no_record_of_is_not_a_lookup_that_failed() {
    // `-8` is what a daemon answers a `getcurrency` miss with, and it is the one
    // refusal that is a statement about the currency. It still is not a
    // statement that the currency is nameless — this output is holding a balance
    // in it — so the panel says what was actually answered.
    let node = scripted_node(vec![
        (LAUNCH_TXID.into(), raw_transaction_reply()),
        (
            "iJhCezBExJHvtyH3fGhNnt2NhU4Ztkf2yq".into(),
            r#"{"error":{"code":-8,"message":"Invalid currency or currency not found"},"id":1}"#
                .into(),
        ),
    ]);

    let assertion = pecu()
        .args(["tx", "explain", LAUNCH_TXID, "--node", &node, "--json"])
        .assert()
        .success();
    let stdout = String::from_utf8_lossy(&assertion.get_output().stdout).into_owned();
    let document: serde_json::Value =
        serde_json::from_str(&stdout).unwrap_or_else(|error| panic!("not json: {error}\n{stdout}"));

    let deposit = document["outputs"]
        .as_array()
        .expect("outputs")
        .iter()
        .find(|output| output["kind"] == "reserve_deposit")
        .expect("a reserve deposit");
    let token = &deposit["tokens"][0];
    assert_eq!(
        token["name"],
        serde_json::json!({
            "known": true,
            "name": null,
            "reason": "the node has no currency with this id",
        }),
        "{token}"
    );
}
