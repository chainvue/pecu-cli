//! `pecu currency show` · `pecu currency launch`.
//!
//! A launch is one-way — the defining identity can never define another — and
//! costs 200 VRSCTEST, so the offline tests are the refusals that must happen
//! before any of that, and the network tests read rather than write.

use assert_cmd::Command;
use predicates::prelude::*;
use predicates::str::contains;
use tempfile::TempDir;

const PASSPHRASE: &str = "correct horse battery staple";
const DEAD_NODE: &str = "https://127.0.0.1:1";

/// A real token on VRSCTEST: `options = 32`, `proofprotocol = 2`.
const TOKEN: &str = "iK2k8YH1jfR7RLmEZ3zac2Mkx5rxSgbMqg";

fn home() -> TempDir {
    tempfile::tempdir().expect("a temp dir")
}

fn pecu(home: &TempDir) -> Command {
    let mut command = Command::cargo_bin("pecu").expect("the pecu binary should be built");
    command
        .env("PECU_HOME", home.path())
        .env("PECU_PASSPHRASE", PASSPHRASE)
        .env_remove("NO_COLOR")
        .env_remove("PECU_THEME");
    command
}

fn generate(home: &TempDir, label: &str) {
    pecu(home)
        .args(["key", "gen", "--label", label])
        .assert()
        .success();
}

/// Output with every run of whitespace collapsed to one space.
///
/// miette hard-wraps help text to the terminal width, and that width differs
/// between an interactive run and a captured one — so a phrase that is on one
/// line by hand straddles two under `cargo test`. Three assertions have been
/// written and rewritten around that. Flattening first makes a multi-word
/// assertion mean what it says instead of testing the wrap.
fn flat(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// An identity that has not yet defined a currency, for the dry-run tests.
///
/// A currency slot is one-shot, and these tests keep outliving the identity
/// they were written against — three times now a launch done by hand has turned
/// a passing test into a false failure. So the name is overridable, and a slot
/// that has been used skips rather than fails: "this identity is spent" is a
/// fact about the chain, not a defect in the code under test.
fn spare_identity() -> String {
    fixture("PECU_SPARE_ID", "pecuspare2@")
}

/// A pre-launch fractional basket, for the preconvert tests.
fn test_basket() -> String {
    fixture("PECU_BASKET_ID", "pecubask2@")
}

/// A named on-chain fixture, overridable.
///
/// These are real identities on a real chain and they do not last: a currency
/// slot gets used, a basket launches, a keystore is lost. Naming them in one
/// place with an env override means replacing a dead fixture is a variable, not
/// a patch across nine call sites — which is what it was the first time.
fn fixture(variable: &str, default: &str) -> String {
    std::env::var(variable).unwrap_or_else(|_| default.to_string())
}

/// True when the launch was refused because the slot is gone.
fn slot_is_spent(stderr: &str) -> bool {
    stderr.contains("already defines a currency")
}

#[test]
fn mainnet_will_not_launch_without_being_told_to() {
    let home = home();
    generate(&home, "demo");
    pecu(&home)
        .args(["currency", "launch", "alice@", "--profile", "mainnet"])
        .assert()
        .failure()
        .stderr(contains("not allowed to spend"));
}

#[test]
fn launching_needs_a_key() {
    let home = home();
    pecu(&home)
        .args(["currency", "launch", "alice@", "--node", DEAD_NODE])
        .assert()
        .failure()
        .stderr(contains("no key to sign with"));
}

#[test]
fn a_preallocation_to_a_transparent_address_is_refused_before_anything_else() {
    let home = home();
    generate(&home, "demo");
    // The supply is held by whoever controls an identity, so a preallocation
    // names one. Caught before the keystore is touched.
    pecu(&home)
        .env_remove("PECU_PASSPHRASE")
        .args([
            "currency",
            "launch",
            "alice@",
            "--preallocate",
            "RComfCn4wHHsGR8vWBAU7T1r3tHHyxN9Hm:100",
            "--node",
            DEAD_NODE,
        ])
        .assert()
        .failure()
        .stderr(contains("is not `address:amount`"))
        .stderr(contains("passphrase").not());
}

#[test]
fn a_start_block_and_a_start_offset_cannot_both_be_given() {
    let home = home();
    generate(&home, "demo");
    pecu(&home)
        .args([
            "currency",
            "launch",
            "alice@",
            "--start-block",
            "1200000",
            "--start-in",
            "50",
            "--node",
            DEAD_NODE,
        ])
        .assert()
        .failure()
        .stderr(contains("cannot be used with"));
}

/// The bitfield and the proof protocol are what a currency *is*, and neither is
/// inferable from its name. Reading `options: 32` off a panel tells nobody
/// anything.
#[test]
#[ignore = "talks to api.verustest.net"]
fn a_currency_reads_back_decoded_rather_than_as_a_number() {
    let home = home();
    let assertion = pecu(&home)
        .args(["currency", "show", TOKEN, "--json"])
        .assert()
        .success();
    let stdout = String::from_utf8_lossy(&assertion.get_output().stdout).into_owned();
    let document: serde_json::Value = serde_json::from_str(&stdout).expect("valid json");

    assert_eq!(document["currency_id"], TOKEN);
    assert_eq!(document["options"], 32);
    // Raw *and* decoded: a consumer should not need the bit values to use this.
    assert_eq!(document["kinds"][0], "token");
    assert_eq!(document["proof_protocol"], 2);
    assert!(
        document["control"]
            .as_str()
            .unwrap_or_default()
            .contains("can mint"),
        "{document:#}"
    );
    // The raw definition is carried through for anything this build does not
    // decode yet.
    assert!(document["definition"].is_object(), "{document:#}");
}

/// `getcurrency` takes a bare name; every other command here takes the `@` form.
#[test]
#[ignore = "talks to api.verustest.net"]
fn a_currency_is_findable_with_or_without_the_at_sign() {
    let home = home();
    let id = |args: &[&str]| {
        let assertion = pecu(&home).args(args).assert().success();
        let stdout = String::from_utf8_lossy(&assertion.get_output().stdout).into_owned();
        serde_json::from_str::<serde_json::Value>(&stdout).expect("json")["currency_id"]
            .as_str()
            .expect("an id")
            .to_string()
    };
    assert_eq!(
        id(&["currency", "show", "TST", "--json"]),
        id(&["currency", "show", "TST@", "--json"])
    );
}

#[test]
#[ignore = "talks to api.verustest.net"]
fn a_currency_nobody_defined_is_an_answer_not_a_crash() {
    let home = home();
    pecu(&home)
        .args(["currency", "show", "nothing-is-called-this-surely@"])
        .assert()
        // The daemon read the name and said no. `1` is what that means, and it
        // is the half of the pair below that needs a node to answer at all.
        .code(1)
        .stderr(contains("nothing on this chain"));
}

/// The other half: a node that never answered has denied nothing. Reporting it
/// as "no such currency" was wrong before and is worse now that the exit code
/// carries the same claim — a script reading `1` would stop looking, and the
/// currency may exist perfectly well.
#[test]
fn an_unreachable_node_is_not_a_currency_that_does_not_exist() {
    let home = home();
    let assertion = pecu(&home)
        .args(["currency", "show", "tok@", "--node", DEAD_NODE, "--json"])
        .assert()
        .code(3);
    let output = assertion.get_output();
    let document: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("one json document");
    assert_eq!(document["error"]["code"], "pecu::node_unreachable");
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("reading the currency failed"),
        "the report names the request, not the currency:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// The same for the lookups behind the commands that spend. `mint` and
/// `preconvert` each read a definition through a different call site, and each
/// one used to flatten a dead node into "nothing on this chain is called that".
#[test]
fn a_spend_against_an_unreachable_node_does_not_deny_the_currency() {
    let home = home();
    generate(&home, "demo");
    for args in [
        vec![
            "currency",
            "mint",
            "tok@",
            "--to",
            "RComfCn4wHHsGR8vWBAU7T1r3tHHyxN9Hm",
            "--amount",
            "1",
            "--node",
            DEAD_NODE,
            "--json",
            "--yes",
        ],
        vec![
            "currency",
            "preconvert",
            "tok@",
            "--amount",
            "1",
            "--to",
            "RComfCn4wHHsGR8vWBAU7T1r3tHHyxN9Hm",
            "--node",
            DEAD_NODE,
            "--json",
            "--yes",
        ],
    ] {
        let assertion = pecu(&home).args(&args).assert().code(3);
        let document: serde_json::Value =
            serde_json::from_slice(&assertion.get_output().stdout).expect("one json document");
        assert_eq!(
            document["error"]["code"], "pecu::node_unreachable",
            "{args:?} still blames the currency"
        );
    }
}

/// `--supply` has to become a preallocation, because a token's supply is the
/// sum of its preallocations and `initial_supply` is read only for a fractional
/// currency. Setting the wrong field launches a token with no supply at all.
#[test]
#[ignore = "talks to api.verustest.net"]
fn supply_lands_where_the_chain_actually_reads_it() {
    let Ok(funded) = std::env::var("PECU_FUNDED_HOME") else {
        eprintln!("PECU_FUNDED_HOME is not set — skipping");
        return;
    };

    let identity = spare_identity();
    // A dry run consumes nothing, so the slot stays free for whoever wants it.
    let assertion = Command::cargo_bin("pecu")
        .expect("built")
        .env("PECU_HOME", &funded)
        .args([
            "currency",
            "launch",
            &identity,
            "--from",
            "faucet",
            "--supply",
            "1000000",
            "--dry-run",
            "--json",
        ])
        .assert();
    let stderr = String::from_utf8_lossy(&assertion.get_output().stderr).into_owned();
    if slot_is_spent(&stderr) {
        eprintln!("{identity} has defined its currency — skipping");
        return;
    }
    let assertion = assertion.success();
    let stdout = String::from_utf8_lossy(&assertion.get_output().stdout).into_owned();
    let document: serde_json::Value = serde_json::from_str(&stdout).expect("valid json");

    assert_eq!(document["broadcast"], false);
    assert_eq!(document["supply"], 100_000_000_000_000u64);
    assert_eq!(document["mintable"], false);

    // The currency's id is the defining identity's own i-address. Read rather
    // than pinned: that identity *is* the currency id is the property under
    // test, and a hardcoded string proves only that somebody typed it twice.
    let shown = Command::cargo_bin("pecu")
        .expect("built")
        .env("PECU_HOME", &funded)
        .args(["id", "show", &identity, "--json"])
        .assert()
        .success();
    let record: serde_json::Value =
        serde_json::from_slice(&shown.get_output().stdout).expect("valid json");
    assert_eq!(
        document["currency_id"], record["identity_address"],
        "the currency id must be the defining identity's own i-address"
    );
}

/// The one-way rule, and the advice that goes with it.
///
/// `pecudepth3@` defined a currency at `2fecffbb…`, so it can never define
/// another. The refusal arrives as `FlowError::NotReady`, not `Content` —
/// matching the wrong variant sent this to "run `pecu doctor`", which is wrong
/// twice over: the node is fine and no retry helps.
#[test]
#[ignore = "talks to api.verustest.net"]
fn an_identity_that_already_defines_a_currency_is_refused_by_name() {
    let Ok(funded) = std::env::var("PECU_FUNDED_HOME") else {
        eprintln!("PECU_FUNDED_HOME is not set — skipping");
        return;
    };

    let assertion = Command::cargo_bin("pecu")
        .expect("built")
        .env("PECU_HOME", &funded)
        .args([
            "currency",
            "launch",
            "pecudepth3@",
            "--from",
            "faucet",
            "--supply",
            "5",
            "--dry-run",
        ])
        .assert()
        .failure();
    let stderr = String::from_utf8_lossy(&assertion.get_output().stderr).into_owned();

    assert!(stderr.contains("already defines a currency"), "{stderr}");
    assert!(stderr.contains("Register another"), "{stderr}");
    assert!(
        !stderr.contains("pecu doctor"),
        "a one-way rule is not a node problem:\n{stderr}"
    );
}

/// What this project launched, read back through its own command.
#[test]
#[ignore = "talks to api.verustest.net"]
fn the_currency_this_project_launched_reads_back() {
    let home = home();
    let assertion = pecu(&home)
        .args(["currency", "show", "pecudepth3@", "--json"])
        .assert()
        .success();
    let stdout = String::from_utf8_lossy(&assertion.get_output().stdout).into_owned();
    let document: serde_json::Value = serde_json::from_str(&stdout).expect("valid json");

    // The currency's id is the defining identity's own i-address.
    assert_eq!(
        document["currency_id"],
        "i7kDJurgpZA63cjPTuyK49CeCKihB5ryDB"
    );
    assert_eq!(document["kinds"][0], "token");
    // Decentralized: `--mintable` was not passed, so the supply is fixed.
    assert_eq!(document["proof_protocol"], 1);

    // The supply is the preallocation, which is the trap `--supply` exists to
    // avoid: setting `initial_supply` here would have launched it empty.
    let preallocations = document["definition"]["preallocations"]
        .as_array()
        .expect("preallocations");
    assert_eq!(preallocations.len(), 1, "{document:#}");
}

/// A basket's weights are its reserve ratios, and consensus reads them as
/// fractions of one whole. Anything else prices it wrongly, permanently.
#[test]
fn reserve_percentages_that_do_not_total_a_hundred_are_refused_before_the_passphrase() {
    let home = home();
    generate(&home, "demo");
    pecu(&home)
        .env_remove("PECU_PASSPHRASE")
        .args([
            "currency",
            "launch",
            "alice@",
            "--supply",
            "100",
            "--reserve",
            "VRSCTEST:30",
            "--reserve",
            "TST:30",
            "--node",
            DEAD_NODE,
        ])
        .assert()
        .failure()
        .stderr(contains("total 60%"))
        .stderr(contains("passphrase").not());
}

#[test]
fn contributing_at_launch_is_refused_before_a_key_is_unlocked() {
    let home = home();
    generate(&home, "demo");
    pecu(&home)
        .env_remove("PECU_PASSPHRASE")
        .args([
            "currency",
            "launch",
            "alice@",
            "--supply",
            "100",
            "--reserve",
            "VRSCTEST:60",
            "--reserve",
            "TST:40",
            "--contribute",
            "VRSCTEST:10",
            "--node",
            DEAD_NODE,
        ])
        .assert()
        .failure()
        .stderr(contains("contribution_unfunded"))
        // The remedy has to be named, because it is the only funded way to put
        // anything into a reserve before the start block.
        .stderr(contains("preconvert"))
        // Refused ahead of the keystore and the node: this is what would fail
        // if the guard drifted back down beside the definition.
        .stderr(contains("passphrase").not());
}

#[test]
fn a_launch_conversion_rate_is_refused_before_a_key_is_unlocked() {
    let home = home();
    generate(&home, "demo");
    pecu(&home)
        .env_remove("PECU_PASSPHRASE")
        .args([
            "currency",
            "launch",
            "alice@",
            "--supply",
            "100",
            "--reserve",
            "VRSCTEST:60",
            "--reserve",
            "TST:40",
            "--conversion",
            "VRSCTEST:4",
            "--node",
            DEAD_NODE,
        ])
        .assert()
        .failure()
        .stderr(contains("launch_price_derived"))
        // The remedy has to name the one number that does move the price, since
        // the flag that sounded like it never did.
        .stderr(contains("--supply"))
        // Refused ahead of the keystore and the node: this is what would fail
        // if the guard drifted back down beside the definition.
        .stderr(contains("passphrase").not());
}

#[test]
fn a_conversion_rate_without_reserves_is_refused_by_name() {
    let home = home();
    generate(&home, "demo");
    pecu(&home)
        .args([
            "currency",
            "launch",
            "alice@",
            "--supply",
            "100",
            "--conversion",
            "VRSCTEST:4",
            "--node",
            DEAD_NODE,
        ])
        .assert()
        .failure()
        .stderr(contains("launch_price_derived"))
        // Not clap's "the following required arguments were not provided:
        // --reserve", which said the rate would work once a reserve was given —
        // the exact inversion of the truth, since --reserve is what makes the
        // price derived.
        .stderr(contains("were not provided").not());
}

#[test]
fn launch_help_says_conversion_is_refused() {
    let home = home();
    generate(&home, "demo");
    let assertion = pecu(&home)
        .args(["currency", "launch", "--help"])
        .assert()
        .success();
    // clap wraps this help to the terminal width, and it honours COLUMNS even
    // when stdout is captured, so a narrow terminal breaks the phrase across
    // two lines. Flatten first so the phrase means what it says.
    let stdout = flat(&String::from_utf8_lossy(&assertion.get_output().stdout));
    assert!(stdout.contains("--conversion"), "{stdout}");
    assert!(stdout.contains("Refused"), "{stdout}");
    // The word alone would pass on --contribute's doc: this is the sentence
    // that has to survive, because softening it back into a description of
    // a rate the flag does not set is the whole defect.
    assert!(stdout.contains("derived from --supply"), "{stdout}");
}

#[test]
fn launch_help_says_contribute_is_refused() {
    let home = home();
    generate(&home, "demo");
    pecu(&home)
        .args(["currency", "launch", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("--contribute"))
        .stdout(predicate::str::contains("Refused"));
}

#[test]
fn a_reserve_without_a_percentage_is_refused() {
    let home = home();
    generate(&home, "demo");
    pecu(&home)
        .args([
            "currency",
            "launch",
            "alice@",
            "--supply",
            "100",
            "--reserve",
            "VRSCTEST",
            "--node",
            DEAD_NODE,
        ])
        .assert()
        .failure()
        .stderr(contains("is not `currency:percent`"));
}

#[test]
fn a_basket_cannot_also_be_mintable() {
    let home = home();
    generate(&home, "demo");
    // A basket mints and burns by conversion; `--mintable` is the token idea
    // of an issuer topping up the supply, and the two do not compose.
    pecu(&home)
        .args([
            "currency",
            "launch",
            "alice@",
            "--reserve",
            "VRSCTEST:100",
            "--mintable",
            "--node",
            DEAD_NODE,
        ])
        .assert()
        .failure()
        .stderr(contains("cannot be used with"));
}

/// A basket prices every reserve from the initial supply, so a supply of zero
/// makes every price zero.
#[test]
#[ignore = "talks to api.verustest.net"]
fn a_basket_without_a_supply_is_refused() {
    let Ok(funded) = std::env::var("PECU_FUNDED_HOME") else {
        eprintln!("PECU_FUNDED_HOME is not set — skipping");
        return;
    };
    Command::cargo_bin("pecu")
        .expect("built")
        .env("PECU_HOME", &funded)
        .args([
            "currency",
            "launch",
            &spare_identity(),
            "--from",
            "faucet",
            "--reserve",
            "VRSCTEST:100",
            "--dry-run",
        ])
        .assert()
        .failure()
        // Checked before the chain is asked, so a spent currency slot cannot
        // make this pass for the wrong reason.
        .stderr(contains("needs a supply"));
}

/// The reserves round-trip: a percentage in is the same percentage back, and
/// the definition carries FRACTIONAL alongside TOKEN.
#[test]
#[ignore = "talks to api.verustest.net"]
fn an_uneven_basket_keeps_its_ratios_exactly() {
    let Ok(funded) = std::env::var("PECU_FUNDED_HOME") else {
        eprintln!("PECU_FUNDED_HOME is not set — skipping");
        return;
    };
    let identity = spare_identity();
    let assertion = Command::cargo_bin("pecu")
        .expect("built")
        .env("PECU_HOME", &funded)
        .args([
            "currency",
            "launch",
            &identity,
            "--from",
            "faucet",
            "--supply",
            "100",
            "--reserve",
            "VRSCTEST:62.5",
            "--reserve",
            "TST:37.5",
            "--dry-run",
        ])
        .assert();
    let stderr = String::from_utf8_lossy(&assertion.get_output().stderr).into_owned();
    if slot_is_spent(&stderr) {
        eprintln!("{identity} has defined its currency — skipping");
        return;
    }
    let assertion = assertion.success();
    let stdout = String::from_utf8_lossy(&assertion.get_output().stdout).into_owned();

    assert!(stdout.contains("fractional basket"), "{stdout}");
    assert!(stdout.contains("62.5%"), "{stdout}");
    assert!(stdout.contains("37.5%"), "{stdout}");
    // A basket's supply is not fixed, and the panel must not say it is.
    assert!(stdout.contains("moves as reserves convert"), "{stdout}");
    assert!(
        !stdout.contains("supply is fixed"),
        "the panel contradicts itself:\n{stdout}"
    );
}

/// An NFT launch refused at `bad-txns-failed-precheck` has to come back with the
/// candidate causes rather than the reject code and `pecu doctor`.
///
/// It must not come back with one cause either. Currency launches are switched
/// off chain-wide on VRSCTEST, so this is what any launch is answered with at
/// the moment, and the SDK gap this diagnostic was originally written for
/// closed upstream — naming it would be wrong twice over.
///
/// This one really does broadcast, because a `-25` is only produced by a chain
/// and no offline fixture can prove the mapping fires on the answer consensus
/// actually returns. What it no longer claims is that the attempt is free: a
/// `-25` says a check failed, not that the transaction was refused, so it does
/// not settle whether the 200 VRSCTEST went. That is exactly why the assertions
/// below are on the candidate wording and on the sentence telling the reader to
/// check before resending — the ones that would have to be true for someone
/// reading this output to work out what happened to their fee.
#[test]
#[ignore = "broadcasts a real launch to api.verustest.net, and a -25 does not settle what it cost"]
fn an_nft_is_refused_with_the_candidates_rather_than_the_reject_code() {
    let Ok(funded) = std::env::var("PECU_FUNDED_HOME") else {
        eprintln!("PECU_FUNDED_HOME is not set — skipping");
        return;
    };
    let assertion = Command::cargo_bin("pecu")
        .expect("built")
        .env("PECU_HOME", &funded)
        .args([
            "currency",
            "launch",
            "pecunft1@",
            "--from",
            "faucet",
            "--nft",
            "--yes",
        ])
        .assert()
        .failure();
    let stderr = String::from_utf8_lossy(&assertion.get_output().stderr).to_string();
    if stderr.contains("already") {
        eprintln!("the spare identity has defined its currency — skipping");
        return;
    }
    assertion
        .stderr(contains("more than one plausible cause"))
        .stderr(contains("switched off chain-wide"))
        .stderr(contains("EVAL_IDENTITY_RECOVER"))
        .stderr(contains("check before resending").or(contains("may already be propagating")));
}

/// The one satoshi is what makes it non-fungible, and it has to survive all the
/// way to the panel. Offline: the refusal below comes from the chain, but the
/// supply is decided before a node is reached.
#[test]
#[ignore = "talks to api.verustest.net"]
fn an_nft_carries_exactly_one_satoshi_of_supply() {
    let Ok(funded) = std::env::var("PECU_FUNDED_HOME") else {
        eprintln!("PECU_FUNDED_HOME is not set — skipping");
        return;
    };
    let assertion = Command::cargo_bin("pecu")
        .expect("built")
        .env("PECU_HOME", &funded)
        .args([
            "currency",
            "launch",
            "pecunft1@",
            "--from",
            "faucet",
            "--nft",
            "--dry-run",
        ])
        .assert();
    let stdout = String::from_utf8_lossy(&assertion.get_output().stdout).to_string();
    if !assertion.get_output().status.success() {
        eprintln!("the spare identity is no longer launchable — skipping");
        return;
    }
    assert!(
        stdout.contains("0.00000001"),
        "the supply is one satoshi:\n{stdout}"
    );
    // Charged the parent's idimportfees, not its currencyregistrationfee.
    assert!(
        stdout.contains("0.02000000"),
        "an NFT costs 0.02, not 200:\n{stdout}"
    );
}

/// A currency defined to govern sub-identity registration is *about* its fee
/// policy, and `show` is the command for "what is this". Leaving the policy off
/// the panel meant the one question it exists to answer went unanswered.
#[test]
#[ignore = "talks to api.verustest.net"]
fn a_currency_that_governs_registrations_shows_what_they_cost() {
    let assertion = Command::cargo_bin("pecu")
        .expect("built")
        .env("PECU_HOME", home().path())
        .args(["currency", "show", "pecurefcur1@"])
        .assert()
        .success();
    let stdout = String::from_utf8_lossy(&assertion.get_output().stdout).to_string();
    for expected in [
        "SUB-IDENTITIES",
        "registration",
        "25.00000000",
        "3 levels",
        "and one is mandatory",
        "0.02000000",
    ] {
        assert!(stdout.contains(expected), "missing {expected}:\n{stdout}");
    }
}

/// The daemon renders money as a JSON float. Printing it back verbatim gives
/// `1000000.0` and `1e-08`, which are the same number in two shapes and neither
/// one lines up in a column. Everything money goes through the same eight
/// places, whatever the node called it.
#[test]
#[ignore = "talks to api.verustest.net"]
fn a_preallocation_is_shown_at_eight_places_not_as_a_json_float() {
    let assertion = Command::cargo_bin("pecu")
        .expect("built")
        .env("PECU_HOME", home().path())
        .args(["currency", "show", "pecuref9@"])
        .assert()
        .success();
    let stdout = String::from_utf8_lossy(&assertion.get_output().stdout).to_string();
    assert!(
        stdout.contains("500000.00000000"),
        "the preallocation is fixed at eight places:\n{stdout}"
    );
    assert!(
        !stdout.contains("500000.0 "),
        "the raw JSON float must not reach the panel:\n{stdout}"
    );
}

// ---------------------------------------------------------------- mint

#[test]
fn mainnet_will_not_mint_without_being_told_to() {
    let home = home();
    generate(&home, "demo");
    pecu(&home)
        .args([
            "currency",
            "mint",
            "tok@",
            "--to",
            "RXyz",
            "--amount",
            "1",
            "--profile",
            "mainnet",
        ])
        .assert()
        .failure()
        .stderr(contains("not allowed to spend"));
}

/// The recipient is checked before a node is reached, so this needs no network:
/// a typo should not cost a round trip, and it certainly should not cost a
/// passphrase prompt.
#[test]
fn a_mint_will_not_pay_a_verusid() {
    let home = home();
    generate(&home, "demo");
    pecu(&home)
        .args([
            "currency", "mint", "tok@", "--to", "alice@", "--amount", "1", "--node", DEAD_NODE,
        ])
        .assert()
        .failure()
        .stderr(contains("transparent address"))
        // The reason, not just the rule: an i-address here pays the R-address
        // with the same hash, which nobody holds a key to. Short phrase —
        // miette wraps the help text and a longer one straddles a line break.
        .stderr(contains("same hash"));
}

#[test]
fn a_mint_of_nothing_is_refused() {
    let home = home();
    generate(&home, "demo");
    pecu(&home)
        .args([
            "currency",
            "mint",
            "tok@",
            "--to",
            "RComfCn4wHHsGR8vWBAU7T1r3tHHyxN9Hm",
            "--amount",
            "0",
            "--node",
            DEAD_NODE,
        ])
        .assert()
        .failure()
        .stderr(contains("minting nothing"));
}

/// A fixed supply is the property a holder can verify, and the refusal should
/// say that rather than reporting a permission error.
#[test]
#[ignore = "talks to api.verustest.net"]
fn a_decentralized_currency_will_not_mint() {
    let Ok(funded) = std::env::var("PECU_FUNDED_HOME") else {
        eprintln!("PECU_FUNDED_HOME is not set — skipping");
        return;
    };
    Command::cargo_bin("pecu")
        .expect("built")
        .env("PECU_HOME", &funded)
        .args([
            "currency",
            "mint",
            "pecudepth3@",
            "--to",
            "RComfCn4wHHsGR8vWBAU7T1r3tHHyxN9Hm",
            "--amount",
            "100",
            "--from",
            "faucet",
        ])
        .assert()
        .failure()
        .stderr(contains("decentralized"))
        .stderr(contains("supply is fixed"));
}

/// A basket has no issuer: its supply moves by conversion.
#[test]
#[ignore = "talks to api.verustest.net"]
fn a_fractional_basket_will_not_mint() {
    let Ok(funded) = std::env::var("PECU_FUNDED_HOME") else {
        eprintln!("PECU_FUNDED_HOME is not set — skipping");
        return;
    };
    Command::cargo_bin("pecu")
        .expect("built")
        .env("PECU_HOME", &funded)
        .args([
            "currency",
            "mint",
            "pecudepth2@",
            "--to",
            "RComfCn4wHHsGR8vWBAU7T1r3tHHyxN9Hm",
            "--amount",
            "100",
            "--from",
            "faucet",
        ])
        .assert()
        .failure()
        .stderr(contains("not minted"));
}

/// The one that actually catches people. A mint is authorised by what the
/// *identity* spends, so a well-funded signing key is not enough — and the
/// message has to name the fix rather than report an empty balance.
#[test]
#[ignore = "talks to api.verustest.net"]
fn a_mint_is_paid_by_the_identity_not_the_signing_key() {
    let Ok(funded) = std::env::var("PECU_FUNDED_HOME") else {
        eprintln!("PECU_FUNDED_HOME is not set — skipping");
        return;
    };
    let assertion = Command::cargo_bin("pecu")
        .expect("built")
        .env("PECU_HOME", &funded)
        .args([
            "currency",
            "mint",
            "pecurefcur1@",
            "--to",
            "RComfCn4wHHsGR8vWBAU7T1r3tHHyxN9Hm",
            "--amount",
            "500",
            "--from",
            "faucet",
            "--dry-run",
        ])
        .assert();
    // Skip only on the one state that legitimately changes this — the identity
    // having been funded since. Any *other* failure is a real one: an earlier
    // version of this skipped whenever the message did not match, which made a
    // missing passphrase look like a pass.
    if assertion.get_output().status.success() {
        eprintln!("pecurefcur1@ has been funded since — skipping");
        return;
    }
    assertion
        .failure()
        .stderr(contains("paid for by the identity"))
        // Short phrases: miette wraps the help text, and the full command line
        // straddles a line break.
        .stderr(contains("pecu send"))
        .stderr(contains("not by the signing key"));
}

/// After a mint, the launch preallocation is the wrong answer to "how much of
/// this exists". Both figures are shown, because the difference is the whole
/// point of a centralized currency.
#[test]
#[ignore = "talks to api.verustest.net"]
fn a_minted_currency_shows_its_live_supply_next_to_the_launch_one() {
    let assertion = Command::cargo_bin("pecu")
        .expect("built")
        .env("PECU_HOME", home().path())
        .args(["currency", "show", "pecuref9@"])
        .assert()
        .success();
    let stdout = String::from_utf8_lossy(&assertion.get_output().stdout).to_string();
    assert!(
        stdout.contains("501000.00000000"),
        "the live supply reflects the mint:\n{stdout}"
    );
    assert!(
        stdout.contains("at launch"),
        "and says what it was before:\n{stdout}"
    );
}

/// A currency nobody has minted shows one figure, not two saying the same thing.
#[test]
#[ignore = "talks to api.verustest.net"]
fn an_unminted_currency_does_not_repeat_its_supply() {
    let assertion = Command::cargo_bin("pecu")
        .expect("built")
        .env("PECU_HOME", home().path())
        .args(["currency", "show", "pecudepth3@"])
        .assert()
        .success();
    let stdout = String::from_utf8_lossy(&assertion.get_output().stdout).to_string();
    // The row label, not merely the word: `control` reads "supply is fixed by
    // the definition" and matching that instead is how this first went wrong.
    let supply = stdout
        .lines()
        .find(|line| line.trim_start().starts_with("supply"))
        .unwrap_or_default();
    assert!(supply.contains("1000000.00000000"), "{stdout}");
    assert!(
        !supply.contains("at launch"),
        "nothing has changed, so there is nothing to compare against:\n{supply}"
    );
}

// ---------------------------------------------------------- preconvert

#[test]
fn mainnet_will_not_preconvert_without_being_told_to() {
    let home = home();
    generate(&home, "demo");
    pecu(&home)
        .args([
            "currency",
            "preconvert",
            "tok@",
            "--amount",
            "1",
            "--profile",
            "mainnet",
        ])
        .assert()
        .failure()
        .stderr(contains("not allowed to spend"));
}

#[test]
fn preconverting_nothing_is_refused() {
    let home = home();
    generate(&home, "demo");
    pecu(&home)
        .args([
            "currency",
            "preconvert",
            "tok@",
            "--amount",
            "0",
            "--node",
            DEAD_NODE,
        ])
        .assert()
        .failure()
        .stderr(contains("converting nothing"));
}

#[test]
fn a_preconversion_will_not_pay_a_verusid() {
    let home = home();
    generate(&home, "demo");
    pecu(&home)
        .args([
            "currency",
            "preconvert",
            "tok@",
            "--amount",
            "1",
            "--to",
            "alice@",
            "--node",
            DEAD_NODE,
        ])
        .assert()
        .failure()
        .stderr(contains("transparent address"))
        .stderr(contains("same hash"));
}

/// The rule that decides which of the two commands is legal at a given height.
/// Answered locally, with the block number, rather than letting the chain
/// refund a transfer days later.
#[test]
#[ignore = "talks to api.verustest.net"]
fn preconverting_into_a_launched_currency_names_the_block_it_missed() {
    let Ok(funded) = std::env::var("PECU_FUNDED_HOME") else {
        eprintln!("PECU_FUNDED_HOME is not set — skipping");
        return;
    };
    let assertion = Command::cargo_bin("pecu")
        .expect("built")
        .env("PECU_HOME", &funded)
        .args([
            "currency",
            "preconvert",
            "pecudepth2@",
            "--amount",
            "1",
            "--from",
            "faucet",
        ])
        .assert()
        .failure();
    let stderr = flat(&String::from_utf8_lossy(&assertion.get_output().stderr));
    assert!(stderr.contains("already_launched"), "{stderr}");
    assert!(stderr.contains("launched at block"), "{stderr}");
    // And says what to reach for instead.
    assert!(
        stderr.contains("ordinary conversion is the thing that works"),
        "{stderr}"
    );
}

/// Consensus refunds a preconversion paid in something the currency is not
/// backed by, rather than refusing it — so the mistake costs a wait unless it
/// is caught here.
#[test]
#[ignore = "talks to api.verustest.net"]
fn preconverting_in_a_currency_that_is_not_a_reserve_is_refused_locally() {
    let Ok(funded) = std::env::var("PECU_FUNDED_HOME") else {
        eprintln!("PECU_FUNDED_HOME is not set — skipping");
        return;
    };
    let assertion = Command::cargo_bin("pecu")
        .expect("built")
        .env("PECU_HOME", &funded)
        .args([
            "currency",
            "preconvert",
            &test_basket(),
            "--amount",
            "1",
            "--spend",
            "pecuref9@",
            "--from",
            "faucet",
        ])
        .assert()
        .failure();
    let stderr = String::from_utf8_lossy(&assertion.get_output().stderr).to_string();
    if stderr.contains("nothing on this chain") || stderr.contains("already_launched") {
        eprintln!("the test basket is not pre-launch right now — skipping");
        return;
    }
    assertion.stderr(contains("not one of"));
}

/// The launch that cost a real basket. Refused now, before anything is spent,
/// because the definition is permanent the moment it lands and this failure
/// only shows up hours later at the start block.
#[test]
#[ignore = "talks to api.verustest.net"]
fn a_basket_capping_only_some_reserves_is_refused_at_launch() {
    let Ok(funded) = std::env::var("PECU_FUNDED_HOME") else {
        eprintln!("PECU_FUNDED_HOME is not set — skipping");
        return;
    };
    let assertion = Command::cargo_bin("pecu")
        .expect("built")
        .env("PECU_HOME", &funded)
        .args([
            "currency",
            "launch",
            &spare_identity(),
            "--from",
            "faucet",
            "--supply",
            "1000000",
            "--reserve",
            "VRSCTEST:50",
            "--reserve",
            "TST:50",
            // Names one of two. The other is capped at zero, which accepts
            // nothing, which dooms the launch.
            "--max-preconvert",
            "VRSCTEST:1000",
            "--dry-run",
        ])
        .assert()
        .failure();
    let stderr = flat(&String::from_utf8_lossy(&assertion.get_output().stderr));
    if slot_is_spent(&stderr) {
        eprintln!("spare identity is spent — skipping");
        return;
    }
    assert!(stderr.contains("reserve_capped_at_zero"), "{stderr}");
    assert!(stderr.contains("caps it at zero"), "{stderr}");
    assert!(
        stderr.contains("refunds the entire launch unless every reserve"),
        "{stderr}"
    );
}

/// Naming none must stay allowed — an empty vector is never consulted. Getting
/// this backwards would refuse every uncapped basket, which is most of them.
#[test]
#[ignore = "talks to api.verustest.net"]
fn a_basket_capping_no_reserves_at_all_is_allowed() {
    let Ok(funded) = std::env::var("PECU_FUNDED_HOME") else {
        eprintln!("PECU_FUNDED_HOME is not set — skipping");
        return;
    };
    let assertion = Command::cargo_bin("pecu")
        .expect("built")
        .env("PECU_HOME", &funded)
        .args([
            "currency",
            "launch",
            &spare_identity(),
            "--from",
            "faucet",
            "--supply",
            "1000000",
            "--reserve",
            "VRSCTEST:50",
            "--reserve",
            "TST:50",
            "--dry-run",
        ])
        .assert();
    let stderr = flat(&String::from_utf8_lossy(&assertion.get_output().stderr));
    if slot_is_spent(&stderr) {
        eprintln!("spare identity is spent — skipping");
        return;
    }
    assert!(
        !stderr.contains("reserve_capped_at_zero"),
        "an uncapped basket must not be refused:\n{stderr}"
    );
}

// ------------------------------------------------------------- convert

#[test]
fn mainnet_will_not_convert_without_being_told_to() {
    let home = home();
    generate(&home, "demo");
    pecu(&home)
        .args([
            "currency",
            "convert",
            "tok@",
            "--amount",
            "1",
            "--profile",
            "mainnet",
        ])
        .assert()
        .failure()
        .stderr(contains("not allowed to spend"));
}

#[test]
fn converting_nothing_is_refused() {
    let home = home();
    generate(&home, "demo");
    pecu(&home)
        .args([
            "currency", "convert", "tok@", "--amount", "0", "--node", DEAD_NODE,
        ])
        .assert()
        .failure()
        .stderr(contains("converting nothing"));
}

#[test]
fn a_conversion_will_not_pay_a_verusid() {
    let home = home();
    generate(&home, "demo");
    pecu(&home)
        .args([
            "currency", "convert", "tok@", "--amount", "1", "--to", "alice@", "--node", DEAD_NODE,
        ])
        .assert()
        .failure()
        .stderr(contains("transparent address"));
}

/// A reserve into the basket that holds it — the commonest shape, and the one
/// that proves the estimate is the node's rather than invented here.
#[test]
#[ignore = "talks to api.verustest.net"]
fn converting_a_reserve_into_a_basket_shows_the_nodes_estimate() {
    let Ok(funded) = std::env::var("PECU_FUNDED_HOME") else {
        eprintln!("PECU_FUNDED_HOME is not set — skipping");
        return;
    };
    let assertion = Command::cargo_bin("pecu")
        .expect("built")
        .env("PECU_HOME", &funded)
        .args([
            "currency",
            "convert",
            "triccrypto2",
            "--amount",
            "1",
            "--from",
            "faucet",
            "--dry-run",
            "--json",
        ])
        .assert()
        .success();
    let document: serde_json::Value =
        serde_json::from_slice(&assertion.get_output().stdout).expect("valid json");
    assert_eq!(document["broadcast"], false);
    assert_eq!(document["spend"], "VRSCTEST");
    assert_eq!(document["into"], "triccrypto2");
    // Not pinned to a figure: a basket's price moves with every conversion that
    // lands. That it is positive and came from the node is the property.
    assert!(
        document["estimated_out"].as_u64().unwrap_or(0) > 0,
        "{document}"
    );
}

/// One reserve into another, priced by a basket that holds both.
#[test]
#[ignore = "talks to api.verustest.net"]
fn converting_between_two_reserves_routes_through_the_basket() {
    let Ok(funded) = std::env::var("PECU_FUNDED_HOME") else {
        eprintln!("PECU_FUNDED_HOME is not set — skipping");
        return;
    };
    let assertion = Command::cargo_bin("pecu")
        .expect("built")
        .env("PECU_HOME", &funded)
        .args([
            "currency",
            "convert",
            "SPORTS",
            "--amount",
            "1",
            "--via",
            "bankroll",
            "--from",
            "faucet",
            "--dry-run",
        ])
        .assert()
        .success();
    let stdout = flat(&String::from_utf8_lossy(&assertion.get_output().stdout));
    assert!(stdout.contains("through"), "{stdout}");
    assert!(stdout.contains("priced by the basket"), "{stdout}");
}

/// The mirror of the preconvert check. Before launch there is nothing to price
/// against, so the other command is the one that works.
#[test]
#[ignore = "talks to api.verustest.net"]
fn converting_through_a_basket_that_has_not_launched_points_at_preconvert() {
    let Ok(funded) = std::env::var("PECU_FUNDED_HOME") else {
        eprintln!("PECU_FUNDED_HOME is not set — skipping");
        return;
    };
    let assertion = Command::cargo_bin("pecu")
        .expect("built")
        .env("PECU_HOME", &funded)
        .args([
            "currency",
            "convert",
            &test_basket(),
            "--amount",
            "1",
            "--from",
            "faucet",
            "--dry-run",
        ])
        .assert();
    let stderr = flat(&String::from_utf8_lossy(&assertion.get_output().stderr));
    if !stderr.contains("not_launched_yet") {
        eprintln!("the test basket has launched since — skipping");
        return;
    }
    assert!(stderr.contains("pecu currency preconvert"), "{stderr}");
}

/// A refunded launch leaves a definition that reads live and holds nothing.
/// Without this the only signal is an estimate of zero.
#[test]
#[ignore = "talks to api.verustest.net"]
fn converting_through_a_refunded_basket_says_so() {
    let Ok(funded) = std::env::var("PECU_FUNDED_HOME") else {
        eprintln!("PECU_FUNDED_HOME is not set — skipping");
        return;
    };
    Command::cargo_bin("pecu")
        .expect("built")
        .env("PECU_HOME", &funded)
        .args([
            "currency",
            "convert",
            "pecudepth2@",
            "--amount",
            "1",
            "--from",
            "faucet",
            "--dry-run",
        ])
        .assert()
        .failure()
        .stderr(contains("launch_refunded"));
}

/// A conversion needs a basket somewhere, and saying which three shapes exist
/// beats reporting that the chain refused.
#[test]
#[ignore = "talks to api.verustest.net"]
fn converting_between_two_plain_tokens_explains_what_is_possible() {
    let Ok(funded) = std::env::var("PECU_FUNDED_HOME") else {
        eprintln!("PECU_FUNDED_HOME is not set — skipping");
        return;
    };
    let assertion = Command::cargo_bin("pecu")
        .expect("built")
        .env("PECU_HOME", &funded)
        .args([
            "currency",
            "convert",
            "pecuref9@",
            "--amount",
            "1",
            "--spend",
            "pecudepth3@",
            "--from",
            "faucet",
            "--dry-run",
        ])
        .assert()
        .failure();
    let stderr = flat(&String::from_utf8_lossy(&assertion.get_output().stderr));
    assert!(stderr.contains("neither_is_a_basket"), "{stderr}");
    assert!(stderr.contains("--via"), "{stderr}");
}

/// The floor is checked before signing and never again. A floor nothing can
/// meet must refuse locally, and say that the price moves on its own.
#[test]
#[ignore = "talks to api.verustest.net"]
fn a_floor_the_estimate_cannot_meet_refuses_before_signing() {
    let Ok(funded) = std::env::var("PECU_FUNDED_HOME") else {
        eprintln!("PECU_FUNDED_HOME is not set — skipping");
        return;
    };
    let assertion = Command::cargo_bin("pecu")
        .expect("built")
        .env("PECU_HOME", &funded)
        .args([
            "currency",
            "convert",
            "triccrypto2",
            "--amount",
            "1",
            "--min-out",
            "99999",
            "--from",
            "faucet",
            "--dry-run",
        ])
        .assert()
        .failure();
    let stderr = flat(&String::from_utf8_lossy(&assertion.get_output().stderr));
    assert!(stderr.contains("the node expects"), "{stderr}");
    // Not the preconvert wording, which an earlier version wrongly reused here.
    assert!(!stderr.contains("start block"), "{stderr}");
    assert!(stderr.contains("lower the floor"), "{stderr}");
}

/// A missing identity is a plausible typo, and registering one burns 100
/// VRSCTEST. So it stays refused without the flag — and the refusal names the
/// flag rather than blaming the node, which is what it used to do.
#[test]
#[ignore = "talks to api.verustest.net"]
fn a_missing_identity_names_the_register_flag_rather_than_the_node() {
    let Ok(funded) = std::env::var("PECU_FUNDED_HOME") else {
        eprintln!("PECU_FUNDED_HOME is not set — skipping");
        return;
    };
    let assertion = Command::cargo_bin("pecu")
        .expect("built")
        .env("PECU_HOME", &funded)
        .args([
            "currency",
            "launch",
            "pecu-no-such-identity-9x@",
            "--from",
            "faucet",
            "--supply",
            "100",
            "--dry-run",
        ])
        .assert()
        .failure();
    let stderr = flat(&String::from_utf8_lossy(&assertion.get_output().stderr));
    assert!(stderr.contains("not on this chain yet"), "{stderr}");
    assert!(stderr.contains("--register"), "{stderr}");
    // The node answered correctly; blaming it sends the reader the wrong way.
    assert!(!stderr.contains("pecu doctor"), "{stderr}");
}

/// The refusal costs nothing to hit but twenty minutes to discover the hard
/// way, so it is on the flag rather than only in the error.
#[test]
fn launch_help_says_a_dry_run_will_not_register() {
    let home = home();
    generate(&home, "demo");
    let assertion = pecu(&home)
        .args(["currency", "launch", "--help"])
        .assert()
        .success();
    // clap wraps this help to the terminal width, and it honours COLUMNS even
    // when stdout is captured — at 80 columns the wrap lands between "dry" and
    // "run". Flatten first so the phrase means what it says.
    let stdout = flat(&String::from_utf8_lossy(&assertion.get_output().stdout));
    assert!(stdout.contains("--register"), "{stdout}");
    assert!(stdout.contains("dry run"), "{stdout}");
    assert!(stdout.contains("stops there"), "{stdout}");
}

#[test]
fn launch_advertises_the_register_flag() {
    let home = home();
    generate(&home, "demo");
    pecu(&home)
        .args(["currency", "launch", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("--register"))
        .stdout(predicate::str::contains("--register-timeout"));
}
