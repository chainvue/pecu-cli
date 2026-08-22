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

