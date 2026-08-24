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

/// `-v` shipped in the scaffold, was wired to nothing, and was advertised on
/// every help screen until #55. It is refused rather than deleted so that the
/// answer is `pecu`'s own, and rather than swallowed so that there is an answer
/// at all. See `cmd::VerboseDoesNothing`.
#[test]
fn verbose_is_refused_by_name_and_exits_one() {
    let output = pecu()
        .args(["key", "list", "-v"])
        .assert()
        // `1`, not clap's `2`: the request was understood and the answer is no.
        .code(1);
    let stderr = String::from_utf8(output.get_output().stderr.clone()).unwrap();

    assert!(stderr.contains("pecu::verbose_does_nothing"), "{stderr}");
    assert!(
        stderr.contains("-v/--verbose turns up logging this build does not have"),
        "{stderr}"
    );
    // The point of declaring it: clap's vocabulary is what this replaces.
    assert!(!stderr.contains("unexpected argument"), "{stderr}");
    // And the reader is sent somewhere that works, not left with a removal.
    assert!(stderr.contains("--explain"), "{stderr}");
}

/// Repeating it, spelling it out, and putting it where a legacy script would
/// have — `-vv` was the form the docs actually promised, `--verbose` is the one
/// clap would have answered with `tip: a similar argument exists: '--version'`,
/// and the flag was `global`, so root position is where a script most likely
/// wrote it.
#[test]
fn every_spelling_and_position_of_verbose_gets_the_same_answer() {
    for form in [
        vec!["key", "list", "-vv"],
        vec!["key", "list", "-vvv"],
        vec!["key", "list", "--verbose"],
        // Root, ahead of the subcommand.
        vec!["-v", "key", "list"],
        vec!["--verbose", "key", "list"],
        // And mid-tree, between the two.
        vec!["key", "-v", "list"],
        // Bundled with another short flag, the way `-y` would be written.
        vec!["key", "list", "-yv"],
    ] {
        let output = pecu().args(&form).assert().code(1);
        let stderr = String::from_utf8(output.get_output().stderr.clone()).unwrap();
        assert!(
            stderr.contains("pecu::verbose_does_nothing"),
            "{form:?}: {stderr}"
        );
        assert!(!stderr.contains("--version"), "{form:?}: {stderr}");
        // Whatever was typed, the answer names both spellings rather than
        // telling a `--verbose` caller to drop a `-v` they never wrote.
        assert!(stderr.contains("-v/--verbose"), "{form:?}: {stderr}");
    }
}

/// After `--`, `-v` is a value and nothing may reinterpret it. The refusal is a
/// `dispatch` gate rather than a scan of `argv` precisely so that this holds —
/// a pre-clap scan is what would break it.
#[test]
fn a_v_that_is_not_the_flag_is_left_alone() {
    let output = pecu().args(["key", "show", "--", "-v"]).assert().code(1);
    let stderr = String::from_utf8(output.get_output().stderr.clone()).unwrap();

    assert!(stderr.contains("pecu::bad_label"), "{stderr}");
    assert!(!stderr.contains("verbose_does_nothing"), "{stderr}");
}

/// The three places `-v` does not reach the refusal, pinned so that they are a
/// decision rather than a surprise. All are clap finishing first, and all are
/// documented in `docs/configuration.md`.
#[test]
fn clap_settles_these_before_the_refusal_gate_runs() {
    // `--help` and `--version` are resolved inside `get_matches`, so `dispatch`
    // never runs and `-v` is silently swallowed at exit `0`. Unreachable without
    // an `argv` scan ahead of clap, which would break the test above.
    for form in [["--help"], ["--version"]] {
        pecu()
            .args(["key", "list", "-v"])
            .args(form)
            .assert()
            .code(0);
    }

    // A command line already invalid for another reason keeps that reason:
    // `<LABEL>` is missing, which is clap's `2`, not this refusal.
    let output = pecu().args(["key", "show", "-v"]).assert().code(2);
    let stderr = String::from_utf8(output.get_output().stderr.clone()).unwrap();
    assert!(
        stderr.contains("required arguments were not provided"),
        "{stderr}"
    );
    assert!(!stderr.contains("verbose_does_nothing"), "{stderr}");
}

/// Exit `2` prints nothing on stdout, so deleting the flag would have handed a
/// `--json` consumer an empty stream. Refusing it keeps the run inside the
/// documented failure shape: one document, with `.error.code` in it.
#[test]
fn a_refused_verbose_still_prints_a_json_document() {
    let output = pecu()
        .args(["key", "list", "-v", "--json"])
        .assert()
        .code(1);
    let stdout = String::from_utf8(output.get_output().stdout.clone()).unwrap();
    let document: serde_json::Value = serde_json::from_str(&stdout)
        .unwrap_or_else(|error| panic!("stdout should be one document: {error}: {stdout:?}"));

    assert_eq!(document["error"]["code"], "pecu::verbose_does_nothing");
    assert!(
        document["error"]["help"]
            .as_str()
            .is_some_and(|help| help.contains("--explain")),
        "{document}"
    );
}

/// The complaint in #55 was the advertising, not only the no-op: `global = true`
/// put "Log more; repeat for more still" under every command. Both help forms,
/// at the root and under a subcommand, because `-h` and `--help` render from
/// different fields.
///
/// This is a sample of rendered output, not the coverage claim. All 39 contexts
/// are covered structurally by
/// `cli::tests::verbose_reaches_every_parser_context_hidden_and_no_completion_context_at_all`,
/// which is the test to widen if a context is ever missed.
#[test]
fn no_help_screen_advertises_verbose_and_all_of_them_still_offer_explain() {
    for args in [
        vec!["--help"],
        vec!["-h"],
        vec!["wallet", "balance", "--help"],
        vec!["send", "--help"],
    ] {
        let output = pecu().args(&args).assert().success();
        let stdout = String::from_utf8(output.get_output().stdout.clone()).unwrap();

        assert!(!stdout.contains("--verbose"), "{args:?}: {stdout}");
        assert!(!stdout.contains("Log more"), "{args:?}: {stdout}");
        // The flag that does the job -v was reaching for is untouched, and is
        // still offered in the same block.
        assert!(stdout.contains("--explain"), "{args:?}: {stdout}");
    }
}

/// The flag was promised 39 times per shell, in all five of them. They regress
/// together — the leak is `Cli::command()` carrying the argument at all, not
/// anything shell-specific — but the claim in `docs/configuration.md` is about
/// the completion scripts, plural, so all five are asserted.
#[test]
fn completion_scripts_no_longer_offer_verbose() {
    for shell in ["bash", "zsh", "fish", "powershell", "elvish"] {
        let output = pecu().args(["completions", shell]).assert().success();
        let stdout = String::from_utf8(output.get_output().stdout.clone()).unwrap();

        assert!(
            !stdout.contains("verbose"),
            "{shell}: script still offers it"
        );
        assert!(stdout.contains("explain"), "{shell}: script lost --explain");
    }
}

/// `-v` and `--explain` sit in the same `Globals`, and the refusal gate runs
/// before every command — a gate that caught the wrong flag, or that ran for
/// every invocation rather than for `-v`, would show up here.
#[test]
fn the_verbose_refusal_leaves_explain_working() {
    pecu()
        .args(["key", "list", "--explain", "--json"])
        .assert()
        .success()
        .stdout(contains("\"keys\""));

    // Passed together, only `-v` is the problem, and the refusal says so
    // without claiming anything about `--explain`.
    pecu()
        .args(["key", "list", "-v", "--explain"])
        .assert()
        .code(1)
        .stderr(contains("pecu::verbose_does_nothing"));
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
