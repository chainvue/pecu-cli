# Design notes


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

