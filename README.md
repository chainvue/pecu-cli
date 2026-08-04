# verus-pecu-cli

```
                             ┌─┐┌─┐┌─┐┬ ┬
                             ├─┘├┤ │  │ │
                             ┴  └─┘└─┘└─┘
              a Verus wallet that lives in your terminal
```

`pecu` is a command-line Verus wallet, and the flagship example app for the
[Verus Rust SDK](https://github.com/chainvue/verus-rust-sdk). It exists to show what
that SDK can do — keys, transparent sends, air-gapped signing, transaction decoding
and the full VerusID lifecycle — without a local `verusd`, without a wallet daemon,
and without ever handing a private key to anything.

It talks to the public **VRSCTEST** node at `https://api.verustest.net`. The node is
only ever asked questions and given finished transaction bytes.

Every command takes `--explain`, which prints the exact `verus-sdk` calls it made.
Reading the output is meant to teach you the SDK.

> **Status: early.** This is being built one milestone at a time. Commands that
> aren't implemented yet say so and exit non-zero — see the table below.

## Install

Requires Rust 1.95 (the toolchain the SDK pins).

```sh
git clone <this repo> && cd verus-pecu-cli
cargo build --release
./target/release/pecu --help
```

There is no `just` dependency; the task runner is `make`:

```sh
make help        # list targets
make check       # fmt-check + clippy -D warnings + tests + build, all offline
make test        # offline tests only
make testnet     # the #[ignore]d tests that hit api.verustest.net
make gallery     # eyeball the UI kit
make snapshots   # accept the current UI output as the new snapshots
make run ARGS="doctor"
```

## Commands

| Command | Does | Status |
|---|---|---|
| `pecu doctor` | Node reachability, chain tip, config paths, build info | ✅ done |
| `pecu key gen\|import\|list\|show\|export\|phrase` | Encrypted keystore (Argon2id + ChaCha20-Poly1305) | ✅ done |
| `pecu wallet balance\|utxos` | Spendable, immature and token balances | M4 |
| `pecu tx explain` | Says what every output in a transaction actually *is* | M4 |
| `pecu send` | Transparent sends, native and token | M5 |
| `pecu plan send` / `pecu sign` / `pecu broadcast` | The air-gap trio, over files or terminal QR codes | M6 |
| `pecu id show\|register\|update\|revoke\|recover` | VerusID lifecycle | M7 |
| `pecu id login\|publish\|read` | Sign-in with VerusID, and VDXF data | M8 |
| `pecu completions <shell>` | Shell completion script | ✅ done |

### `pecu doctor`

The first thing to run. It answers the three questions in the order they go
wrong: where are my files, what was this binary built from, and is the node
answering.

```
┌─ LOCAL ─────────────────────────────────────────────┐
│ profile     testnet                                 │
│ node        https://api.verustest.net               │
│ currency    VRSCTEST                                │
│ spending    ✓ spending allowed                      │
│ config      ~/.config/verus-pecu/config.toml        │
│ keys        ~/.config/verus-pecu/keys (0 keys)      │
├─ BUILD ─────────────────────────────────────────────┤
│ pecu        0.1.0                                   │
│ verus-sdk   ae08bc0                                 │
│ features    network                                 │
├─ NODE ──────────────────────────────────────────────┤
│ chain       VRSCTEST                                │
│ daemon      1.2.17-3                                │
│ tip         ▸ 1,176,514   mined 82s ago             │
│ sync        ✓ in sync                               │
│ mempool     0 transactions                          │
│ latency     232 ms                                  │
└─────────────────────────────────────────────────────┘
  ▸ no config file yet — running on the built-in profiles
```

It exits non-zero when the node cannot be reached, but still prints the local
half — "my setting is being ignored" and "the node is down" are different
problems, and the output should tell them apart. `pecu doctor --json` gives the
same report as machine-readable data, including when the node is down.

### `pecu key`

Keys live in an encrypted keystore: one file per key at
`~/.config/verus-pecu/keys/<label>.json`, mode `0600`.

```sh
pecu key gen --label demo                          # a random key
pecu key gen --label paper --from-phrase --show-phrase   # recoverable from paper
pecu key list
pecu key show demo
pecu key export demo --yes                         # prints the private key
pecu key phrase                                    # a phrase, stored nowhere
```

```
┌─ RECOVERY PHRASE ───────────────────────────────┐
│  1. pudding    7. caution  13. away   19. pizza │
│  2. elite      8. nest     14. level  20. use   │
│  3. nothing    9. crumble  15. spell  21. sauce │
│  4. rent      10. focus    16. pair   22. dwarf │
│  5. solution  11. action   17. first  23. nasty │
│  6. device    12. aim      18. try    24. camp  │
└─────────────────────────────────────────────────┘
  ▸ write this down, on paper, now — it is shown once and is not stored
```

**How it is protected.** Argon2id (19 MiB, 2 passes, 1 lane — the OWASP
interactive figure) derives a key from your passphrase; ChaCha20-Poly1305 seals
the 32 private key bytes under it. The envelope's metadata — version, label,
address, compression flag — is authenticated as associated data, so editing the
address in a key file produces a decryption failure rather than a key that
silently belongs to a different address than it claims. The KDF parameters
travel with each file, so raising the cost later never strands an old key.

That defends a stolen file against an offline guess. It does not defend a running
process: once unlocked, the key is in memory, held in `Zeroizing` wrappers and
wiped on drop, which narrows the window without closing it.

**Where the entropy comes from.** `verus-keys` deliberately offers no
`PrivateKey::generate` — where the bytes come from is the most security-critical
decision a wallet makes, and a library that picks quietly moves it somewhere
nobody reviews. So it is in `src/keystore.rs`, in the open, and it is the OS
CSPRNG via `getrandom`.

**Two key schedules, one phrase.** The same 24 words drive both sides of Verus by
different routes: the shielded side goes BIP-39 → seed → ZIP-32, and the
transparent side ignores BIP-39 entirely and hashes the phrase text *verbatim*.
`pecu key phrase` shows all three so the difference is visible. Because the text
is hashed verbatim, an imported phrase is never trimmed.

**Secrets never go on the command line.** A WIF or a phrase in `argv` lands in
your shell history and in the process list of every other user on the machine, so
`key import` reads from a no-echo prompt, or from stdin when it is piped:

```sh
pecu key export demo --yes --json | jq -r .wif | pecu key import --label copy
```

`PECU_PASSPHRASE` supplies the encryption passphrase for scripts and tests. It
deliberately does *not* supply the imported key — `key import` needs two
different secrets in one run, and one variable cannot provide both without
silently using the same value for each.

`key export` refuses to run without `--yes`, and says why first.

## Configuration

Everything resolves in one order: **command-line flag → environment → config
file → built-in default**.

There is no config file until you make one; the `testnet` and `mainnet` profiles
are built in. Files live under `$PECU_HOME`, else `$XDG_CONFIG_HOME/verus-pecu`,
else `~/.config/verus-pecu` — XDG rather than the platform convention, because
`~/.config` is where someone reaching for a terminal wallet will look.

```toml
# ~/.config/verus-pecu/config.toml
default_profile = "testnet"

[profiles.testnet]
node = "https://api.verustest.net"

# Built in, but shipped unable to spend: moving real coins from an example app
# should take a deliberate act, not a forgotten --profile.
[profiles.mainnet]
allow_spend = true
```

A profile that appears only in the file inherits testnet's defaults for whatever
it leaves out. Unknown keys are refused rather than ignored, so a typo is an
error instead of a setting that silently does nothing.

`$PECU_HOME` also makes the tests hermetic — they point it at a temporary
directory and cannot see, or damage, a real keystore.

### Global flags

| Flag | Meaning |
|---|---|
| `--json` | Machine-readable output instead of the rendered UI |
| `--dry-run` | Build and sign, but never broadcast |
| `--explain` | Print the `verus-sdk` calls this command makes |
| `-y`, `--yes` | Answer yes to every confirmation |
| `--profile <NAME>` | Config profile (env: `PECU_PROFILE`) |
| `--node <URL>` | Override the endpoint (env: `VERUS_ENDPOINT`) |
| `--theme auto\|phosphor\|plain` | Phosphor on a TTY, plain when piped. `NO_COLOR` always wins |
| `-v`, `-vv` | More logging |

## Why the output looks like that

The house style is *phosphor*: green on black, box-drawing frames, dim labels and
bright values. Panels shrink to fit their content rather than filling the window.

```
┌─ WALLET ───────────────────────────────────────────┐
│ addr   RXyz9k2mP…7Qa4                              │
│ tip    ▸ 3,481,207        node ✓ api.verustest.net │
├────────────────────────────────────────────────────┤
│ SPENDABLE  312.50000000  VRSCTEST  (4 utxos)       │
│ IMMATURE     6.00000000  VRSCTEST  (1 coinbase)    │
├─ TOKENS ───────────────────────────────────────────┤
│ 1200.00000000  pecu@    iJhCe4Ap7…y8Kd             │
│    0.50000000  bridge@  i3f7QwErT…V2Lm             │
└────────────────────────────────────────────────────┘
  ▸ 2 CryptoCondition outputs carry no currency
```

It is deliberately readable when piped. `--theme plain` drops the frames, the
colour and the box-drawing entirely; `--theme auto` (the default) picks plain
whenever stdout is not a terminal; `NO_COLOR` always wins; and `--json` will
bypass the renderer completely once commands have data to serialise.

```
$ pecu dev ui --theme plain
WALLET
  addr   RXyz9k2mP...7Qa4
  tip    - 3,481,207        node ok api.verustest.net

  SPENDABLE  312.50000000  VRSCTEST  (4 utxos)
  IMMATURE     6.00000000  VRSCTEST  (1 coinbase)
```

`pecu dev ui` renders every widget in the kit — it is both the design reference
and what the snapshot tests read, so a change to the renderer shows up as a diff
rather than as a surprise somewhere else. Run `make gallery` to see it in colour.

Nothing outside `src/ui/` emits an escape sequence or a box-drawing character.
Commands describe *what* a block contains; the renderer decides how it looks.

## Design notes

- **Keys never leave the machine.** The SDK is offline-first by construction: the
  transaction builders open no sockets. Signing happens locally; only finished bytes
  go to the node.
- **The SDK has no `generate()` on purpose** — the caller supplies entropy. That job
  belongs to the keystore, which is why `pecu key gen` exists rather than a library call.
- **Money is integer satoshis end to end.** No floats, anywhere.
- **The SDK dependency is pinned by commit.** It is pre-1.0 and not on crates.io, so
  "latest `main`" would not be a reproducible build. `pecu --version` prints the rev.

## Not in scope (yet)

Shielded/z-address operations, the currency launch wizard, marketplace offers, and a
live `watch` dashboard. They come after the transparent and identity paths are solid.

## License

Apache-2.0, matching the SDK.
