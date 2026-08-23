# Configuration


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

# How long to wait for a single RPC reply, in seconds. Short by default so a
# wrong URL fails while you are still looking at the terminal; a busy public node
# can take longer than that to answer `getaddressutxos`, and past the ceiling the
# read fails outright rather than slowly. Nothing to do with `id register
# --timeout`, which is in minutes and bounds a wait for confirmations.
timeout_secs = 20
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

Under `--json`, stdout carries one JSON document and nothing else. Panels,
notes, progress lines and the `--explain` record are not written there at all —
`--explain` goes to stderr instead — so `pecu … --json | jq` never has to step
over prose, and a consumer that stops reading gets the exit code back rather
than a broken-pipe panic.

Two commands are exceptions, and both are cases where there is no document to
print: `dev ui` renders the widget gallery, which is a picture of how things
look and has no machine-readable form, and `completions <shell>` prints a shell
script. Both accept `--json`, say so on stderr where it matters, and exit `0`.

### Failures in JSON

A failing run prints a document too. The human report still goes to stderr,
unchanged, and the exit code is still non-zero — the JSON is additional.

```console
$ pecu wallet balance --address bob@ --json --node https://127.0.0.1:1
{
  "error": {
    "causes": [
      "transport: https://127.0.0.1:1/: Connection Failed: Connection refused"
    ],
    "code": "pecu::node_unreachable",
    "help": "check your connection, or point somewhere else with --node — …",
    "message": "looking up the identity failed against https://127.0.0.1:1"
  }
}
$ echo $?
3
```

`code` is the diagnostic's own `pecu::…` identifier — the same token stderr
prints after `Error:`, not a second naming scheme derived from it. That is the
thing to switch on. `message` and `help` are the sentences the report renders,
and `causes` is the chain underneath the head, which is where the SDK's own
wording lives. All four keys are always present; `code` and `help` are `null`
when the diagnostic carries none, and `causes` is `[]`.

Three commands build a document of their own and then fail: `doctor`, whose
local half is worth having when the node is down; `send`, whose signed `hex`
cannot be recovered afterwards; and `sign`, whose `partial` is what the next
signer has to be handed. Each gets the error object folded into that document
under the same top-level `error` key rather than followed by a second document,
so `.error.code` reads the same on every failing `--json` run and every run
prints exactly one document.

### Exit codes

Deliberately few. There are over a hundred distinct `pecu::…` diagnostic codes,
and a status per diagnostic would be a contract nobody could keep — the code is
the fine-grained discriminator, and it travels in the JSON. What the exit status
answers is the coarser question a script branches on: is this worth retrying?

| Code | Meaning | Retry? |
|---|---|---|
| `0` | It worked | — |
| `1` | The request was understood and the answer was no: a refusal, a missing key, an amount that will not parse, a daemon that answered with an error code | No. Nothing about running it again is different |
| `2` | Usage error — clap could not parse the command line. Printed before anything else runs, so there is no JSON on this path | No |
| `3` | The endpoint did not answer the question: nothing came back, or what came back was not an answer this build can use — a refused connection, a timeout, a proxy's HTML, a method the node will not serve. Nothing happened | Yes, or point `--node` somewhere else |
| `4` | The outcome is genuinely unknown. A broadcast whose bytes may or may not be propagating | **No — check first.** The document carries the `txid` and the signed `hex`; `pecu tx explain <txid>` says whether the chain has it |

`4` exists because `3` has to be safe to retry. A connection that breaks *after*
a `sendrawtransaction` goes out is not a node that was never reached: the
transaction may already be in a mempool, and a blind retry is a second payment.
The SDK reports that case separately and so does this.

`2` is narrower than "the command line was wrong". A flag that names something
`pecu` understands and cannot do is declared and refused here rather than left
unknown to clap, so it exits `1` with a `pecu::…` code and a JSON document —
`unexpected argument '--currency' found` reads as a misspelling and carries
neither. `pecu plan send --currency` and `pecu plan send --from-identity` moved
from `2` to `1` for that reason.

`2` still covers everything clap settles before a command runs: a flag the
parser has never heard of, and a flag it has — these two included — given no
value. `pecu plan send -c` exits `2` with `a value is required for
'--currency'`, because there is nothing yet to refuse by name.

The status and `.error.code` answer different questions, and on one diagnostic
they look like they disagree. `pecu::node_unreachable` is a single diagnostic
over every failed node request, including the ones the node *answered* — a
daemon error code renders under that name and exits `1`, because the request was
understood. The status is the finer discriminator there; the code says which
request failed, not what kind of failure it was.

