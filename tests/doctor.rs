//! `pecu doctor`, end to end.
//!
//! Every test here points `PECU_HOME` at a temporary directory, so none of them
//! can see — or damage — a real config or keystore. All but the last run
//! offline; the network one is `#[ignore]`d and runs under `make testnet`.

use assert_cmd::Command;
use predicates::str::contains;
use tempfile::TempDir;

/// An unroutable address that fails fast rather than hanging: port 1 on
/// loopback has nothing listening, and `connect` is refused immediately.
const DEAD_NODE: &str = "https://127.0.0.1:1";

fn pecu(home: &TempDir) -> Command {
    let mut command = Command::cargo_bin("pecu").expect("the pecu binary should be built");
    command.env("PECU_HOME", home.path()).env_remove("NO_COLOR");
    command
}

fn home() -> TempDir {
    tempfile::tempdir().expect("a temp dir")
}

fn write_config(home: &TempDir, contents: &str) {
    std::fs::write(home.path().join("config.toml"), contents).expect("writable temp dir");
}

#[test]
fn an_unreachable_node_still_reports_the_local_half_then_fails() {
    let home = home();
    let assertion = pecu(&home)
        .args(["doctor", "--node", DEAD_NODE])
        .assert()
        .failure();
    let output = assertion.get_output();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    // The half that does not need a node is still worth having.
    assert!(stdout.contains("testnet"), "profile missing:\n{stdout}");
    assert!(
        stdout.contains("verus-sdk"),
        "build info missing:\n{stdout}"
    );
    assert!(stdout.contains(DEAD_NODE), "endpoint missing:\n{stdout}");
    // And the failure is a diagnostic, not a backtrace.
    assert!(
        stderr.contains("pecu::node_unreachable"),
        "no diagnostic:\n{stderr}"
    );
    assert!(stderr.contains("--node"), "no advice:\n{stderr}");
}

#[test]
fn json_is_emitted_even_when_the_node_is_down() {
    let home = home();
    let assertion = pecu(&home)
        .args(["doctor", "--node", DEAD_NODE, "--json"])
        .assert()
        .failure();
    let stdout = String::from_utf8_lossy(&assertion.get_output().stdout).into_owned();
    let document: serde_json::Value = serde_json::from_str(&stdout).expect("valid json");

    assert_eq!(document["node"]["reachable"], false);
    assert_eq!(document["profile"]["name"], "testnet");
    assert_eq!(document["paths"]["config_exists"], false);
    assert_eq!(document["build"]["features"][0], "network");
    assert!(document["node"]["error"].is_string());
}

#[test]
fn plaintext_http_is_refused_before_anything_is_sent() {
    let home = home();
    pecu(&home)
        .args(["doctor", "--node", "http://example.com"])
        .assert()
        .failure()
        .stderr(contains("pecu::bad_endpoint"))
        .stderr(contains("plaintext"));
}

#[test]
fn an_unknown_profile_names_the_ones_that_exist() {
    let home = home();
    pecu(&home)
        .args(["doctor", "--profile", "nope"])
        .assert()
        .failure()
        .stderr(contains("no profile named `nope`"))
        .stderr(contains("mainnet, testnet"));
}

#[test]
fn mainnet_reports_that_it_cannot_spend() {
    let home = home();
    pecu(&home)
        .args(["doctor", "--profile", "mainnet", "--node", DEAD_NODE])
        .assert()
        .failure()
        .stdout(contains("spending is off for this profile"));
}

#[test]
fn a_config_file_can_add_a_profile() {
    let home = home();
    write_config(
        &home,
        "default_profile = \"mine\"\n\n[profiles.mine]\ncurrency = \"WIDGET\"\n",
    );
    let assertion = pecu(&home)
        .args(["doctor", "--node", DEAD_NODE, "--json"])
        .assert()
        .failure();
    let stdout = String::from_utf8_lossy(&assertion.get_output().stdout).into_owned();
    let document: serde_json::Value = serde_json::from_str(&stdout).expect("valid json");

    assert_eq!(document["profile"]["name"], "mine");
    assert_eq!(document["profile"]["currency"], "WIDGET");
    assert_eq!(document["paths"]["config_exists"], true);
}

#[test]
fn the_profile_environment_variable_is_honoured() {
    let home = home();
    pecu(&home)
        .env("PECU_PROFILE", "mainnet")
        .args(["doctor", "--node", DEAD_NODE])
        .assert()
        .failure()
        .stdout(contains("mainnet"));
}

#[test]
#[ignore = "talks to api.verustest.net"]
fn a_healthy_testnet_node_passes() {
    let home = home();
    let assertion = pecu(&home).args(["doctor", "--json"]).assert().success();
    let stdout = String::from_utf8_lossy(&assertion.get_output().stdout).into_owned();
    let document: serde_json::Value = serde_json::from_str(&stdout).expect("valid json");

    assert_eq!(document["node"]["reachable"], true);
    assert_eq!(document["node"]["chain"], "VRSCTEST");
    assert!(
        document["node"]["blocks"].as_u64().unwrap_or(0) > 1_000_000,
        "tip looks wrong: {}",
        document["node"]["blocks"]
    );
}
