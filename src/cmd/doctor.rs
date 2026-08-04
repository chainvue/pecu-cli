//! `pecu doctor` — is this thing plugged in?
//!
//! Three questions, in the order they go wrong: where are my files, what was
//! this binary built from, and is the node answering. The local half is printed
//! even when the node is unreachable, because "my setting is being ignored" and
//! "the node is down" are different problems and the output should tell them
//! apart.

use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde_json::json;
use verus_sdk::network::{ChainInfo, ChainReader, RpcError};

use crate::cli::sdk_rev;
use crate::config::{tildify, Settings};
use crate::node::{self, NodeError};
use crate::ui::{fmt, Panel, Text, Ui};

/// A tip older than this is worth pointing at: VRSCTEST aims for a block a
/// minute, so ten of them missing means the node is stuck or syncing.
const STALE_AFTER: Duration = Duration::from_secs(600);

/// Cells the label column and its gutter take up, so a path can be shortened to
/// what is left. `verus-sdk` is the longest label in this report.
const LABEL_COLUMN: usize = 12;

/// What the node told us, or why it did not.
struct NodeReport {
    chain: ChainInfo,
    latency: Duration,
    /// `None` when the endpoint refuses `getrawmempool` — public nodes sit
    /// behind a method allowlist, and that is not a failure of the wallet.
    mempool: Option<usize>,
    /// Unix seconds of the tip block, when the node would say.
    tip_time: Option<u64>,
}

pub fn run(ui: &Ui, settings: &Settings) -> miette::Result<()> {
    let profile = &settings.profile;
    let report = probe(&profile.node);

    if ui.is_json() {
        emit_json(settings, &report);
    } else {
        render(ui, settings, &report);
    }

    // The local half is worth printing either way, but a doctor that cannot
    // reach the node has not passed.
    match report {
        Ok(_) => Ok(()),
        Err(error) => Err(error.into()),
    }
}

fn probe(url: &str) -> Result<NodeReport, NodeError> {
    let node = node::connect(url)?;

    let started = Instant::now();
    let chain = node
        .chain_info()
        .map_err(|source| NodeError::request("asking for chain info", url, source))?;
    let latency = started.elapsed();

    // Both of these are extras. A node that answered `getinfo` and refuses the
    // rest is still a working node for most of what this tool does, so neither
    // is allowed to turn a healthy report into a failure.
    let mempool = match node.mempool() {
        Ok(ids) => Some(ids.len()),
        Err(RpcError::MethodUnavailable { .. }) | Err(_) => None,
    };
    let tip_time = node
        .block(&chain.blocks.to_string())
        .ok()
        .and_then(|block| block.get("time").and_then(serde_json::Value::as_u64));

    Ok(NodeReport {
        chain,
        latency,
        mempool,
        tip_time,
    })
}

/// Cargo features this binary was compiled with. `network` is unconditional.
fn features() -> Vec<&'static str> {
    let mut features = vec!["network"];
    if cfg!(feature = "shielded") {
        features.push("shielded");
    }
    if cfg!(feature = "prover") {
        features.push("prover");
    }
    if cfg!(feature = "light") {
        features.push("light");
    }
    features
}

fn seconds_since(unix: u64) -> Option<u64> {
    let now = SystemTime::now().duration_since(UNIX_EPOCH).ok()?.as_secs();
    now.checked_sub(unix)
}

fn render(ui: &Ui, settings: &Settings, report: &Result<NodeReport, NodeError>) {
    let theme = &ui.theme;
    let palette = theme.palette;
    let glyphs = theme.glyphs;
    let profile = &settings.profile;
    let paths = &settings.paths;

    ui.banner(&[
        concat!("verus wallet · v", env!("CARGO_PKG_VERSION")),
        "doctor",
    ]);
    ui.blank();

    let spend = if profile.allow_spend {
        Text::of(glyphs.ok, palette.ok)
            .space()
            .push("spending allowed", palette.value)
    } else {
        Text::of(glyphs.warn, palette.warn)
            .space()
            .push("read-only — spending is off for this profile", palette.warn)
    };

    // Paths are the one thing here that can be arbitrarily long, and a line
    // wider than the panel breaks the frame. Shortened from the middle: the tail
    // of a path is the part that identifies it.
    let key_count = paths.key_count();
    let keys_suffix = format!("({})", fmt::plural(key_count, "key", "keys"));
    let path_budget = |reserved: usize| theme.width.saturating_sub(LABEL_COLUMN + reserved);
    let fit = |path: &std::path::Path, reserved: usize| {
        fmt::fit(&tildify(path), path_budget(reserved), glyphs.ellipsis)
    };

    let mut panel = Panel::new("LOCAL")
        .row("profile", Text::of(&profile.name, palette.accent))
        .row("node", Text::of(&profile.node, palette.value))
        .row("currency", Text::of(&profile.currency, palette.value))
        .row("spending", spend)
        .row(
            "config",
            Text::of(fit(&paths.config_file(), 0), palette.value),
        )
        .row(
            "keys",
            Text::of(fit(&paths.keys_dir(), keys_suffix.len() + 1), palette.value)
                .space()
                .push(keys_suffix, palette.muted),
        )
        .section("BUILD")
        .row("pecu", Text::of(env!("CARGO_PKG_VERSION"), palette.value))
        .row("verus-sdk", Text::of(&sdk_rev!()[..7], palette.value))
        .row("features", Text::of(features().join(" "), palette.value));

    panel = match report {
        Ok(report) => node_section(panel.section("NODE"), report, ui),
        Err(error) => panel.section("NODE").row(
            "status",
            Text::of(glyphs.danger, palette.danger)
                .space()
                .push(error.to_string(), palette.danger),
        ),
    };

    if !settings.config_exists {
        panel = panel.note(Text::of(
            "no config file yet — running on the built-in profiles",
            palette.muted,
        ));
    }

    ui.panel(&panel);
}

/// The node half, kept out of [`render`] so the happy path reads in order.
fn node_section(panel: Panel, report: &NodeReport, ui: &Ui) -> Panel {
    let palette = ui.theme.palette;
    let glyphs = ui.theme.glyphs;
    let chain = &report.chain;

    let behind = chain.longest_chain.saturating_sub(chain.blocks);
    let sync = if behind == 0 {
        Text::of(glyphs.ok, palette.ok)
            .space()
            .push("in sync", palette.value)
    } else {
        Text::of(glyphs.warn, palette.warn).space().push(
            format!("{} behind the longest chain", fmt::height(behind.into())),
            palette.warn,
        )
    };

    let mut tip = Text::of(glyphs.bullet, palette.accent)
        .space()
        .push(fmt::height(chain.blocks.into()), palette.accent);
    if let Some(age) = report.tip_time.and_then(seconds_since) {
        let stale = age > STALE_AFTER.as_secs();
        tip = tip.push("   mined ", palette.muted).push(
            format!("{} ago", fmt::duration(age)),
            if stale { palette.warn } else { palette.muted },
        );
    }

    panel
        .row("chain", Text::of(&chain.name, palette.accent))
        .row("daemon", Text::of(&chain.version, palette.value))
        .row("tip", tip)
        .row("sync", sync)
        .row(
            "mempool",
            match report.mempool {
                Some(count) => Text::of(
                    fmt::plural(count, "transaction", "transactions"),
                    palette.value,
                ),
                None => Text::of("not offered by this endpoint", palette.muted),
            },
        )
        .row(
            "latency",
            Text::of(format!("{} ms", report.latency.as_millis()), palette.value),
        )
}

fn emit_json(settings: &Settings, report: &Result<NodeReport, NodeError>) {
    let profile = &settings.profile;
    let paths = &settings.paths;

    let node = match report {
        Ok(report) => json!({
            "reachable": true,
            "chain": report.chain.name,
            "chain_id": report.chain.chain_id,
            "daemon_version": report.chain.version,
            "blocks": report.chain.blocks,
            "longest_chain": report.chain.longest_chain,
            "behind": report.chain.longest_chain.saturating_sub(report.chain.blocks),
            "tip_time": report.tip_time,
            "tip_age_seconds": report.tip_time.and_then(seconds_since),
            "mempool": report.mempool,
            "latency_ms": report.latency.as_millis(),
        }),
        Err(error) => json!({ "reachable": false, "error": error.to_string() }),
    };

    let document = json!({
        "profile": {
            "name": profile.name,
            "node": profile.node,
            "explorer": profile.explorer,
            "currency": profile.currency,
            "allow_spend": profile.allow_spend,
        },
        "paths": {
            "root": paths.root(),
            "config": paths.config_file(),
            "config_exists": settings.config_exists,
            "keys": paths.keys_dir(),
            "key_count": paths.key_count(),
        },
        "build": {
            "version": env!("CARGO_PKG_VERSION"),
            "sdk_rev": sdk_rev!(),
            "features": features(),
        },
        "node": node,
    });

    println!(
        "{}",
        serde_json::to_string_pretty(&document).expect("the report is plain data")
    );
}
