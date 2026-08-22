# verus-pecu-cli

```
                             ┌─┐┌─┐┌─┐┬ ┬
                             ├─┘├┤ │  │ │
                             ┴  └─┘└─┘└─┘
              a Verus wallet that lives in your terminal
```

`pecu` is a command-line Verus wallet, and the example app for the
[Verus Rust SDK](https://github.com/chainvue/verus-rust-sdk). Keys, transparent
sends, air-gapped signing, transaction decoding, the VerusID lifecycle and
currency operations — from one binary.

![pecu generating a key, decoding a transaction and reading an identity](docs/media/demo.svg)

## No full node

There is no `verusd` to install, no chain to sync, no wallet daemon to keep
running. `pecu` asks a public RPC endpoint questions and hands it finished
transaction bytes. Nothing else.

```sh
pecu id show VRSCTEST@      # ~0.2s, about as fast as curl to the same node
```

Your keys never leave the machine you are on. They are generated locally,
encrypted at rest with Argon2id + ChaCha20-Poly1305, and signing happens in
process — the node is never asked to hold, unlock or use a key. Point it
somewhere else with `--node` or a config file; it is one URL.

## Install

Rust 1.95, which `rust-toolchain.toml` pins.

```sh
cargo build --release
./target/release/pecu --help
```

## What it does

| Command | Does |
|---|---|
| `pecu doctor` | Node reachability, chain tip, config paths, build info |
| `pecu key gen\|import\|list\|show\|export\|phrase` | Encrypted keystore |
| `pecu wallet balance\|utxos\|history` | Spendable, withheld, token and unconfirmed balances |
| `pecu tx explain` | Says what every output in a transaction actually *is* |
| `pecu send` | Transparent sends: native, token, or out of a VerusID's own funds |
| `pecu plan send` · `sign` · `broadcast` | The air-gap trio, over files or QR codes |
| `pecu id show\|register\|update\|revoke\|recover\|unlock` | The VerusID lifecycle, including timelocks |
| `pecu currency show\|launch\|mint\|preconvert\|convert` | Currency definitions, launches, minting, conversions |

Every command takes `--explain`, which prints the exact `verus-sdk` calls it
made. The output is meant to teach you the SDK.

Full reference: **[docs/commands.md](docs/commands.md)** ·
[configuration](docs/configuration.md) · [design notes](docs/design.md) ·
[status](docs/status.md)

## Read an identity

```
$ pecu id show VRSCTEST@

IDENTITY
  name          VRSCTEST@
  i-address     iJhCezBExJHvtyH3fGhNnt2NhU4Ztkf2yq
  status        ok active
  last change   block 1,186,412   2026-08-12 02:08 UTC  (10d 20h ago)

CONTROL
  signatures    1-of-1
  - RXKs5Gz8kRqpA52M25AW5FzP3aCNq46yMh
  revocation    iJhCezBExJHvtyH3fGhNnt2NhU4Ztkf2yq  (itself)
  recovery      iJhCezBExJHvtyH3fGhNnt2NhU4Ztkf2yq  (itself)

  - this identity cannot be revoked: it is its own recovery authority, and
    consensus refuses a revocation nobody could undo
```

That last line is the point of the tool. An identity that is its own recovery
authority is unrevokable, and nothing in the raw RPC reply says so.

## Decode a transaction

Offline. Takes hex on the command line, from a file, or on stdin.

```
$ pecu tx explain "$(cat fixtures/coinbase.hex)"

TRANSACTION
  txid     7f291acf765bf375baae43337c5b27fd123800e5486782ea00f0ff90603cd894
  expiry   ! never — this transaction stays minable forever

OUTPUTS
  4 outputs — 3.00010000 in native satoshis
  #0 3.00010000
       -> R9UJQch4pJLAo649s5yQh5hp2tXZij9oQG  pays a bare public key, 33 bytes — a mined coinbase
  #1 0.00000000
       a CryptoCondition this SDK does not decode (eval 4) — it cannot hold currency
```

## Register a VerusID

Two transactions: one commits to the name, a second claims it once the first
confirms. `pecu` runs both and waits in between.

```
$ pecu id register alice --from mykey

  ok step 1 of 2 — commitment d29cd23570a9…
SAVED
  registration   ~/.config/verus-pecu/pending/alice.json
  - this file holds the salt. Without it the name cannot be claimed and the
    commitment fee is lost
  waiting — in the mempool, 30s elapsed
  ...
  ok broadcast — txid 5f7d49c59fec…
REGISTRATION SENT
  name        alice@
  paid        100.00000000 VRSCTEST
```

**Interrupting is safe.** The reservation is written to disk *before* the
commitment is broadcast, so Ctrl-C, a timeout or a dead connection all leave a
registration the same command picks up. That path is exercised, not theoretical:
the run above lost its node mid-wait and resumed from the file.

## The air gap

Three commands, three machines. The one holding the key never opens a socket.

```sh
pecu plan send --address R… --to R… --amount 1 --out plan.hex   # online, no key
pecu sign @plan.hex --key cold --out signed.hex                 # offline, no node
pecu broadcast @signed.hex                                      # online, no key
```

Each step also speaks QR codes, so the offline machine needs no cable.

## Spending is guarded

- **Mainnet ships unable to spend.** `allow_spend` is off for it; moving real
  coins takes a deliberate edit, not a forgotten `--profile`.
- **`--dry-run` builds and stops.** Nothing is broadcast, nothing is written.
- **`--json` is output, not consent.** Machine-readable mode refuses to spend
  without `--yes`, because the confirmation prompt would go to the stream you
  are parsing.
- **Irreversible things say so first**, on a panel, before asking.

## Status

Early, and honest about it.

**Proven on chain:** identity registration and reads, transaction decoding, key
management, balances and UTXO reads, transparent sends.

**Built, waiting on a chain:** `--nft`, seeded contributions at launch, and
paying a token to a VerusID are each refused by a named diagnostic rather than
attempted. The SDK gaps they were written for have since closed upstream;
`tests/upstream.rs` proves that offline. Removing a guard needs it watched onto
the chain, which needs DeFi enabled on the test chain — see
[docs/status.md](docs/status.md).

**Not in scope:** shielded/z-address operations, marketplace offers.

## License

Apache-2.0, matching the SDK.
