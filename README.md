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
| `pecu wallet balance\|utxos` | Spendable, withheld and token balances | ✅ done |
| `pecu tx explain` | Says what every output in a transaction actually *is* | ✅ done |
| `pecu send` | Transparent sends, native and token | ✅ done |
| `pecu plan send` / `pecu sign` / `pecu broadcast` | The air-gap trio, over files or QR codes | ✅ done |
| `pecu id show\|register` | Read an identity; register one (two-phase, resumable) | ✅ done |
| `pecu id update\|revoke\|recover` | The rest of the lifecycle | M7b |
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

### `pecu tx explain`

Says what every output in a transaction actually *is*. Offline for hex; a txid is
the one input that needs a node, and only to fetch the bytes.

```sh
pecu tx explain <txid>                    # fetches the hex, decodes it locally
pecu tx explain <raw hex>                 # offline
pecu tx explain <output script hex>       # offline, a bare scriptPubKey
cat tx.hex | pecu tx explain -
```

```
┌─ TRANSACTION ──────────────────────────────────────────────────────────────┐
│ txid     df69640e4cfafe7cbe9cabd3c790ed3c556f7ee340e5f10ce73dd1b590f0556d  │
│ expiry   height 1,167,853                                                  │
├─ INPUTS ───────────────────────────────────────────────────────────────────┤
│ #0 e740a3149f…600f15:0                                                     │
│ #1 ec69f05ffd…728670:0                                                     │
├─ OUTPUTS ──────────────────────────────────────────────────────────────────┤
│ 7 outputs — 105.00000000 in native satoshis                                │
│ #0 0.00000000                                                              │
│      the VerusID verusrpc-test-mrhu3gpo3wws@ — 1-of-1, revocation          │
│      iEiEX5Voi…nAyd, recovery iEiEX5Voi…nAyd                               │
│ #2 0.00000000                                                              │
│      a CryptoCondition this SDK does not decode (eval 13) — ▲ IT MAY HOLD  │
│      CURRENCY; do not treat this output as empty                           │
│ #5 100.00000000                                                            │
│      reserves held for i9G2QgG74f7tErEyF3cWp2x1exBGbFa19t: 100.00000000    │
│      iJhCezBEx…f2yq                                                        │
│ #6 5.00000000                                                              │
│      → i9G2QgG74f7tErEyF3cWp2x1exBGbFa19t held for a VerusID, not a key    │
└────────────────────────────────────────────────────────────────────────────┘
```

**Why an output needs decoding at all.** On Bitcoin an output is a script and a
number of satoshis, and the number is the value. On Verus that is true only for
the plain ones. A token lives in the *payload* of a CryptoCondition output whose
satoshi field is zero; an identity is an output; a conversion in flight is an
output; a name commitment is an output. Reading the satoshi column of a Verus
transaction and calling it the value is how a wallet reports that an address
holds nothing while it holds a fortune in tokens.

So this says what each output *is*, and where it cannot tell, it says so —
including whether the thing it cannot read is **able** to hold money. That last
distinction is the one worth having: an undecodable output that provably cannot
carry currency is safe to ignore, and one that can is not.

An output that fails to decode does not fail the transaction. It sits beside the
ones that decoded fine, marked, because refusing the whole thing would throw away
the answer you came for.

### `pecu wallet`

Read-only, and deliberately so: it takes an address, not a key. Watching a
balance is the one wallet job that needs no secret, and a command that cannot
unlock anything cannot spend anything.

```sh
pecu wallet balance --address iJhCezBExJHvtyH3fGhNnt2NhU4Ztkf2yq
pecu wallet balance --key demo    # the address of a stored key
pecu wallet balance               # the sole stored key, if there is exactly one
pecu wallet utxos --key demo
```

```
┌─ WALLET ───────────────────────────────────────────────┐
│ address   iJhCezBExJHvtyH3fGhNnt2NhU4Ztkf2yq           │
│ tip       ▸ 1,176,594                                  │
├────────────────────────────────────────────────────────┤
│ SPENDABLE          0.00000000  VRSCTEST  (0 outputs)   │
│ WITHHELD           0.00000000  VRSCTEST  (15 outputs)  │
│ HELD BY ID     20155.03513344  VRSCTEST  (11 outputs)  │
│ IN CONDITIONS   1159.18038198  VRSCTEST  (210 outputs) │
│ TOTAL          21314.21551542  VRSCTEST                │
├─ TOKENS ───────────────────────────────────────────────┤
│ 9272.49511041  (unnamed)  iHBwQo7LU…dK9f               │
└────────────────────────────────────────────────────────┘
```

That is a real VRSCTEST address, and it is the whole point: **nothing a key can
spend, 21,314 in native value, and 9272 in a token.** A Verus balance is not one
number, and a wallet that prints one is wrong.

| Row | What it is |
|---|---|
| `SPENDABLE` | Plain P2PKH outputs. What a transparent key can move right now |
| `WITHHELD` | Outputs the node reported as not spendable yet |
| `HELD BY ID` | Native value in pay-to-identity outputs — a VerusID's own funds, spendable by its authority rather than by a key |
| `IN CONDITIONS` | Native value in every other CryptoCondition output |
| `TOTAL` | The sum, which is what a block explorer shows |

`HELD BY ID` is where an i-address keeps everything it owns. The SDK deliberately
keeps those outputs out of the spendable bucket — the native builders would
destroy what they carry — but they are still the balance, and there is a test
that reconciles `TOTAL` against the node's own `getaddressbalance`, satoshi for
satoshi.

- **`WITHHELD`, not "immature".** Coinbase maturity is the usual cause, but the
  SDK routes *any* output the node reports as unspendable into that bucket. An
  output with a million confirmations labelled "immature" is a wrong answer
  printed confidently.
- **A failed token lookup means "unknown", never "none".** It is a separate call
  from the native figure so that one bucket being uncountable cannot take the
  other down with it, and `--json` says `"known": false` rather than an empty list.
- **Currency names are untrusted.** They come from the node and Verus permits
  more in a name than it looks like it does, so they are stripped of control
  characters, folded onto one line and capped before printing — and the currency
  **id** is always shown next to the name, because the id is the part that
  identifies anything.
- **Addresses are parsed before the node sees them.** A typo'd address comes back
  from a node as an empty balance, which reads as "no funds" — the one wrong
  answer a wallet must never give.
- **Several stored keys are refused, not guessed between.** Picking one silently
  would report the wrong address's balance.

A long-lived mining address can have a UTXO set far past the SDK's 8 MiB reply
ceiling. That is a memory bound against a hostile node, not a bug; raise it for a
profile with `max_response_mb` and the error says so.

### `pecu send`

```sh
pecu send --to bob@ --amount 0.1                     # a VerusID name resolves
pecu send --to RXyz…7Qa4 --amount 0.1 --from cold    # or an address
pecu send --to bob@ --amount 5 --currency pecu@      # a token
pecu send --to bob@ --amount 0.1 --dry-run           # build and sign, send nothing
```

The order is deliberate: unlock the key, **build and sign locally**, show you the
finished transaction decoded output by output, and only then offer to broadcast.
Nothing leaves the machine until you have seen what it says, and the last step
wants the word `yes` typed out rather than a keystroke.

```
┌─ REVIEW ──────────────────────────────────────────────────────┐
│ from     RComfCn4wHHsGR8vWBAU7T1r3tHHyxN9Hm (faucet)          │
│ to       bob@ (bob.VRSCTEST@)                                 │
│ amount   0.10000000 VRSCTEST                                  │
│ fee      0.00010000 VRSCTEST                                  │
│ change   9.89990000 VRSCTEST                                  │
│ txid     a3f1…                                                │
│ expiry   height 1,176,620                                     │
├─ OUTPUTS AS BUILT ────────────────────────────────────────────┤
│ #0 0.10000000 VRSCTEST                                        │
│      → iBob… held for a VerusID, not a key                    │
│ #1 9.89990000 VRSCTEST                                        │
│      → RComfCn4wHHsGR8vWBAU7T1r3tHHyxN9Hm                     │
└───────────────────────────────────────────────────────────────┘

  type `yes` to broadcast:
```

Those outputs are decoded from the **bytes that would go out**, not printed back
from the arguments — re-showing what you typed would confirm nothing. It is the
same decoder `pecu tx explain` uses, so `--dry-run` hands you hex you can read
straight back:

```sh
pecu send --to bob@ --amount 0.1 --dry-run --json | jq -r .hex | pecu tx explain -
```

**The dry run is enforced by the SDK's types, not by remembering.**
`flows::prepare_send` takes a `ChainReader` and no `Broadcaster`, so what it
returns is *incapable* of being sent; broadcasting is a separate, explicit step.

**Mainnet cannot spend until you say so.** `allow_spend` is `false` there by
default — see [Configuration](#configuration).

### The air gap

Three commands, because there are three machines' worth of trust.

```sh
# 1. where the node is. No key on this machine at all.
pecu plan send --address RComf…N9Hm --to RJ7gs…w5hp --amount 0.4 --qr-out plan.png

# 2. where the key is. This one opens no socket.
pecu sign --qr-in plan-1.png --key cold --qr-out signed.png

# 3. back where the node is. Carries no key.
pecu broadcast --qr-in signed-1.png
```

Files and pipes work just as well — `--out plan.hex`, then `pecu sign @plan.hex`,
then `pecu broadcast @signed.hex`. Everything accepts hex as an argument, `@file`,
or `-` for stdin.

```
┌─ PLAN ─────────────────────────────────────────────────────────┐
│ from           RComfCn4wHHsGR8vWBAU7T1r3tHHyxN9Hm              │
│ spending       449.74990000 VRSCTEST across 1 input            │
│ paying out     449.74980000 VRSCTEST                           │
│ fee and burn   0.00010000 VRSCTEST                             │
│ expiry         height 1,176,653                                │
│ commits        ✓ every input covers every output (SIGHASH_ALL) │
├─ OUTPUTS ──────────────────────────────────────────────────────┤
│ #0 0.30000000 VRSCTEST                                         │
│      → RJ7gsKDjUjPS8XZzENqmQMmWJRLuTnw5hp                      │
│ #1 449.44980000 VRSCTEST                                       │
│      → RComfCn4wHHsGR8vWBAU7T1r3tHHyxN9Hm                      │
└────────────────────────────────────────────────────────────────┘
```

**`sign` genuinely needs no network.** Not "does not usually use one" — there is
a test that signs a plan with `--node https://127.0.0.1:1` and succeeds. If that
ever stops being true, the suite fails.

**`commits` is the row to read.** Whoever planned the transaction chose the
outputs, and a signature is the irreversible step. Outputs are only binding on
your input if your input commits to them: under `SIGHASH_NONE` they are not
covered at all, and whoever holds the partial can redirect the money after you
sign. `sign` refuses without `--yes` when that check fails.

**A partial that still needs another signature is never dressed up as finished.**
It comes back as a partial, with a non-zero exit and instructions to pass it on.

#### QR framing

A QR code holds at most 4296 alphanumeric characters, so payloads are split into
numbered frames:

```
PECU1:2/5:A1B2C3…
```

They reassemble in any order and duplicates are ignored — a stack of photographs
is rarely tidy — but a *missing* frame is refused by number, because a payload
silently short by one is a transaction that fails at the daemon for no visible
reason. Hex is upper-cased so QR uses alphanumeric mode at 5.5 bits per character
rather than byte mode at 8.

`--qr` draws in the terminal; `--qr-out <stem>` writes `<stem>-1.png`, `-2.png`, …

### `pecu id`

```sh
pecu id show VRSCTEST@              # read any identity off the chain
pecu id register alice --from cold  # run it again to carry on where it left off
```

```
┌─ IDENTITY ───────────────────────────────────────────────┐
│ name         pecucli7.VRSCTEST@                          │
│ i-address    i7r29bDQfrwjkTxjv4bcYD6B1ZV7WZ4kGo          │
│ status       ✓ active                                    │
│ registered   block 1,176,650                             │
├─ CONTROL ────────────────────────────────────────────────┤
│ signatures   1-of-1                                      │
│ ▸ RComfCn4wHHsGR8vWBAU7T1r3tHHyxN9Hm                     │
│ revocation   i7r29bDQfrwjkTxjv4bcYD6B1ZV7WZ4kGo (itself) │
│ recovery     i7r29bDQfrwjkTxjv4bcYD6B1ZV7WZ4kGo (itself) │
└──────────────────────────────────────────────────────────┘
```

`(itself)` is the row worth reading. A freshly registered VerusID is **its own
revocation and recovery authority**, which makes it unrevokable and
unrecoverable — and the SDK is explicit that pointing those elsewhere is a
decision at registration time, not a later refinement. `RegistrationOptions` has
no field for it, so this build cannot offer the choice; it does refuse to let it
pass unmentioned, warning before you pay.

#### Registration is two transactions

Step one commits to the name under a salt. Step two reveals it and pays. Between
them sits a confirmation — and a salt that exists nowhere else. Lose it and the
name is unclaimable and the commitment fee is gone.

So the `Pending` is written to `<config>/pending/<name>.json` **before anything
is broadcast**, and re-running the same command picks it up:

```
┌─ WAITING ────────────────────────────────────────────────────────────────────┐
│ name            pecucli7@                                                    │
│ commitment      bc1e12add8d97582a0814ed79afa094c5cbb5d5ad165239baf265fe923c5 │
│                 a07d                                                         │
│ confirmations   0 (still in the mempool)                                     │
└──────────────────────────────────────────────────────────────────────────────┘
  ▸ run the same command again in a minute
```

The SDK makes the ordering hard to get wrong: `complete` exists only on
`Pending<ReadyToRegister>`, and the only way to hold one is a `poll` that saw the
commitment confirm. Running step two early is a **compile error**, not a spent
commitment.

The success message says *broadcast*, not *registered* — the identity does not
exist until the transaction is mined, and `id show` will say nothing is called
that until then.

### `--explain`

Any command takes it. It prints the `verus-sdk` calls that command actually
made, with the arguments it passed and a summary of what came back:

```
┌─ SDK CALLS ────────────────────────────────────────────────────────┐
│ verus_sdk::network::prepare_send(&node, &key, "iJhCe…", "0.1")     │
│   → Unsent<Sent> { txid: a3f1…, fee: 0.0001, change: 9.8999 }      │
│                                                                    │
│ unsent.broadcast(&node)                                            │
│   → Sent { txid: a3f1… }                                           │
└────────────────────────────────────────────────────────────────────┘
```

It prints on the failure path too, which is when it is most useful.

Note this is *not* a `tracing` layer. `verus-sdk` emits no spans, so the events
would have to be written at the call site regardless — and then a subscriber is
pure ceremony between a `debug!` and a `println!`. The cost of recording
explicitly is that it is only as accurate as the call sites keep it.

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

# Ceiling on a single RPC reply. A memory bound against a hostile or overloaded
# node, not a performance knob. 8 MiB covers any ordinary wallet; a long-lived
# mining address can need far more.
max_response_mb = 8
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
