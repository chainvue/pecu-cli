//! What a failing run looks like to a script.
//!
//! Three things have to hold together on every failing path, and each test here
//! asserts all three rather than one: a `--json` run puts a machine-readable
//! error on **stdout**, the rendered report still goes to **stderr**, and the
//! **exit code** says which kind of failure it was.
//!
//! Offline. `PECU_HOME` is a throwaway directory everywhere, and the one node
//! any of this points at has nothing listening on it.

use assert_cmd::Command;
use tempfile::TempDir;

/// An unroutable address that fails fast rather than hanging: port 1 on
/// loopback has nothing listening, and `connect` is refused immediately.
const DEAD_NODE: &str = "https://127.0.0.1:1";

fn home() -> TempDir {
    tempfile::tempdir().expect("a temp dir")
}

fn pecu(home: &TempDir) -> Command {
    let mut command = Command::cargo_bin("pecu").expect("the pecu binary should be built");
    command
        .env("PECU_HOME", home.path())
        .env_remove("NO_COLOR")
        .env_remove("PECU_THEME");
    command
}

/// Every document in `text`, so "exactly one" can be asserted rather than
/// assumed from the fact that the first one parsed.
fn documents(text: &str) -> Vec<serde_json::Value> {
    serde_json::Deserializer::from_str(text)
        .into_iter::<serde_json::Value>()
        .map(|document| document.unwrap_or_else(|error| panic!("not json: {error}\n{text}")))
        .collect()
}

fn only_document(text: &str) -> serde_json::Value {
    let documents = documents(text);
    assert_eq!(documents.len(), 1, "expected one document:\n{text}");
    documents.into_iter().next().expect("one document")
}

/// The shape every failing `--json` run answers with. `--json` used to produce
/// an empty stdout here, which is worse than it sounds: `jq` accepts empty
/// input and exits 0, so the pipeline succeeded silently.
#[test]
fn a_json_failure_names_its_diagnostic_code_on_stdout() {
    let home = home();
    let assertion = pecu(&home)
        .args(["tx", "explain", "deadbeef", "--json"])
        .assert()
        .code(1);
    let output = assertion.get_output();
    let document = only_document(&String::from_utf8_lossy(&output.stdout));
    let error = &document["error"];

    // The discriminator, verbatim — the same token stderr prints after
    // `Error:`, so nobody has to regex prose to get it.
    assert_eq!(error["code"], "pecu::undecodable");
    assert_eq!(
        error["message"],
        "these bytes are not a transaction, and not an output script either"
    );
    assert!(error["help"].is_string(), "{document:#}");
    assert!(error["causes"].is_array(), "always an array:\n{document:#}");

    // And the human half is untouched.
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("Error: pecu::undecodable"),
        "the report is the contract:\n{stderr}"
    );
    assert!(stderr.contains('×'), "not the rendered report:\n{stderr}");
}

/// The report is what a person reads, and it is not this change's to alter.
/// Asking for JSON adds a document on stdout and changes stderr by nothing at
/// all — asserted as bytes, because "looks the same" is how wrap widths drift.
#[test]
fn asking_for_json_does_not_change_the_rendered_report() {
    let home = home();
    let rendered = pecu(&home)
        .args(["tx", "explain", "deadbeef"])
        .assert()
        .code(1);
    let machine = pecu(&home)
        .args(["tx", "explain", "deadbeef", "--json"])
        .assert()
        .code(1);

    assert_eq!(
        rendered.get_output().stderr,
        machine.get_output().stderr,
        "--json is additive; the report on stderr is the same bytes"
    );
    assert!(
        rendered.get_output().stdout.is_empty(),
        "the rendered path writes its failure to stderr only"
    );
    only_document(&String::from_utf8_lossy(&machine.get_output().stdout));
}

/// Item 4 of #49, from the outside: "the node never answered" is a different
/// answer from "the node answered and the answer was no", and a script has to
/// be able to tell them apart without reading either stream. This is the first
/// half — the second is in `tests/send.rs`, which has a node that can answer.
#[test]
fn a_node_that_could_not_be_reached_exits_three() {
    let home = home();
    let assertion = pecu(&home)
        .args([
            "wallet",
            "balance",
            "--address",
            "bob@",
            "--json",
            "--node",
            DEAD_NODE,
        ])
        .assert()
        .code(3);
    let output = assertion.get_output();

    let document = only_document(&String::from_utf8_lossy(&output.stdout));
    assert_eq!(document["error"]["code"], "pecu::node_unreachable");
    // The SDK's own wording, which the rendered report shows under `╰─▶` and
    // which the message alone does not carry.
    assert!(
        document["error"]["causes"][0]
            .as_str()
            .unwrap_or_default()
            .contains("transport"),
        "{document:#}"
    );
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("pecu::node_unreachable"),
        "the report is still on stderr"
    );
}

/// A refusal that never involved a node at all. Nothing about running it again
/// is different, which is what `1` means.
#[test]
fn a_local_refusal_exits_one() {
    let home = home();
    let assertion = pecu(&home)
        .args(["send", "--to", "bob@", "--amount", "1", "--yes", "--json"])
        .assert()
        .code(1);
    let document = only_document(&String::from_utf8_lossy(&assertion.get_output().stdout));
    // No keystore, so there is nothing to spend from and no request was made.
    assert_eq!(document["error"]["code"], "pecu::no_key");
}

/// `doctor` prints a document *and* fails, and the local half of that document
/// is the reason it prints at all. So the error object goes inside it: two
/// documents would break `| jq`, and none would leave the one command that
/// already handled this case as the odd one out.
#[test]
fn doctor_carries_the_error_inside_its_own_document() {
    let home = home();
    let assertion = pecu(&home)
        .args(["doctor", "--node", DEAD_NODE, "--json"])
        .assert()
        .code(3);
    let output = assertion.get_output();
    let document = only_document(&String::from_utf8_lossy(&output.stdout));

    assert_eq!(document["error"]["code"], "pecu::node_unreachable");
    // The local half, unchanged.
    assert_eq!(document["node"]["reachable"], false);
    assert_eq!(document["profile"]["name"], "testnet");
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("pecu::node_unreachable"),
        "the report is still on stderr"
    );
}

/// clap answers a malformed command line before any of this runs, and it has
/// always exited 2. Left alone on purpose: `2` is already taken, which is why
/// the split above starts at `3`.
#[test]
fn a_usage_error_is_still_clap_s_exit_two() {
    let home = home();
    let assertion = pecu(&home)
        .args(["key", "export", "--json"])
        .assert()
        .code(2);
    assert!(
        assertion.get_output().stdout.is_empty(),
        "clap never reaches the JSON handler"
    );
}

/// A consumer that stops reading is ordinary — `pecu … --json | head -1`,
/// `| jq -e …`, `| grep -q …` — and the document was printed with `println!`,
/// which panics on a broken pipe. The run that exists to answer with a
/// documented exit code answered `101` instead, and the panic came *before* the
/// report reached stderr, so the human half was lost too.
#[test]
fn a_consumer_that_stopped_reading_still_gets_the_exit_code() {
    let home = home();
    let mut child = std::process::Command::new(assert_cmd::cargo::cargo_bin("pecu"))
        .args(["tx", "explain", "deadbeef", "--json"])
        .env("PECU_HOME", home.path())
        .env_remove("NO_COLOR")
        .env_remove("PECU_THEME")
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("the pecu binary should be built");

    // Closed while the child is still starting up, so the write it makes on the
    // way out has nowhere to go.
    drop(child.stdout.take());
    let output = child.wait_with_output().expect("the child ran");

    assert_eq!(
        output.status.code(),
        Some(1),
        "a closed pipe is not a panic: {:?}",
        output.status
    );
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("pecu::undecodable"),
        "the report has to survive the pipe closing:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// `--explain` is documented as working on any command, and stdout under
/// `--json` belongs to the document — so the panel goes to stderr rather than
/// being dropped or printed in front of the JSON.
#[test]
fn explain_under_json_goes_to_stderr() {
    let home = home();
    pecu(&home)
        .args(["key", "gen", "--label", "demo"])
        .env("PECU_PASSPHRASE", "correct horse battery staple")
        .assert()
        .success();

    let assertion = pecu(&home)
        .args([
            "send",
            "--to",
            "bob@",
            "--amount",
            "1",
            "--yes",
            "--json",
            "--explain",
            "--node",
            DEAD_NODE,
        ])
        .env("PECU_PASSPHRASE", "correct horse battery staple")
        .assert()
        .code(3);
    let output = assertion.get_output();

    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    only_document(&stdout);
    assert!(
        !stdout.contains("SDK CALLS"),
        "the panel is prose, and stdout is the parsed stream:\n{stdout}"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("SDK CALLS") && stderr.contains("node.identity"),
        "the record is what --explain was asked for:\n{stderr}"
    );
}
