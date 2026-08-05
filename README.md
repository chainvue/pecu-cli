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

**Use the built binary for anything interactive.** `cargo run` re-checks the build
graph every time, and switching between `make check` and `cargo run` used to cost
a full rebuild — with the SDK in the dependency graph that is over a minute.
`make lint` now gives clippy its own target directory so it stops refingerprinting
everything the other commands built. Even so:

```sh
cargo build --release
./target/release/pecu id show VRSCTEST@     # ~0.2s, the same as curl to the node
```

Commands are as fast as the node answers. If one feels slow, time the binary
before blaming the command — `cargo run` is usually what you are measuring.

## Commands

| Command | Does | Status |
|---|---|---|
| `pecu doctor` | Node reachability, chain tip, config paths, build info | ✅ done |
| `pecu key gen\|import\|list\|show\|export\|phrase` | Encrypted keystore (Argon2id + ChaCha20-Poly1305) | ✅ done |
| `pecu wallet balance\|utxos\|history` | Spendable, withheld, token and unconfirmed balances; the transaction log | ✅ done |
| `pecu tx explain` | Says what every output in a transaction actually *is* | ✅ done |
| `pecu send` | Transparent sends: native, token, or out of a VerusID's own funds | ✅ done |
| `pecu plan send` / `pecu sign` / `pecu broadcast` | The air-gap trio, over files or QR codes | ✅ done |
| `pecu id show\|register` | Read an identity; register one (two-phase, resumable) | ✅ done |
| `pecu id update\|revoke\|recover` | The rest of the lifecycle | ✅ done |
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
│ verus-sdk   435491d                                 │
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
pecu wallet history --key demo --from-height 1176000
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

A sixth section appears only when something is moving. A UTXO set and a delta
list both agree that an unconfirmed payment does not exist, so an address that
has just been paid reports its old balance until a block is mined — the one
answer a wallet must not give while money is demonstrably on its way:

```
├─ PENDING ──────────────────────────────────────────────┤
│ in flight   1 transaction                              │
│ INCOMING  348.54919600  VRSCTEST                       │
│ OUTGOING  348.54929600  VRSCTEST                       │
│ NET        -0.00010000  VRSCTEST                       │
└────────────────────────────────────────────────────────┘
  ▸ pending: in this node's mempool, not in any block, and excluded from the
    totals above. It may confirm, be replaced, or never arrive, and another
    node may not have seen it at all
```

That is a real self-send caught mid-flight: everything leaves the address and
comes back, so the net is exactly the fee. It costs one request —
`ChainReader::address_mempool` — where the alternative is `mempool()` plus one
`raw_transaction()` per txid, scanning outputs.

- **It is never added to `TOTAL`.** Confirmed figures and mempool figures answer
  different questions, so they get different sections; a reader who adds them up
  should have to mean it.
- **A failed mempool read says so.** Like the token lookup, `Err` means
  *unknown*, and the panel prints `⚠ unknown: …` rather than nothing at all —
  silence here reads as "nothing pending", which is the wrong answer stated as a
  fact. `--json` carries `"known": false`.
- **`wallet utxos` marks the outputs a pending transaction already claims.** The
  chain still shows them unspent, so coin selection still offers them; funding a
  second payment from one builds a double spend the node refuses with
  `bad-txns-inputs-spent`. Unconfirmed arrivals appear there too, at `0`
  confirmations, and are never counted as spendable.

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

`pecu wallet history` is the other view: what happened, rather than what is
left.

```
┌─ HISTORY ──────────────────────────────────────────────────────┐
│ address   RComfCn4wHHsGR8vWBAU7T1r3tHHyxN9Hm                   │
│ found     19 transactions                                      │
├────────────────────────────────────────────────────────────────┤
│    HEIGHT        WHEN                CHANGE  TRANSACTION       │
│ 1,177,072  1h 44m ago  -0.00010000 VRSCTEST  97985193dc…242fe5 │
│ 1,177,079  1h 35m ago  -0.00010000 VRSCTEST  e0c5c6972e…beb532 │
│ 1,177,175  4m 40s ago  -1.00010000 VRSCTEST  ea47ee557a…b5b8b3 │
│ 1,177,178  3m 13s ago  +0.40000000 VRSCTEST  2e10e7944e…7a8bc5 │
└────────────────────────────────────────────────────────────────┘
```

- **Net per transaction, not gross.** An output spent and mostly returned as
  change counts as what actually moved, which is the number a reader wants.
- **A `+0.00000000` that still spent something is a transfer to yourself** — the
  value came back and only the fee left. The panel says so when it happens,
  because a zero that means "nothing happened" and a zero that means "you paid a
  fee" are different answers.
- **A token-only transfer moves no native value at all**, so the change column
  shows the token leg instead of a misleading `0`.
- **An open-ended `--from-height` is closed at the tip**, not at `u32::MAX` — the
  daemon refuses that with `-1: JSON integer out of range`, which reads as a
  broken node rather than as an argument it dislikes.
- `--limit` drops from the *front*: a terminal scrolls, so the most recent entry
  should be the one still on screen. When it truncates, it says how many it hid.

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

**Paying out of a VerusID's own funds** is the other half of the `HELD BY ID`
row in `wallet balance`. Money flows *into* an identity with an ordinary
`--to alice@`; getting it back out needs `--from-identity`, because the inputs
are pay-to-identity outputs and each carries a fulfillment rather than a
scriptSig:

```sh
pecu send --from-identity alice@ --to RXyz…7Qa4 --amount 1 --from alicekey
```

```
┌─ REVIEW ──────────────────────────────────────────────────────┐
│ from        pecucli7@ (the identity's own funds)              │
│ signed by   RComfCn4wHHsGR8vWBAU7T1r3tHHyxN9Hm (faucet)       │
│ to          RJ7gsKDjUjPS8XZzENqmQMmWJRLuTnw5hp                │
│ amount      0.10000000 VRSCTEST                               │
│ change      0.49980000 VRSCTEST                               │
├─ OUTPUTS AS BUILT ────────────────────────────────────────────┤
│ #0 0.10000000 VRSCTEST                                        │
│      → RJ7gsKDjUjPS8XZzENqmQMmWJRLuTnw5hp                     │
│ #1 0.49980000 VRSCTEST                                        │
│      → i7r29bDQ… held for a VerusID, not a key                │
└───────────────────────────────────────────────────────────────┘
```

The identity owns the money and the key proves the authority, so the panel names
both — `from` is the payer and `signed by` is the signer, and conflating them on
a spend-confirmation panel would misstate whose balance is about to drop. The
change goes back to the identity, not to the key. `--json` carries
`from_identity` for the same reason.

The SDK refuses ahead of time everything the chain would refuse later with a
message naming nothing: a revoked identity, a key the identity does not list, or
fewer distinct keys than its `minimumsignatures`. That last one currently means
`pecu send` cannot spend from a multi-signature identity at all — it signs with
one key — and the error says so rather than building something that dies at the
daemon.

**The dry run is enforced by the SDK's types, not by remembering.**
`flows::prepare_send` takes a `ChainReader` and no `Broadcaster`, so what it
returns is *incapable* of being sent; broadcasting is a separate, explicit step.

**Mainnet cannot spend until you say so.** `allow_spend` is `false` there by
default — see [Configuration](#configuration).

**`--json` is output, not consent.** It will not broadcast without `--yes`. The
confirmation prompt writes to the stream you are parsing and there is nobody to
answer it, so consent has to be passed in rather than assumed from the fact that
you asked for machine-readable output.

`send --json` prints **exactly one document**, on every path including the one
where the broadcast fails — that path is where the signed `hex` matters most,
since it is the only field that cannot be recovered afterwards. `broadcast` is a
tri-state, because a broadcast that did not come back is not the same as one that
was refused:

| `outcome` | `broadcast` | What it means |
|---|---|---|
| `not_broadcast` | `false` | A dry run. Built, signed, deliberately not sent |
| `accepted` | `true` | The node took it. `txid`, `fee` and `change` are the node's figures |
| `rejected` | `false` | The daemon read it and refused. It is in no mempool |
| `unknown` | `null` | The request did not complete. It **may** still have reached the mempool — check the `txid` before rebuilding |

`null` rather than `false` on that last row is the whole point: a timed-out
broadcast that is reported as "not sent" invites a second payment, and telling
someone their money is safe when it may already be moving is the wrong answer to
be confident about.

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
revocation and recovery authority**, so both roles answer to the same primary
keys listed above — there is no independent guardian to fall back on if those
keys are lost.

That is a default, not a life sentence: an identity update can repoint either
authority at another VerusID. `RegistrationOptions` has no field to set them at
registration, so this build cannot offer the choice up front, but it says what
the default means before you pay.

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

### `pecu id update` · `revoke` · `recover`

```sh
pecu id update i7r29bDQ… --recovery guardian@ --allow-authority-change
pecu id revoke i7r29bDQ… --from guardiankey
pecu id recover i7r29bDQ… --from guardiankey --primary RNew…  --min-sigs 1
```

**Who may change what.** The identity output's condition is `1-of-3`, and
consensus validates the three branches *independently*, each guarding its own
fields:

| changing | needs |
|---|---|
| `primary_addresses`, `min_sigs` | the primary condition |
| `revocation_authority` | the revocation condition |
| `recovery_authority` | the recovery condition |

A freshly registered identity is all three at once, so its own keys can point
either authority elsewhere. Once an authority names **another** identity, those
keys can no longer move it and cannot take it back. That direction has no undo.

**An identity that is its own recovery authority cannot be revoked.** This is a
consensus rule, not a policy — `identity.cpp` refuses a revocation nobody could
undo. The trigger is *recovery*, not revocation: an identity may revoke itself
perfectly well as long as somebody else can recover it. It is refused here
before a signature exists:

```
Error: pecu::flow_failed
  × building the revocation failed
  ╰─▶ recovery authority is the identity itself; revoking it would strand it
      permanently
  help: an identity that is its own recovery authority cannot be revoked:
        nobody could undo it. Point recovery at another VerusID first with
        `pecu id update --recovery <name@> --allow-authority-change`
```

**Every failure here is caught locally, because consensus will not explain
itself.** A revocation signed by the wrong authority comes back as
`-26: 16: mandatory-script-verify-flag-failed` — after the fee is spent, naming
neither which condition failed nor which authority was needed. So the flows read
the named authority, compare your keys against its primary addresses and
threshold, and refuse with both named. When the identity is still its own
authority that check is offline, decoded from the output script.

That pre-check is **advisory** whenever the authority is a different identity:
every fact in it comes from the node, so a lying node can fail a valid
revocation or pass an invalid one. It is a usability guard, not a security
boundary.

**Prefer the i-address for anything destructive.** Naming an identity by
i-address is verified against the decoded object with no node involved. A `name@`
can only be checked against what the node itself reported, which catches a node
inconsistent with itself but not one that lies consistently. The panel says which
one is in play.

**An update restates the whole identity**, so everything you do not name is
carried through — decoded from the output script consensus reads, not from a
rendering of it. Verified rather than asserted: the no-op update at
`00fecccd36f46e77a423de3f1027c31077a7452b3768dcf8cc65ae202eb5275c` came back with
its `contentmultimap` intact.

**`--allow-authority-change` is required for anything that touches control**, and
is checked before the passphrase prompt. Publishing addresses nobody holds, or a
threshold nobody can meet, is the one mistake with no remedy — not for the
holder, not for the recovery authority, not for anyone.

**Recovery without `--primary` brings the identity back under exactly the keys it
had when it was revoked** — including any that were compromised, which is usually
not what a recovery is for. The panel says so either way.

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
┌─ WALLET ────────────────────────────────────────────────┐
│ addr        RXyz9k2mP…7Qa4                              │
│ tip         ▸ 3,481,207        node ✓ api.verustest.net │
├─────────────────────────────────────────────────────────┤
│ SPENDABLE  312.50000000  VRSCTEST  (4 utxos)            │
│ WITHHELD     6.00000000  VRSCTEST  (1 coinbase)         │
├─ TOKENS ────────────────────────────────────────────────┤
│ 1200.00000000  pecu@    iJhCe4Ap7…y8Kd                  │
│    0.50000000  bridge@  i3f7QwErT…V2Lm                  │
├─ PENDING ───────────────────────────────────────────────┤
│ in flight   2 transactions                              │
│ INCOMING   0.25000000  VRSCTEST                         │
│ OUTGOING   1.00010000  VRSCTEST                         │
│ NET       -0.75010000  VRSCTEST                         │
└─────────────────────────────────────────────────────────┘
  ▸ pending: in this node's mempool, not in any block, and excluded from the totals above
```

It is deliberately readable when piped. `--theme plain` drops the frames, the
colour and the box-drawing entirely; `--theme auto` (the default) picks plain
whenever stdout is not a terminal; `NO_COLOR` always wins; and `--json` will
bypass the renderer completely once commands have data to serialise.

```
$ pecu dev ui --theme plain
WALLET
  addr        RXyz9k2mP...7Qa4
  tip         - 3,481,207        node ok api.verustest.net

  SPENDABLE  312.50000000  VRSCTEST  (4 utxos)
  WITHHELD     6.00000000  VRSCTEST  (1 coinbase)
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
