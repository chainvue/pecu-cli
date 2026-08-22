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
| `pecu id update\|revoke\|recover\|unlock` | The rest of the lifecycle, including timelocks | ✅ done |
| `pecu id login\|publish\|read` | Sign-in with VerusID, and VDXF data | M8 |
| `pecu currency show\|launch\|mint\|preconvert\|convert` | Read a currency definition; launch a token or fractional basket; mint new supply of a centralized one; buy into a launching currency; convert through a launched one | ✅ done |
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
│ verus-sdk   ae279ea                                 │
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
pecu wallet balance --address bob@   # a VerusID name is resolved
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
  characters and of the invisible and direction-changing ones that let two
  different names print identically, folded onto one line and capped before
  printing — and the currency **id** is always shown next to the name, because
  the id is the part that identifies anything. On the `TOKENS` rows that id is
  re-encoded from the currency id the SDK parsed rather than reprinted from the
  node's text, and shortened to fit the column. `wallet history` has no such
  luxury: it falls back to the node's own key for a currency it has no name for,
  and that string is filtered the same way the name is.
- **Addresses are parsed before the node sees them.** A typo'd address comes back
  from a node as an empty balance, which reads as "no funds" — the one wrong
  answer a wallet must never give.
- **Several stored keys are refused, not guessed between.** Picking one silently
  would report the wrong address's balance.

**A VerusID name works wherever an address does**, and the panel says what it
resolved to — an i-address alone does not tell you whether `bob@` meant the
identity you had in mind:

```
│ address   iDxZS81ZCdqgdFVF6H1BfW43uov8ZUe222  (jbratchet.VRSCTEST@) │
```

The cost is that a typo'd *address* is no longer refused offline: anything that
does not parse as one is looked up as a name first. The refusal is the same, one
request later. What is **not** conflated is a missing identity and an
unreachable node — only the daemon's own `-5` and `-8` mean "that names
nothing", and anything else says the node failed rather than denying the
identity exists.

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

**A token send names the token.** With `--currency`, the `amount` row is labelled
with the currency you asked for rather than the chain's own, and a `currency id`
row carries the i-address that name resolved to — a currency name is untrusted
display text somebody registered, so the id is the part that identifies
anything, and it is what the truncated currency on the `OUTPUTS AS BUILT` line
can be matched against by eye. `fee` and `change` stay labelled with the chain's
own currency because they really are native: a token moves as a reserve output
while the miner is still paid in the chain's coins. So do the `OUTPUTS AS BUILT`
figures, which are each output's native `value` — a reserve output's
`0.00000000` is the truth about that output, and the token it holds is on the
line beneath it. `--json` carries `currency` (the name as given) and
`currency_id` (the i-address); both are `null` when native coins are moving, so
a consumer can tell the two apart instead of reading a ticker that might be
either.

**The `to` row carries two names**: the one you typed and the one the node says
it resolved to. The node's half is untrusted display text like any other name —
stripped of control characters and of the invisible and direction-changing ones
that let two different names print identically, folded onto one line and capped
before it goes inside the frame — and the pair is budgeted together, so an
ordinary sub-identity still prints in full rather than losing its middle to an
ellipsis.

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

**The guards sit on the two commands that touch the chain.** `plan send` and
`broadcast` both read `allow_spend`, and `broadcast` also honours `--dry-run` and
refuses `--json` without `--yes` — the same three rules `pecu send` follows,
because between them these two *are* `send` taken apart. A plan cannot be
broadcast, so `plan send` has nothing to stop short of. `sign` is exempt on
purpose: it opens no socket and a signature alone moves nothing, so the machine
holding the key needs no profile that is allowed to spend.

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

**Every string on that panel is the node's word**, the i-addresses included —
the `i-address` row, the `status`, the primary addresses and the two
authorities all arrive as JSON, and nothing between the socket and the frame
checks their shape. So they all go through the same filter a registrant's name
goes through: the address rows budgeted at an address's exact width — an
i-address is exactly 34 characters, so a well-formed one prints whole — and the
`status` word on the same name budget the `name` row uses. An answer that is
neither cannot repaint the row you were told to compare against.

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

**One command runs both phases.** `pecu id register alice` broadcasts the
commitment, waits for it to confirm, then reveals and pays — polling every 30
seconds up to `--timeout` minutes (20 by default). `--no-wait` gives the old
one-step-at-a-time behaviour, which is what a script wants.

Interrupting the wait costs nothing. The reservation is written to disk
**before** the commitment is broadcast, so Ctrl-C, a timeout, or a closed laptop
all leave a registration the same command picks up later. The waiting is a
convenience on top of that, not a replacement for it.

```
  ok step 1 of 2 — commitment bb0a644e…
  ▸ waiting for it to confirm. Interrupting is safe — the file above survives
  waiting — in the mempool, 30s elapsed
  waiting — in the mempool, 60s elapsed
  ok broadcast — txid a290c464…
```

**A commitment carries the expiry height it was built at**, so one that never
confirms eventually stops being broadcastable — the signed bytes are refused
before they reach the mempool, and re-anchoring cannot move an expiry that is
already inside the signature. The saved reservation is then worth nothing but is
still enough to wedge every later attempt at that name, so `--restart` discards
it and claims the name again. Nothing was spent on the dead one.

**A bad `--primary` or `--min-sigs` is refused before the commitment.** Both are
first checked where the SDK builds *step two*, which runs only after the
commitment has confirmed — so an i-address in `--primary`, or a `--min-sigs`
above the number of primaries, used to cost a commitment and up to `--timeout`
minutes of polling before anything said so, and left a reservation escapable
only with `--restart`. Neither needs a node or a key, so both are refused before
the passphrase prompt. Primary addresses are transparent R-addresses: a
registration writes its primary condition as bare key hashes, so a VerusID
cannot be one — delegating control to another identity is what the revocation
and recovery authorities are for. A reservation already on disk that names an
impossible threshold says so before the poll, and points at `--restart`, rather
than failing at the reveal with advice about the node. Both refusals sit above
the `--dry-run` gate as well as the poll: pricing step two of a registration
that can never reach it would be a lie, so a dry run errors rather than
printing an estimate.

**A referral makes you pay less, not more.** Each referrer receives
`fee / (levels + 2)` and your outlay is `fee * (levels + 1) / (levels + 2)`;
whatever the payouts do not consume is burned. On VRSCTEST — 100 coins, 3
levels — that is 80 out of pocket rather than 100:

```
│ fee              80.00000000 VRSCTEST  reduced from 100.00000000 by the referral │
│ referral         pecucli7@                                                       │
│   to referrers   20.00000000 VRSCTEST  across 1 level                            │
│   burned         60.00000000 VRSCTEST                                            │
```

**A referrer who was itself referred is paid too**, one output per level,
nearest first — and the registrant's outlay does *not* change with depth. Only
the split between payouts and burn does.

Both depths proven on chain rather than argued:

| depth | transaction | payout outputs | burned | outlay |
|---|---|---|---|---|
| 0 | `9129ede5…` | none | 100 | 100 |
| 1 | `0ccfe028…` | 1 × 20 | 60 | 80.000224 |
| 2 | `6ab375a6…` | 2 × 20 | 40 | 80.000244 |
| 3 | `60a76a8b…` | 3 × 20 | 20 | 80 |
| 4+ | — | 3 × 20, capped | 20 | 80 |

The depth-2 transaction carries **two** payout outputs, in that order, and both
identities' balances moved by exactly 20. The outlay differs only by miner fees.

**Registration works at every depth**, and the last row is not an assumption.
`idreferrallevels` is 3 on VRSCTEST, and the chain walk truncates to that
*before* the transaction is built — so a request at depth 4 hands the builder the
same three-entry chain a depth-3 request does. There is no distinct depth-4
transaction to fail. Measured: asking for a fourth level under `pecudepth3@`
produces the same 60/20 split against the same 80.

**The cap is silent, though.** Anyone further back receives nothing and is told
nothing, so the panel says so when a chain reaches it — a referrer who was
quietly dropped has no other way to find out.

The walk reads each referrer's **own registration transaction** and takes its
payout outputs, which is how a chain is discovered at all rather than declared.

Both numbers come from the **currency being registered under**, not the chain,
so they differ per currency. The SDK's `registration_fee` is policy *before* the
discount — showing that beside a referral overstated the cost by a fifth and
called money burned when part of it is a payment to somebody.

**`--dry-run` costs nothing and writes nothing.** It prepares the registration,
prints what it would cost, and stops before both the commitment and the saved
file — a saved registration whose commitment was never broadcast would send the
next run to poll for a transaction nobody made. A `--primary` or `--min-sigs`
the reveal could never accept is the exception: it is refused above that gate,
so there is no estimate to print and the run exits non-zero.

**`--json` will not register without `--yes`.** Same rule as `pecu send`, and
this one burns a hundred coins rather than moving them.

**Both hold on the resumed half too.** A run that finds a saved reservation
answers to the same two flags: `--dry-run` prints what finishing would cost,
straight off the file, and stops without polling, broadcasting, or touching the
reservation — including under `--restart`, which reports what it would discard
rather than discarding it. Saved controls the reveal could never accept are the
same exception: refused above the gate rather than priced. `--json` still
refuses to reveal the name and burn the hundred without `--yes`. The one thing
`--json` may do unconsented is *read*: `--no-wait --json` reports whether the
commitment has confirmed, which spends nothing.

The SDK makes the ordering hard to get wrong: `complete` exists only on
`Pending<ReadyToRegister>`, and the only way to hold one is a `poll` that saw the
commitment confirm. Running step two early is a **compile error**, not a spent
commitment.

The success message says *broadcast*, not *registered* — the identity does not
exist until the transaction is mined, and `id show` will say nothing is called
that until then.

### `pecu id update` · `revoke` · `recover` · `unlock`

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

**Timelocks: two forms, and one of them cannot be unlocked by hand.**

```sh
pecu id update i7r29bDQ… --lock-until 1200000    # absolute height
pecu id update i7r29bDQ… --unlock-delay 100      # locked until someone asks
pecu id update i7r29bDQ… --clear-timelock        # remove one
pecu id unlock i7r29bDQ… --extra-blocks 100      # ask, and wait longer than the floor
```

`timelock` on an identity is **either an absolute height or a relative delay**,
and which one it is depends on `FLAG_LOCKED`. `id show` prints whichever it is,
and only when there is one:

```
TIMELOCK
  unlock delay   10 blocks
  state          ! locked, and no unlock requested
```

An absolute height counts down from when it is mined and cannot be paused. A
delay counts down from *nothing at all* until an unlock is requested — so it is
locked indefinitely rather than until some height, and only the revocation and
recovery authorities can act meanwhile.

**`id unlock` is its own command because the height is not the caller's to
compute.** Consensus measures the countdown from the transaction's own
`nExpiryHeight`, not from the tip, so the floor is `delay + expiry` — and the
expiry belongs to the transaction being built. Measured on VRSCTEST with a
10-block delay:

| | |
|---|---|
| tip when signed | 1,177,377 |
| naive `tip + delay` | 1,177,387 — **refused**, 20 blocks short |
| what the flow published | 1,177,407 = `delay + tip + expiry` |

The refusal for a wrong height is `mandatory-script-verify-flag-failed`, naming
nothing, after the transaction is built and signed. A stolen key cannot shorten
a lock either — that was measured too, and it is the property the whole feature
rests on.

**A timelocked identity cannot spend its own funds**, and that is refused before
the transaction is built. `send --from-identity` on a locked identity used to
build and sign a perfectly good transaction and then collect
`mandatory-script-verify-flag-failed`, which names neither the identity nor the
height. Reported as chainvue/verus-rust-sdk#107 and fixed in the SDK, so the
refusal now costs no extra request and distinguishes the two forms: a height to
wait for, or a delay nobody has started — which has no height at all, and whose
remedy is `pecu id unlock`.

**An over-long delay is refused rather than clamped.** Consensus caps it at
`MAX_UNLOCK_DELAY` (~22 years). The daemon's own helper silently clamps instead
of erroring, which can hand back a lock decades shorter than the one asked for;
this refuses, before a key is unlocked.

**Recovery without `--primary` brings the identity back under exactly the keys it
had when it was revoked** — including any that were compromised, which is usually
not what a recovery is for. The panel says so either way.

### `pecu currency`

```sh
pecu currency show TST@
pecu currency launch mytoken@ --from mykey --supply 1000000
pecu currency launch mytoken@ --from mykey --mintable --preallocate iAlice…:500
pecu currency mint mytoken@ --to RComfCn4w…N9Hm --amount 1000
```

**`--register` creates the defining identity if it is missing.** A currency is
defined by an identity, so launching on a name that does not exist yet takes two
registrations' worth of waiting and 300 VRSCTEST. `--register` does the whole
thing in one command: register, wait for the commitment, reveal, wait to be
mined, then launch. `--register-timeout` bounds each of those two waits
separately, so a slow chain can take up to twice it. **The start block is
measured after that wait, not before it.** `--start-in` counts from the tip the
definition is actually built against, which with `--register` is the tip once
the registration is mined — so a registration that takes eight blocks does not
eat eight blocks of the offset and leave the launch refused for starting in the
past.

**It stops rather than waiting for a registration it did not send.** Two cases
end the command early instead of polling. A dry run stops: registering burns 100
VRSCTEST, so `--dry-run` will not do it, and with no identity on chain there is
nothing for the launch to be defined by and nothing to price a preview against —
`pecu id register <name> --dry-run` prices the registration on its own, and the
launch preview works once the identity exists. And a registration that does not
finish inside the one run stops too — `--json` stops at the commitment, and the
commitment may not confirm inside `--register-timeout` — because past that point
there is no reveal on its way to a block to wait for. Nothing is lost either
way: the reservation is on disk before anything is broadcast, and the same
command carries on from it.

It is opt-in and stays that way. A misspelled name is a plausible mistake and
registration burns 100 VRSCTEST — creating `pecubaskt1@` because somebody typed
their basket's name wrong would be an expensive convenience. Without the flag a
missing identity is refused, and the refusal names the flag:

```
× reading the defining identity failed
 help: a currency is defined by an identity, and that name is not on this
       chain yet. Add --register to create it first, for 100 VRSCTEST on top
       of the launch fee, or register it separately with `pecu id register`
```

**A currency is something an identity becomes.** There is no separate object:
the currency's id *is* the defining identity's i-address, and consensus marks
that identity so it can **never define another**. Closer to claiming a name than
to deploying a contract.

```
┌─ CURRENCY ──────────────────────────────────────────────────────┐
│ name          TST                                               │
│ currency id   iK2k8YH1jfR7RLmEZ3zac2Mkx5rxSgbMqg                │
│ parent        iJhCezBExJHvtyH3fGhNnt2NhU4Ztkf2yq                │
│ kind          token                                             │
│ control       centralized — the defining identity can mint more │
│ starts        block 879,130                                     │
├─ PREALLOCATED ──────────────────────────────────────────────────┤
│               200.0 VRSCTEST  iK2k8YH1j…bMqg                    │
└─────────────────────────────────────────────────────────────────┘
```

**`options` and `proofprotocol` are decoded, not printed.** They are a bitfield
and an enum, they are what the currency *is*, and neither is inferable from the
name. `options: 32` on a panel tells a reader nothing they can act on; `token`
does. The raw values are still in `--json` alongside the decoded ones.

**The `currency id` row is the node's word too**, and it is the row the rest of
the panel is meant to be checked against — so it goes through the same filter
the name above it does, budgeted at an address's exact width. An i-address is
exactly 34 characters, so a well-formed one prints whole; an answer longer than
that is not an address, whatever field it arrived in, and cannot forge a row
inside the box. The `currency id` on `MINT` and `PRECONVERT` and the `into` row
on `CONVERT` are filtered the same way, because each of those panels precedes a
spend. The launch panels are a different case: their id is re-encoded from the
currency id the launch itself produced, not reprinted from anything a node
said.

**`--supply` becomes a preallocation to the defining identity**, and has to. A
token's supply is the **sum of its preallocations** — `initial_supply` is read
only for a fractional currency, so setting it on a token produces one with no
supply at all. The panel shows the resulting preallocation rather than hiding
the substitution.

**Decentralized by default.** `--mintable` sets `proofprotocol = 2`, letting the
identity mint more later. It cannot be undone, and a fixed supply is the
property a holder can actually verify, so it is an opt-in.

**Fractional baskets** are reserve-backed: a share of each reserve, priced
against an initial supply.

```sh
pecu currency launch mybasket@ --from key --supply 100 \
  --reserve VRSCTEST:50 --reserve TST:50
```

```
┌─ WOULD LAUNCH ───────────────────────────────────────────────────────┐
│ identity      pecudepth2@                                            │
│ currency id   iSHPgvF7f4huHK5WZ52tURDkZxbkCvsYke                     │
│ kind          fractional basket, token                               │
│ control       decentralized — supply moves as reserves convert in    │
│               and out                                                │
│ starts        block 1,178,783                                        │
│ fee           200.00000000 VRSCTEST                                  │
│ txid          731eaf355203611e3dd69488ed6b4c535ac1d5d629c0585e0ea274 │
│               699653a090                                             │
│ supply        100.00000000  the reserves are priced against this     │
├─ RESERVES ───────────────────────────────────────────────────────────┤
│                 62.5%  iJhCezBEx…f2yq                                │
│                 37.5%  iK2k8YH1j…bMqg                                │
└──────────────────────────────────────────────────────────────────────┘
```

Percentages, not the raw weights consensus stores — those are fractions of
`SATOSHIDEN`, and asking for `25000000` instead of `25` invites an
off-by-a-factor that prices the basket wrongly forever. They must total exactly
100, checked before a node is reached or a key unlocked.

**A basket reads `--supply` from a different field than a token does.** A
token's supply is the sum of its preallocations; a basket's is `initial_supply`,
which every reserve price divides by — so a basket without one gets a price of
zero on every reserve, and is refused.

**`--mintable` and `--reserve` do not compose.** A basket mints and burns by
conversion; `--mintable` is the token idea of an issuer topping up a supply.

**`--contribute` is refused, and the thing it sounded like is `preconvert`.**
Seeding a reserve at launch means an extra value-bearing output funding it. The
SDK's launch builder emits seven outputs and never that one, and the launch
notarization it publishes in the same transaction states the reserves hold
nothing — so a definition naming contributions would claim backing nothing put
there, permanently, while not one satoshi left the signing key. This repo has
the artefact: `pecudepth2@` (`0b08811f…`) went out with exactly such a
declaration and nothing behind it. `pecu` now refuses the flag before a key is
unlocked or a node is asked, and names `pecu currency preconvert` — which does
spend — as the way to put coins into a reserve before the start block. That is
also the answer to the `--max-preconvert` trap two paragraphs down: the
contribution a fractional basket needs from every reserve comes from
`preconvert`, not from the definition. The SDK has since learnt to build the
funding output ([#129](https://github.com/chainvue/verus-rust-sdk/issues/129)),
and the pin has moved — so what the refusal now waits on is one basket launched
with a seeded reserve and checked at the start block, not a dependency.

**`--conversion` is refused too, and permanently.** A fractional basket's
pre-launch price is not a number in its definition: consensus derives it at
launch as `SATOSHIDEN³ / (initial supply × weight)` and writes it into the
launch notarization published in the same transaction. The `conversions` field
the flag wrote is read by nothing, and the daemon zeroes it on the way in — a
definition created by passing `[4.0]` comes back carrying `[0.0]`, and every
fractional definition in the SDK's captures of real daemon output has an
all-zero vector. The old flag was worse than useless: it required `--reserve`,
which is the one configuration where the field is derived and ignored, and the
confirmation panel printed the number back as `rate` beside genuinely effective
rows, right before the prompt that spends 200 VRSCTEST on something
unchangeable. The figure that does move the price is `--supply`, the
denominator every reserve price divides by — the one the panel labels *the
reserves are priced against this*. Unlike `--contribute`, this refusal has no
expiry: it is a consensus fact rather than an SDK gap.

**The prelaunch economics, and the sub-identity policy.** All of it is per
reserve, and all of it is keyed by the reserve's *name*:

```sh
pecu currency launch mybasket@ --from key --supply 100 \
  --reserve VRSCTEST:60 --reserve TST:40 \
  --min-preconvert VRSCTEST:1 --min-preconvert TST:1 \
  --max-preconvert VRSCTEST:1000 --max-preconvert TST:1000 \
  --prelaunch-discount 5 --prelaunch-carveout 10 \
  --id-registration-fee 25 --id-referral-levels 2 --id-import-fee 0.02
```

```
RESERVES
                     60%  iJhCezBEx…f2yq  min 1.00000000  max 1000.00000000
                     40%  iK2k8YH1j…bMqg  min 1.00000000  max 1000.00000000
  discount       5%  to anyone converting before launch
  carveout       10%  of the launch, to this identity

SUB-IDENTITIES
  registration   25.00000000
  referrals      2 levels  optional
  import         0.02000000
```

**Keyed by name rather than by position, deliberately.** The definition stores
these as vectors indexed by the reserve list, and `serialize_definition` refuses
one whose *length* disagrees — but a vector of the right length in the wrong
order is accepted, and prices the basket against the wrong currencies. Naming
the reserve removes the possibility instead of checking for it, and a reserve
you say nothing about gets zero rather than somebody else's number.

**For `--max-preconvert`, name every reserve or name none.** This one is a trap
worth spelling out, because it cost a real launch. A cap of zero is *"nothing
accepted"*, not *"no limit"* — consensus refunds anything over the cap
(`GetRefundTransfer`, not a rejection), and once the vector exists at all a
reserve nobody named is a zero rather than an absence. Meanwhile a fractional
basket refunds the **entire launch** unless every one of its reserves receives a
contribution (`notarization.cpp:1474`). So capping one reserve of two silently
guarantees the launch fails — hours later, at the start block, with the 200
VRSCTEST gone.

Naming none is safe and common: an empty vector is never consulted, so every
reserve stays uncapped. `pecu` refuses the half-named case at launch, which is
the last moment anything can be changed:

```
× --max-preconvert names some reserves but not `dude-test-centralized`,
│ which caps it at zero
 help: a cap of zero means nothing is accepted into that reserve, not that it
       is unlimited — and a fractional basket refunds the entire launch unless
       every reserve receives a contribution…
```

`preconvert` refuses the same thing from the paying side, and its panel lists
what each reserve holds so far — an empty leg is the difference between a
contribution working and coming back.

`--id-referral-levels` sets the referral option bit on its own: consensus pays
referrals only when the bit says to, so a level count without it publishes a
policy that never applies.

Everything above appears on the panel before you confirm, because none of it can
be changed afterwards.

Still absent: `notaries` and `min_notaries_confirm`, which only matter
cross-chain, and `gateway_converter_issuance`, which belongs to the gateway case
the SDK refuses outright.

It costs `currencyregistrationfee`, 200 VRSCTEST at the time of writing, read
from the parent's chain policy rather than assumed — except for an NFT, which is
charged the parent's `idimportfees` instead, 0.02. Consensus picks between the
two on the tokenized-control bit, so `--nft` pins the fee rather than taking the
one the flow would read.

#### `pecu currency mint`

```sh
pecu currency mint mytoken@ --to RComfCn4wHHsGR8vWBAU7T1r3tHHyxN9Hm --amount 1000
```

Only for a currency launched `--mintable`. `proofprotocol = 2` is the whole
permission system, and it is decided once, at launch.

**The identity pays, not the signing key.** This is the part that catches
people, so it is on the panel in its own row. Consensus accepts new supply only
from a transaction that *spends an output the controlling identity holds* — the
controlling identity being the currency itself, same i-address. A wallet with a
well-funded key and an empty identity cannot mint, and the refusal says so:

```
× minting failed
╰─▶ i49TaUGBXA4ZHbybQe3tw1r58BhCW361SC holds no spendable outputs; a mint is
    paid for by the identity — send() it some coins first
 help: a mint is paid for by the identity, not by the signing key — consensus
       accepts new supply only from a transaction that spends what the
       identity holds. Send it some native coins first: `pecu send --to
       pecurefcur1@ --amount 1`
```

```
┌─ MINT ──────────────────────────────────────────────────────────────────┐
│ currency      pecuref9                                                  │
│ currency id   iKh6DBXjPVU72BBD4sq5qbdFFeQGVcYokg                        │
│ amount        1000.00000000  new supply, created by this                │
│ to            RComfCn4wHHsGR8vWBAU7T1r3tHHyxN9Hm                        │
│ paid by       pecuref9  the identity's own coins, not the key's         │
│ signed by     faucet  RComfCn4w…N9Hm                                    │
│ fee           0.00020000                                                │
└─────────────────────────────────────────────────────────────────────────┘
  ▸ this currency is centralized — its supply is whatever its identity
    decides, and every holder is trusting that
```

**The recipient must be a transparent R-address**, and that is an SDK limit
rather than a protocol one, and it is a limit `pecu` now keeps on its own.
Consensus treats `DEST_ID` as a first-class reserve transfer destination —
`sendcurrency` pays identities routinely — and `build_conversion` used to write
every recipient as `Destination::PubKeyHash`, discarding the address kind. An
i-address run through that would have paid the R-address sharing its hash, which
nobody holds a key to.

The SDK maps `AddressKind::Identity` to `Destination::Identity` now
([chainvue/verus-rust-sdk#115](https://github.com/chainvue/verus-rust-sdk/issues/115)),
so the refusal here is no longer describing the SDK — it is waiting for somebody
to pay an identity a token and read it back off the chain as the *identity's*
holdings rather than a key holder's.

The same limit means **a token cannot be paid to a VerusID at all** — `pecu send
--to <name@> --currency <token>` is refused for the same reason, while the same
command *without* `--currency` sends native coins to an identity fine. So an
identity can hold native coins but not tokens, which is awkward exactly where it
matters most: the issuing identity is the natural place for a centralized
currency's treasury, and it is the one destination a mint cannot name.

There is no workaround that means the same thing, and the error text says so.
Minting to one of the identity's primary addresses puts the tokens with whoever
holds that key — a different owner with different authority, not the identity's
own holdings.

The two refusals that are not about permissions are worth keeping distinct. A
**decentralized** currency has no authority that could add to it, which is the
property its holders can verify — not a lock to be worked around. A **fractional
basket** has no issuer at all: its supply grows when reserves convert in and
shrinks when they convert out.

#### `pecu currency preconvert`

```sh
pecu currency preconvert mybasket@ --amount 10 --from mykey
pecu currency preconvert mybasket@ --amount 10 --spend TST --from mykey
```

Buys into a currency **before it launches**, at the launch price. `--spend`
defaults to the chain's own currency and must name one of the target's reserves;
`--to` defaults to the paying key.

**There is no estimate, and the panel does not pretend otherwise.** A launching
currency has no reserves, so there is nothing to price against — what a
contribution pays out is settled at the start block, from the final ratio of
everyone's contributions together. Two identical commands a day apart can pay
out differently because other people contributed in between. The SDK refuses a
slippage floor here by name for the same reason, so the command never offers
one; a floor could only be checked against a number nobody produced.

```
┌─ PRECONVERT ─────────────────────────────────────────────────────────────┐
│ into          pecubask1                                                  │
│ currency id   i9dpvtcsH6FRD4UmNVur75cLXj7rUx9iD1                         │
│ spending      5.00000000  VRSCTEST                                       │
│ you receive   settled at launch  from the final ratio of every           │
│               contribution                                               │
│ to            RComfCn4wHHsGR8vWBAU7T1r3tHHyxN9Hm                         │
│ launches      block 1,179,161  153 blocks to go                          │
│ fee           0.00020000                                                 │
└──────────────────────────────────────────────────────────────────────────┘
  ▸ if the launch misses its minimum, every contribution is refunded —
    including this one, to the paying key
  ▸ over the maximum is refunded too, rather than refused, so this can come
    back even if the launch succeeds
```

**Preconvert and convert are never both valid.** Before the start block a plain
conversion is refused for want of reserves; after it a preconversion is refused
in turn. Which one applies is decided entirely by the height, so this refuses
locally and names the block rather than letting the chain answer:

```
× `pecudepth2` launched at block 1178834, and the tip is 1179008
 help: a preconversion buys at the launch price and is only accepted before
       the start block. Afterwards the currency has reserves and an ordinary
       conversion is the thing that works — the two are never both valid
```

**Two ways a contribution comes back**, both worth knowing before sending one.
A launch that misses its `min_preconversion` refunds *everyone*. And a
contribution that pushes a reserve past its `max_preconversion` is **refunded
rather than refused** — consensus calls `GetRefundTransfer` rather than
rejecting the transaction, so it can come back even when the launch succeeds.
The panel shows both figures when the definition sets them.

Paying in a currency the target is not backed by is also refunded rather than
refused, so that one is checked here against the definition's reserve list — a
mistake that would otherwise cost a wait rather than an error.

#### `pecu currency convert`

Once a basket has launched, value moves three ways — and consensus writes all
three as the same `CReserveTransfer`, differing only in which currency each slot
names:

```sh
pecu currency convert mybasket@ --amount 10                      # a reserve into the basket
pecu currency convert VRSCTEST  --amount 10 --spend mybasket@    # the basket back into a reserve
pecu currency convert SPORTS    --amount 1  --via bankroll       # one reserve into another
```

Which shape you mean is inferable from the definitions, so it is inferred rather
than asked for — and then **stated on the panel**, because guessing silently
would be worse than asking, and saying which guess was made is better than both.

```
┌─ WOULD CONVERT ──────────────────────────────────────────────────────────┐
│ spending      1.00000000  VRSCTEST                                       │
│ into          SPORTS  iGhBps9rmbN7U544dZY7nx2rfg26QTh1zY                 │
│ through       bankroll  one reserve into another, priced by the basket   │
│ you receive   15897.04750000  estimated, not guaranteed                  │
│ at least      15000.00000000  checked now, not by the chain              │
│ fee           0.00020000                                                 │
└──────────────────────────────────────────────────────────────────────────┘
```

**Unlike a preconversion, this has a price** — a launched basket has reserves,
so the node can estimate. That is what makes `--min-out` meaningful here and
impossible before launch. The floor is checked **before signing and never
again**: the chain does not enforce it, so if the price moves after broadcast
the conversion still happens at whatever the reserves make it.

Three refusals worth having, all local:

* **Not launched yet** — points at `preconvert`, which is the thing that works
  before the start block. The exact mirror of `preconvert`'s own check; the two
  are never both valid.
* **Launch refunded** — a basket whose launch failed still reads as a live
  currency definition but holds nothing and never will. Without this the only
  signal is an estimate of zero.
* **Neither side is a basket** — names the three shapes rather than reporting
  that the chain refused.

Launched on VRSCTEST rather than argued about, each on its own identity because
a slot is one-shot:

| combination | identity | transaction |
|---|---|---|
| decentralized token, fixed supply | `pecudepth3@` | `2fecffbb…` |
| centralized token, `proofprotocol` 2 | `pecuref9@` | `8764a045…` |
| fractional basket, min/max preconvert, 5% discount, 10% carveout | `pecudepth2@` | `0b08811f…` |
| centralized, governs sub-identity registration: 25 fee, 3 referral levels, mandatory | `pecurefcur1@` | `3205c03f…` |
| NFT | `pecunft1@` | refused — see below |
| **mint** — 1,000 new supply on a centralized token | `pecuref9@` | `e8c9d409…` |
| **preconvert** — 5 VRSCTEST into a pre-launch basket | `pecubask1@` | `0bb8a7ae…` |
| **convert** — 1 VRSCTEST into a live basket, `--min-out` floor honoured | `triccrypto2` | `68c8363c…` |

**`--nft` is built but consensus refuses it, and that is upstream.** An NFT is a
*currency-mapped* token: `options 2080`, one satoshi of supply preallocated to
the defining identity, and — non-obviously — `currencies = [parent]` despite not
being fractional, because consensus requires `maxPreconvert.size() == 1` and the
per-reserve vectors are indexed by the reserve list. `pecu` builds all of that,
and the transaction decodes field-for-field identical to a working on-chain NFT
across all seven outputs.

It was refused by one missing destination. An identity with tokenized control
carries a *second* destination on its recovery condition — the key hash of the
`EVAL_IDENTITY_RECOVER` contract pubkey, a constant — and the SDK's identity
output script did not emit it, so consensus derived a different script. That sat
inside the transaction builder where no caller could reach it, so the flag
stopped at a diagnostic rather than at `bad-txns-failed-precheck`.

`identity_primary_script` takes the tokenized-control flag now
([chainvue/verus-rust-sdk#111](https://github.com/chainvue/verus-rust-sdk/issues/111)),
so the refusal has outlived its reason and is kept only until an NFT launch is
watched onto the chain — three of the seven gaps that closed were NFT ones, and
they want proving together. Nothing is spent by the attempt meanwhile: the fee
is not paid and the identity's one currency slot is untouched.

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

## TODO

### Guards waiting on a chain, not on an SDK

Each of these is built here and refused here, with a named diagnostic — none
reaches the user as a bare node error, and none spends anything on the way to
failing.

They were refused because the SDK could not express them. **That stopped being
true on 7 August**, when every one of these closed upstream; the pin caught up
on the 22nd. What stands between them and working is no longer a dependency, it
is evidence: each is a money path, and the only tests that exercise one spend
real VRSCTEST against a public node. So the guards stay until somebody removes
one and watches it land.

| | closed upstream by | what pecu does today | what removing the guard takes |
|---|---|---|---|
| **Mint / preconvert / convert to a VerusID** — `build_conversion` wrote every recipient as `PubKeyHash` and dropped the kind, so an i-address would have paid the R-address sharing its hash | [#115](https://github.com/chainvue/verus-rust-sdk/issues/115) — `convert.rs` now maps `AddressKind::Identity` to `Destination::Identity` | refuses, and says paying a primary address is *not* the same thing | pay an identity a token and read it back off the chain as the identity's, not a key holder's |
| **Token send to a VerusID** — same root cause, so an identity could hold native coins but not tokens | [#115](https://github.com/chainvue/verus-rust-sdk/issues/115) | refuses with the native-vs-token asymmetry spelled out | as above; the two share a code path and should be proven together |
| **NFT launch** — the identity output omitted the tokenized-control recovery destination, so consensus derived a different script | [#111](https://github.com/chainvue/verus-rust-sdk/issues/111) — `identity_primary_script` now takes `has_tokenized_control` | `--nft` builds the definition, then reports the gap | launch one, and check consensus accepts the identity output rather than `bad-txns-failed-precheck` |
| **NFT launch fee** — charged `currencyregistrationfee` (200) instead of `idimportfees` (0.02) | [#112](https://github.com/chainvue/verus-rust-sdk/issues/112) | pins the right fee itself | confirm the fee the flow now reads matches the one pinned here, then stop pinning it |
| **NFT definition shape** — `NFT_TOKEN` on a `token()` could not express a valid one | [#113](https://github.com/chainvue/verus-rust-sdk/issues/113) — `CurrencyDefinition::nft()` exists | sets all five fields by hand | decode a launch built by the constructor against one built by hand; they should agree field for field |
| **Seeded contributions at launch** — the builder emitted no output funding a declared contribution, and the notarization in the same transaction said the reserves were zero | [#129](https://github.com/chainvue/verus-rust-sdk/issues/129) — the daemon's eighth output is built now | refuses `--contribute` and points at `preconvert` | launch a basket with a seeded reserve and confirm the reserve actually holds it at the start block |

Two rows left this table rather than moving down it.

**The expired commitment** ([#114](https://github.com/chainvue/verus-rust-sdk/issues/114)) is done. `CommitmentStatus::Expired` exists and `resume` matches it by name, before any broadcast — where the old string match on `expiring-soon` could only fire after one had already been attempted, and the two states need opposite actions. The string match is kept as a fallback for the same rejection arriving unclassified.

**Two payments in one block** ([#118](https://github.com/chainvue/verus-rust-sdk/issues/118)) needs no guard removed, because `pecu` never had one — the limitation was simply true. `funding` now reads the mempool and withholds coins an unconfirmed transaction already spends, so a second payment should build against different coins. Unproven here: it wants two sends in one block, watched.

### Built but unproven

Paths that exist and have never been exercised against the chain. They are not
claimed as working:

* **`pecu key --history` on a key with more than one revision.** The single
  revision case is covered; nothing has rotated yet.
* **Multi-signature identities.** `send`, `mint` and the whole lifecycle sign
  with one key and refuse an m-of-n identity by name. The air-gap trio is the
  natural home for this, once it learns identity inputs.

### Parked

* **`feat/m8-login-vdxf`** — `id login` and VDXF publish/read, deliberately kept
  off `main`. Well behind now; needs a rebase before it is worth looking at.

## Not in scope (yet)

Shielded/z-address operations, the currency launch wizard, marketplace offers, and a
live `watch` dashboard. They come after the transparent and identity paths are solid.

## License

Apache-2.0, matching the SDK.
