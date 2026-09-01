# CLAUDE.md

What an agent needs to know before changing anything in this repository. Read it
first; it is shorter than working it out from the source.

## What this is

`pecu` is a Verus wallet that lives in a terminal. It exists to demonstrate the
[Verus Rust SDK](https://github.com/chainvue/verus-rust-sdk), so "does this teach
the SDK correctly" is a real acceptance criterion here, not a nicety.

It holds private keys and signs transactions that move real money. There is no
staging environment for that.

## Commands

The `Makefile` is the source of truth. `just` is not installed on this project.

| Command | What it does |
| --- | --- |
| `make check` | fmt-check, clippy, tests, build — everything CI runs offline |
| `make fmt` | format the tree |
| `make fmt-check` | fail if unformatted |
| `make lint` | clippy, warnings are errors |
| `make test` | the offline suite; must pass with the network unplugged |
| `make build` | debug build |
| `make run ARGS="doctor"` | run the CLI |
| `make snapshots` | accept current UI output as the new insta snapshots |
| `python web/build.py` | build the docs site from `docs/*.md` |

**`make check` is the gate.** Run it before claiming anything works. If you
changed `docs/*.md` or anything under `web/`, run `python web/build.py` too — it
rejects a link to a `.md` file the site does not serve, and CI runs it.

There is no separate typecheck: `clippy --all-targets -D warnings` is it.

**Never run `make testnet` or `cargo test -- --ignored`.** Those tests talk to
`api.verustest.net` and some of them spend real VRSCTEST from a funded key. They
are `#[ignore]`d for that reason and CI does not run them either.

The toolchain is pinned to 1.95.0 in `rust-toolchain.toml`. Nothing else picks a
channel; do not add a `+toolchain` argument or a channel to a workflow.

## Architecture

One binary, `pecu`, built from `src/main.rs`. Roughly 24k lines of Rust plus 8k
lines of tests.

**`src/cli.rs`** — the command tree, organised the way the SDK is: keys, then
reading, then spending, then identities.

**`src/cmd/`** — one module per command group. `key` (make, import, inspect),
`wallet` (read-only balances and UTXOs), `tx` (decode), `send` (spend), `airgap`
(`plan send` / `sign` / `broadcast`), `id` (register, show), `lifecycle`
(update, revoke, recover), `currency`, `doctor`, `dev`. Anything not implemented
yet answers with `NotYet` rather than panicking, so `pecu --help` is an honest
map of the build.

**`src/config.rs`** — where files live and which chain we point at. Settings
resolve in exactly one order, everywhere: **flag → environment → config file →
built-in default**. Nothing else in the tree reads the environment. That is a
property of this module, not a convention each command remembers.

**`src/keystore.rs`** — one encrypted file per key under `<config>/keys/<label>.json`.
Argon2id derives a key from the passphrase, ChaCha20-Poly1305 seals the private
key bytes, and the envelope's metadata (version, label, address, compression) is
authenticated as associated data — so editing the address in the file produces a
decryption failure rather than a key that lies about which address it is.

**`src/node.rs`** — the single place a client is built, so timeout, response
ceiling and error wording are decided once. The SDK separates `ChainReader` from
`Broadcaster`; a command that must not spend takes `&impl ChainReader` and is
therefore *incapable* of spending. Preserve that — it is a type-level guarantee,
not a comment.

**`src/ui/`** — everything that decides how output looks. Commands build `Panel`s
and `Table`s and hand them to a `Ui`. **Nothing outside this module writes an
escape sequence or a box-drawing character.** That is what makes `--theme plain`,
`NO_COLOR` and `--json` one decision each.

**`src/failure.rs`** — the exit path, and the only place a JSON document reaches
stdout. It knows whether `--json` was asked for, prints the `pecu::…` code as a
field rather than buried in prose, and picks an exit code by kind of failure.

**`src/explain.rs`** — `--explain` records every call into `verus-sdk` at the
call site, with its arguments and a one-line summary of the result. Deliberately
not a `tracing` subscriber: the SDK emits no spans, so the events would have to
be written at the call site anyway.

**`src/payload.rs`** — hex in and out of the air-gap commands, from an argument,
a file, or stdin. This is the only untrusted input the program takes.

**`src/currency_name.rs`** — a name is display text; the currency *id* identifies
things. A failed name lookup is never allowed to render as a currency with no
name.

`verus-sdk` is a git dependency **pinned by rev**. Moving that pin changes every
signature this program produces and no test here can tell you it was safe. Do not
move it as part of another change.

## Code conventions

- Comments explain **why**, and usually name the concrete failure that motivated
  the code. A comment that restates the line below it is noise; read the comments
  around what you are changing before writing one. Module headers (`//!`) carry
  the design argument and are worth updating when the argument changes.
- Errors are `miette::Result` everywhere, with a `pecu::…` code. Nothing swallows
  a failure into a default.
- Every command that prints must work under `--json`, `--theme plain` and
  `NO_COLOR`.
- No `unwrap`/`expect` on anything that depends on input, the network, or the
  filesystem. Money arithmetic never goes through a float — the amounts exceed
  what an `f64` holds exactly.
- Match the surrounding style rather than importing a new one. Do not add a
  dependency as a side effect of another change.

## Test conventions

All tests are **integration tests** in `tests/*.rs` that drive the built binary
with `assert_cmd`. There are no unit tests to speak of, and that is deliberate:
what this tool promises is its output, so that is what is pinned.

The house pattern, from `tests/key.rs`:

```rust
use assert_cmd::Command;
use tempfile::TempDir;

const PASSPHRASE: &str = "correct horse battery staple";

fn home() -> TempDir {
    tempfile::tempdir().expect("a temp dir")
}

fn pecu(home: &TempDir) -> Command {
    let mut command = Command::cargo_bin("pecu").expect("the pecu binary should be built");
    command
        .env("PECU_HOME", home.path())      // never sees a real keystore
        .env("PECU_PASSPHRASE", PASSPHRASE) // stands in for the prompt
        .env_remove("NO_COLOR")
        .env_remove("PECU_THEME");
    command
}

fn json(command: &mut Command) -> serde_json::Value {
    let assertion = command.arg("--json").assert().success();
    let stdout = String::from_utf8_lossy(&assertion.get_output().stdout).into_owned();
    serde_json::from_str(&stdout).unwrap_or_else(|error| panic!("not json: {error}\n{stdout}"))
}

#[test]
fn the_key_file_holds_no_plaintext_key_material() {
    let home = home();
    generate(&home, "demo");
    let exported = json(pecu(&home).args(["key", "export", "demo", "--yes"]));
    let wif = exported["wif"].as_str().expect("a wif");

    let written = std::fs::read_to_string(home.path().join("keys/demo.json")).expect("readable");
    assert!(!written.contains(wif), "the WIF is sitting in the key file");
    assert!(written.contains("argon2id"), "no kdf recorded:\n{written}");
}
```

What that shows, and what a new test should copy:

- **Its own `PECU_HOME` in a `TempDir`.** No test can see or damage a real
  keystore, and tests do not interfere with each other.
- **Assert on `--json`, not on prose.** Wrap widths and help text are free to
  change; a JSON field is a contract.
- **Assert the specific fact,** with a message that shows the actual value when
  it fails. `.success()` on its own pins nothing.
- **Test names are sentences.** `the_key_file_holds_no_plaintext_key_material`,
  not `test_keystore_2`.
- Terminal rendering is pinned with `insta` snapshots (`tests/ui.rs`,
  `tests/tx.rs`, snapshots under `tests/snapshots/`).
- Network tests are `#[ignore]`d with a reason string saying what they need.
  Anything new that touches the network goes the same way.

## The rule about tests

**Existing tests may not be deleted, skipped, renamed away, weakened, or have
their assertions loosened in order to make a build green.** Not `#[ignore]`, not
a narrowed assertion, not a deleted case.

Accepting an insta snapshot counts as changing a test. `make snapshots` rewrites
the expected output wholesale, so read the diff line by line and accept it only
if every changed line is a change the specification asked for.

If an existing test genuinely contradicts what you were asked to build, that is
not yours to resolve: stop, say which test and which assertion, and let a human
decide. Adding tests is always allowed and is usually the answer.

This is enforced, not just requested. `scripts/check-test-tampering.sh` runs on
every pull request and a removed `#[test]`, an added `#[ignore]` or a deleted
test file forces `changes_requested` regardless of anything else in the review.

## The automated pipeline

Issues move through labels: `claude:ready` → spec gate → `claude:approved` →
implementation → pull request → adversarial review. See
[docs/claude-automation.md](docs/claude-automation.md) for the whole flow, and
`.github/claude-risk-paths.yml` for which paths are treated as high blast radius.
