//! Surface-level checks on the command tree. Offline: nothing here touches a node.

use assert_cmd::Command;
use predicates::str::contains;

/// Every invocation is pointed at a throwaway config root. Nothing here should
/// write anything, but a test suite that *could* touch a real keystore is one
/// refactor away from doing it.
fn pecu() -> Command {
    let mut command = Command::cargo_bin("pecu").expect("the pecu binary should be built");
    command.env("PECU_HOME", tempfile::tempdir().expect("a temp dir").keep());
    command
}

#[test]
fn bare_invocation_shows_help_and_fails() {
    pecu()
        .assert()
        .failure()
        .stderr(contains("Usage: pecu"))
        .stderr(contains("doctor"));
}

#[test]
fn help_lists_every_top_level_command() {
    let expected = [
        "doctor",
        "key",
        "wallet",
        "tx",
        "send",
        "plan",
        "sign",
        "broadcast",
        "id",
        "completions",
    ];
    let output = pecu().arg("--help").assert().success();
    let stdout = String::from_utf8(output.get_output().stdout.clone()).unwrap();
    for command in expected {
        assert!(stdout.contains(command), "`{command}` missing from --help");
    }
}

#[test]
fn long_version_names_the_pinned_sdk_revision() {
    // Read from `Cargo.toml` rather than written out again here. The revision
    // lives in two places — the dependency and a literal in `cli.rs`, which
    // cannot read it — and an assertion that repeated it would be a third,
    // failing only after the first two had already drifted apart.
    let manifest = include_str!("../Cargo.toml");
    let rev = manifest
        .lines()
        .find_map(|line| line.split_once("rev = \"")?.1.split('"').next())
        .expect("Cargo.toml should pin verus-sdk by rev");

    pecu()
        .arg("--version")
        .assert()
        .success()
        .stdout(contains(format!("verus-sdk rev {}", &rev[..7])));
}

#[test]
fn unimplemented_commands_say_so_and_exit_non_zero() {
    // The last stubs left, and parked rather than merely unwritten: the SDK has
    // no flows for them yet. When they land, delete this test — there will be
    // no stub for it to be about.
    for command in ["update", "revoke", "recover"] {
        pecu()
            .args(["id", command])
            .assert()
            .failure()
            .stderr(contains("not implemented"))
            .stderr(contains("M7b"));
    }
}

#[test]
fn completions_generate_for_zsh() {
    pecu()
        .args(["completions", "zsh"])
        .assert()
        .success()
        .stdout(contains("#compdef pecu"));
}

#[test]
fn global_flags_are_accepted_after_the_subcommand() {
    // --json and friends are declared global; regression guard against someone
    // moving them onto the root command only. `key list` because it is the
    // cheapest implemented command that needs neither a node nor a passphrase.
    pecu()
        .args(["key", "list", "--json", "--dry-run", "--explain", "-y"])
        .assert()
        .success()
        .stdout(contains("\"keys\""));
}
