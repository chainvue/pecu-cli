//! Surface-level checks on the command tree. Offline: nothing here touches a node.

use assert_cmd::Command;
use predicates::str::contains;

fn pecu() -> Command {
    Command::cargo_bin("pecu").expect("the pecu binary should be built")
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
    pecu()
        .arg("--version")
        .assert()
        .success()
        .stdout(contains("verus-sdk rev ae08bc0"));
}

#[test]
fn unimplemented_commands_say_so_and_exit_non_zero() {
    // `key gen` rather than an implemented command, so this stays offline and
    // keeps testing the stub path as milestones land.
    pecu()
        .args(["key", "gen"])
        .assert()
        .failure()
        .stderr(contains("not implemented"))
        .stderr(contains("M3"));
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
    // moving them onto the root command only.
    pecu()
        .args(["key", "gen", "--json", "--dry-run", "--explain", "-y"])
        .assert()
        .failure()
        .stderr(contains("not implemented"));
}
