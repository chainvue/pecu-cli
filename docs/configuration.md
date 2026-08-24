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
| `--theme auto\|phosphor\|light\|plain` | Phosphor on a TTY, plain when piped; `light` for a light-background terminal (env: `PECU_THEME`). `NO_COLOR` always wins |
| `-v`, `--verbose` | Refused. It never logged anything — see below |

`-v` is the one flag in that table you cannot use. It was scaffolded with the
command tree and wired to nothing: no logging framework is linked, and `-v`,
`-vv` and `-vvv` all produced output byte-identical to no flag at all, on
stdout and on stderr. It is now hidden from every help screen and from the
completion scripts, and passing it is refused by name:

```console
$ pecu wallet balance --key default -v
Error: pecu::verbose_does_nothing

  × -v/--verbose turns up logging this build does not have
  help: nothing in `pecu` logs. … `--explain` is the diagnostic that works …
$ echo $?
1
```

**This is a breaking change.** A run that passed `-v` and exited `0` exits `1`
now. Nobody can have depended on what the flag *did*, because it did nothing;
what breaks is scripts that passed it and were accepted. Deleting it outright
would have broken those the same way and answered worse — clap's `unexpected
argument '-v'` carries no code and no document, and `--verbose` draws `tip: a
similar argument exists: '--version'`, a flag that parses, prints a version
string and exits `0` without doing the work. Refusing it by name says what
happened and points at `--explain`, which does the job `-v` was reaching for.

Two details if you read the exit code or `.error.code` from a script:

- A run that was **already failing** keeps exit `1` and changes its code. `pecu
  wallet balance -v` reported `pecu::no_address` and now reports
  `pecu::verbose_does_nothing`, because the refusal is answered before the
  command runs. A consumer branching on `.error.code` sees that even though the
  status did not move.
- Three command lines never reach the refusal, because clap settles them first.
  `pecu … -v --help` and `pecu … -v --version` still exit `0` with `-v` ignored:
  clap resolves both during parsing, and no command runs. A command line already
  invalid for another reason keeps that answer — `pecu key show -v` is exit `2`
  for the missing `<LABEL>`. Catching these would mean scanning the arguments
  ahead of clap, which cannot tell the flag `-v` from the value `-v` in `pecu
  key show -- -v`.

Under `--json`, stdout carries one JSON document and nothing else. Panels,
notes, progress lines and the `--explain` record are not written there at all —
`--explain` goes to stderr instead — so `pecu … --json | jq` never has to step
over prose, and a consumer that stops reading gets the exit code back rather
than a broken-pipe panic.

Two commands are exceptions, and both are cases where there is no document to
print: `dev ui` renders the widget gallery, which is a picture of how things
look and has no machine-readable form, and `completions <shell>` prints a shell
script. Both accept `--json`, say so on stderr where it matters, and exit `0`.

### Colour on a light terminal

`auto` picks between phosphor and plain on one question — is stdout a terminal —
and it will not pick `light` for you. The phosphor palette is 256-colour, and
every index it uses sits above the sixteen slots a terminal profile can remap,
so a light profile cannot rescue it: the value column lands at 1.10:1 on white,
invisible, while the labels beside it stay legible. A panel that looks complete
and has no figures in it is the worst way for this to fail, so `--theme light`
re-inks all nine roles — every one measured at 4.5:1 or better on white, pinned
by a unit test.

Nothing sniffs the background. `$COLORFGBG` is set by a minority of terminals
and an OSC 11 query means putting the tty into raw mode and waiting on a reply,
which is a new way for a spend confirmation to fail. Your terminal's background
is a fact about you, so you state it:

```sh
export PECU_THEME=light        # or --theme light, per command
pecu wallet balance --key default
```

`NO_COLOR=1` is the other way out and needs no flag: it keeps the frames and
drops every colour, so the output stays the shape of the tool. `--theme plain`
drops the frames too.

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
from `2` to `1` for that reason, and `-v`/`--verbose` moved from `0` to `1` for
the same one — a flag that cannot be honoured is worth a name, a code and a
document whether it was refused yesterday or accepted by mistake. See the note
under [Global flags](#global-flags) for the cases `-v` does not change.

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

