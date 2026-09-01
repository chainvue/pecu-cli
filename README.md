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

**[chainvue.github.io/pecu-cli](https://chainvue.github.io/pecu-cli/)** — the
same documentation, searchable, with the demos below playing in place.

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="docs/media/doctor-dark.svg">
  <img alt="pecu doctor reporting profile, node, build, chain tip and DeFi being switched off" src="docs/media/doctor-light.svg">
</picture>

Every demo here is one recording shown twice: phosphor green if you read GitHub
in the dark, the same panels in dark ink if you read it in the light. `pecu`
decides for itself whether to emit colour at all — a pipe or `NO_COLOR` gets
none — but it never guesses your terminal's background, because there is no way
to ask that is worth a wrong answer on a spend confirmation. On a light
terminal, say so once:

```sh
export PECU_THEME=light     # or --theme light, one command at a time
```

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
| `pecu doctor` | Node reachability, chain tip, whether DeFi is switched off, config paths, build info |
| `pecu key gen\|import\|list\|show\|export\|phrase` | Encrypted keystore |
| `pecu wallet balance\|utxos\|history` | Spendable, withheld, token and unconfirmed balances |
| `pecu tx explain` | Says what every output in a transaction actually *is* |
| `pecu send` | Transparent sends: native, token, or out of a VerusID's own funds |
| `pecu plan send` · `sign` · `broadcast` | The air-gap trio, over files or QR codes — the chain's own coins only |
| `pecu id list` | Which VerusIDs an address is a primary of — the one read that starts from a key |
| `pecu id show\|register\|update\|revoke\|recover\|unlock` | The VerusID lifecycle, including timelocks |
| `pecu currency show\|launch\|mint\|preconvert\|convert` | Currency definitions, launches, minting, conversions |

Every command takes `--explain`, which prints the exact `verus-sdk` calls it
made. The output is meant to teach you the SDK.

Full reference: **[docs/commands.md](docs/commands.md)** ·
[configuration](docs/configuration.md) · [design notes](docs/design.md) ·
[status](docs/status.md)

## Read an identity

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="docs/media/id-dark.svg">
  <img alt="pecu id show VRSCTEST@, listing authorities and timelock state" src="docs/media/id-light.svg">
</picture>

The last line is the point of the tool. An identity that is its own recovery
authority is unrevokable, and nothing in the raw RPC reply says so.

## Decode a transaction

Give it a txid and it fetches; give it hex or `-` and it never touches the
network. This is a real currency launch on VRSCTEST — seven outputs, of which
one *holds a VerusID* and one holds reserves:

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="docs/media/tx-dark.svg">
  <img alt="pecu tx explain decoding a real VRSCTEST currency launch" src="docs/media/tx-light.svg">
</picture>

Output `#2` is the reason this command exists: an undecodable CryptoCondition
that **may hold currency**, called out rather than shown as an empty `0.00000000`.

## Check a balance

Spendable, withheld, in-conditions and every token on the address, separated —
because a balance that adds them up is a balance you cannot spend from.

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="docs/media/wallet-dark.svg">
  <img alt="pecu wallet balance separating spendable, withheld and token balances" src="docs/media/wallet-light.svg">
</picture>

## Register a VerusID

Two transactions: one commits to the name, a second claims it once the first
confirms. `pecu` runs both and waits in between.

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="docs/media/register-dark.svg">
  <img alt="pecu id register running both phases, saving the reservation and revealing the name" src="docs/media/register-light.svg">
</picture>

**Interrupting is safe.** The reservation is written to disk *before* the
commitment is broadcast, so Ctrl-C, a timeout or a dead connection all leave a
registration the same command picks up. That is not theoretical: an earlier run
lost its node mid-wait and resumed from the file.

## Send

`--dry-run` builds and signs and stops, so you can read the panel before
anything is broadcast. `--json` refuses to spend without `--yes`.

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="docs/media/send-dark.svg">
  <img alt="pecu send --dry-run showing the review panel, the built outputs and the signed transaction" src="docs/media/send-light.svg">
</picture>

### Sending a token

`--currency` moves a token instead of the chain's own coins. The amount is
labelled with what is actually moving; the fee stays in VRSCTEST, because that
is what the miner is paid in. The `currency id` row is what the name resolved
to — a name is text somebody registered, the id is the part that identifies
anything.

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="docs/media/send-token-dark.svg">
  <img alt="pecu send --currency moving a token, with the amount labelled by the token and the fee in VRSCTEST" src="docs/media/send-token-light.svg">
</picture>

The token-carrying outputs read `0.00000000 VRSCTEST` because a token rides in
the output's script, not its value. Five go to the recipient, twenty come back
as change — which is the sum a reader is meant to check before typing yes.

## The air gap

Three commands, three machines. The one holding the key never opens a socket.

```sh
pecu plan send --address R… --to R… --amount 1 --out plan.hex   # online, no key
pecu sign @plan.hex --key cold --out signed.hex                 # offline, no node
pecu broadcast @signed.hex                                      # online, no key
```

Each step also speaks QR codes, so the offline machine needs no cable.

**The gap carries the chain's own coins, and nothing else.** `pecu plan send`
takes `--currency` and `--from-identity` and refuses each by name, before it
opens a socket: a token rides in an output's script and a VerusID's funds sit in
pay-to-identity outputs, and the SDK builds an unsigned form of neither — every
token and identity builder signs as it builds, so there is no partial to carry
offline. `pecu send` moves both, and it signs on the machine that talks to the
node.

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

## Contributing

Issues use the [Spec template](.github/ISSUE_TEMPLATE/spec.yml), and a specified
issue can be picked up by an automated pipeline: a spec gate reviews it, an agent
implements it, and an adversarial reviewer scores the pull request before a human
reads it. Nothing merges itself. [docs/claude-automation.md](docs/claude-automation.md)
is the operating manual; [CLAUDE.md](CLAUDE.md) is what the agents read before
touching the code.

Pull requests from forks are reviewed by hand — a workflow with write permissions
should not run against code an outsider controls, and on a wallet that is not a
close call.

### Branch protection

The automation assumes these are set, and several of its guarantees are only
guarantees because they are. They cannot be configured from a file; set them in
**Settings → Branches → Add rule** for `main`, or as a ruleset.

- **Require a pull request before merging.** Nothing pushes to `main` directly.
- **Require approvals: 1.** The pipeline exists to produce reviewable pull
  requests, not to merge them.
- **Require review from Code Owners.** This is what makes
  [`.github/CODEOWNERS`](.github/CODEOWNERS) do anything. Without it, an agent
  could change the workflow that reviews it, or the tests that constrain it,
  with no human in the loop.
- **Dismiss stale approvals when new commits are pushed.** An approval of a
  diff that no longer exists is worse than no approval.
- **Require status checks to pass**, and select **`fmt, clippy, test`** (the job
  in `ci.yml`) and **`Find reasons not to merge`** (the review job). Also tick
  *Require branches to be up to date before merging*.
- **Do not allow force pushes**, and **do not allow deletions**.
- **Do not** enable auto-merge. The score on a pull request is advice.

One more, outside branch protection: **Settings → Actions → General → Workflow
permissions** must have *Allow GitHub Actions to create and approve pull
requests* enabled, or the implementation agent cannot open one. In an
organisation, the same switch exists at organisation level and overrides this.

## License

Apache-2.0, matching the SDK.
