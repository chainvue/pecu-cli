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
    pecu()
        .arg("--version")
        .assert()
        .success()
        .stdout(contains("verus-sdk rev ae08bc0"));
}

#[test]
fn unimplemented_commands_say_so_and_exit_non_zero() {
    // Deliberately the last command scheduled to land, so this needs updating
    // as rarely as possible. When `id read` ships, delete this test — there
    // will be no stub left for it to be about.
    pecu()
        .args(["id", "read"])
        .assert()
        .failure()
        .stderr(contains("not implemented"))
        .stderr(contains("M8"));
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
