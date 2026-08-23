//! `pecu dev ui` — every widget in the kit, on one screen.
//!
//! Two jobs. It is what the snapshot tests read, so a change to the renderer
//! shows up as a diff rather than as a surprise in some other command; and it is
//! how the look is judged by eye without needing funds or a node.
//!
//! The numbers are invented, but they go through the same `verus_sdk::money::Amount`
//! the real commands use — a formatting bug shows up here first.

use verus_sdk::money::Amount;

use crate::ui::{fmt, Align, Column, Panel, Table, Text, Ui};

pub fn gallery(ui: &Ui) {
    let theme = &ui.theme;
    let palette = theme.palette;
    let glyphs = theme.glyphs;
    let ellipsis = glyphs.ellipsis;

    // No version in the subtitle: this output is snapshotted, and a version
    // bump should not show up as a UI diff.
    ui.banner(&["verus wallet", "widget gallery"]);
    ui.blank();

    // ── the balance panel, which is the shape most commands borrow ──────────
    let mut balances = Table::headerless([Align::Left, Align::Right, Align::Left, Align::Left]);
    balances.push(vec![
        Text::of("SPENDABLE", palette.label),
        Text::of(
            fmt::amount(Amount::from_sat(31_250_000_000)),
            palette.accent,
        ),
        Text::of("VRSCTEST", palette.muted),
        Text::of(
            format!("({})", fmt::plural(4, "utxo", "utxos")),
            palette.muted,
        ),
    ]);
    balances.push(vec![
        // "WITHHELD", matching `wallet balance`. The node routes any output it
        // calls unspendable into this bucket, not only immature coinbase.
        Text::of("WITHHELD", palette.label),
        Text::of(fmt::amount(Amount::from_sat(600_000_000)), palette.value),
        Text::of("VRSCTEST", palette.muted),
        Text::of("(1 coinbase)", palette.muted),
    ]);

    // Movements, not holdings — the one table here whose numbers are signed.
    let mut pending = Table::headerless([Align::Left, Align::Right, Align::Left, Align::Left]);
    pending.push(vec![
        Text::of("INCOMING", palette.label),
        Text::of(fmt::amount(Amount::from_sat(25_000_000)), palette.ok),
        Text::of("VRSCTEST", palette.muted),
    ]);
    pending.push(vec![
        Text::of("OUTGOING", palette.label),
        Text::of(fmt::amount(Amount::from_sat(100_010_000)), palette.warn),
        Text::of("VRSCTEST", palette.muted),
    ]);
    pending.push(vec![
        Text::of("NET", palette.label),
        Text::of(fmt::signed(-75_010_000), palette.accent),
        Text::of("VRSCTEST", palette.muted),
    ]);

    let mut tokens = Table::headerless([Align::Right, Align::Left, Align::Left]);
    tokens.push(vec![
        Text::of(
            fmt::amount(Amount::from_sat(120_000_000_000)),
            palette.value,
        ),
        Text::of("pecu@", palette.accent),
        Text::of(
            fmt::address("iJhCe4Ap7ZfGqNzT1rXkV2Bd9Ls6Wy8Kd", ellipsis),
            palette.muted,
        ),
    ]);
    tokens.push(vec![
        Text::of(fmt::amount(Amount::from_sat(50_000_000)), palette.value),
        Text::of("bridge@", palette.accent),
        Text::of(
            fmt::address("i3f7QwErTy5UiOp8AsDfGhJkLzXcV2Lm", ellipsis),
            palette.muted,
        ),
    ]);

    let wallet = Panel::new("WALLET")
        .row(
            "addr",
            Text::of(
                fmt::address("RXyz9k2mPqWeRtYuIoAsDfGhJkL7Qa4", ellipsis),
                palette.value,
            ),
        )
        .row(
            "tip",
            Text::of(glyphs.bullet, palette.accent)
                .space()
                .push(fmt::height(3_481_207), palette.accent)
                .push("        ", palette.muted)
                .push("node", palette.label)
                .space()
                .push(glyphs.ok, palette.ok)
                .space()
                .push("api.verustest.net", palette.value),
        )
        .rule()
        .table(balances)
        .section("TOKENS")
        .table(tokens)
        .section("PENDING")
        .row("in flight", Text::of("2 transactions", palette.value))
        .table(pending)
        .note(Text::of(
            "pending: in this node's mempool, not in any block, and excluded from the totals \
             above",
            palette.muted,
        ));
    ui.panel(&wallet);
    ui.blank();

    // ── the identity panel, and the two timelock forms ──────────────────────
    //
    // Both forms are here because they are the pair most easily rendered
    // wrongly: one field means an absolute height or a relative delay
    // depending on a flag, and the tense has to follow. A countdown that has
    // elapsed keeps its height forever, so "unlocked at" is as ordinary a
    // state as "locked for N more".
    let identity = Panel::new("IDENTITY")
        .row("name", Text::of("pecu.VRSCTEST@", palette.accent))
        .row(
            "last change",
            Text::of("block 1,177,214", palette.value)
                .push("   ", palette.muted)
                .push("2026-08-05 11:50 UTC", palette.value)
                .push("  (1h 01m ago)", palette.muted),
        )
        .section("TIMELOCK")
        .row("unlock delay", Text::of("10 blocks", palette.value))
        .row(
            "state",
            Text::of(glyphs.warn, palette.warn)
                .space()
                .push("locked, and no unlock requested", palette.warn),
        )
        .note(Text::of(
            "the delay does not start until an unlock is asked for",
            palette.muted,
        ));
    ui.panel(&identity);
    ui.blank();

    let unlocked = Panel::new("IDENTITY")
        .row("name", Text::of("dude.VRSCTEST@", palette.accent))
        .section("TIMELOCK")
        .row("unlocked at", Text::of("block 1,177,407", palette.value))
        .row(
            "state",
            Text::of(glyphs.ok, palette.ok)
                .space()
                .push("unlocked", palette.ok),
        )
        .note(Text::of(
            "the height stays on the identity after a countdown finishes — a leftover, not a \
             pending unlock",
            palette.muted,
        ));
    ui.panel(&unlocked);
    ui.blank();

    // ── a table with headers, the shape `wallet utxos` will use ─────────────
    let mut utxos = Table::new(vec![
        Column::left("outpoint"),
        Column::right("amount"),
        Column::right("conf"),
        Column::left("status"),
    ]);
    utxos.push(vec![
        Text::of(
            format!(
                "{}:0",
                fmt::hash("9f2c1ab4de77605318bbcafe0021d4e9", ellipsis)
            ),
            palette.value,
        ),
        Text::of(fmt::sats(25_000_000_000), palette.accent),
        Text::of(fmt::height(1_204), palette.muted),
        Text::of(format!("{} spendable", glyphs.ok), palette.ok),
    ]);
    utxos.push(vec![
        Text::of(
            format!(
                "{}:1",
                fmt::hash("41bd8e0092ff7c33ae5510cbb0d7a2f6", ellipsis)
            ),
            palette.value,
        ),
        Text::of(fmt::sats(600_000_000), palette.value),
        Text::of(fmt::height(37), palette.muted),
        Text::of(format!("{} immature", glyphs.warn), palette.warn),
    ]);
    ui.panel(&Panel::new("UTXOS").table(utxos));
    ui.blank();

    // ── free-form lines, the shape `tx explain` will use ────────────────────
    let outputs = Panel::new("OUTPUTS")
        .line(
            Text::of("#0", palette.muted)
                .space()
                .push(fmt::sats(10_000_000), palette.accent)
                .space()
                .push(glyphs.arrow, palette.muted)
                .space()
                .push("RXyz9k2mP", palette.value)
                .push(ellipsis, palette.muted)
                .push("7Qa4", palette.value),
        )
        .line(
            Text::of("#1", palette.muted)
                .space()
                .push(fmt::sats(0), palette.muted)
                .space()
                .push("the VerusID pecu@ itself — 1-of-1 signature", palette.value),
        )
        .line(
            Text::of("#2", palette.muted)
                .space()
                .push(fmt::sats(0), palette.muted)
                .space()
                .push("eval 2 — MAY HOLD CURRENCY", palette.danger),
        )
        .blank()
        .line(Text::of(
            "a blank line inside the frame, for grouping",
            palette.muted,
        ));
    ui.panel(&outputs);
    ui.blank();

    ui.line(
        Text::of("status lines", palette.label)
            .space()
            .push(glyphs.arrow, palette.muted)
            .space()
            .push(
                "outside any frame, so they compose with anything",
                palette.muted,
            ),
    );

    // ── status lines, which live outside any frame ──────────────────────────
    ui.ok("broadcast accepted by the node");
    ui.warn("expiry is 0 — this transaction stays minable forever");
    ui.fail("wrong passphrase");
    ui.note("keys live in ~/.config/verus-pecu/keys");
    ui.blank();

    // ── formatting, spelled out ─────────────────────────────────────────────
    let formatting = Panel::new("FORMATTING")
        .row(
            "amount",
            Text::of(fmt::sats(1), palette.value)
                .push("   one satoshi, eight places, never a float", palette.muted),
        )
        .row(
            "height",
            Text::of(fmt::height(3_481_207), palette.value).push(
                "       grouped; amounts deliberately are not",
                palette.muted,
            ),
        )
        .row(
            "address",
            Text::of(
                fmt::address("RXyz9k2mPqWeRtYuIoAsDfGhJkL7Qa4", ellipsis),
                palette.value,
            )
            .push(
                "      both ends kept — both ends get compared",
                palette.muted,
            ),
        )
        // The only value the renderer is allowed to shorten, and the only one
        // where the tail is what identifies it.
        .path(
            "path",
            std::path::Path::new(
                "/var/lib/somewhere/rather/deeply/nested/verus-pecu/keys/cold-storage.json",
            ),
        );
    ui.panel(&formatting);
}
