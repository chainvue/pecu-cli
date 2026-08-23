//! `pecu doctor` — is this thing plugged in?
//!
//! Three questions, in the order they go wrong: where are my files, what was
//! this binary built from, and is the node answering. The local half is printed
//! even when the node is unreachable, because "my setting is being ignored" and
//! "the node is down" are different problems and the output should tell them
//! apart.

use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde_json::json;
use verus_sdk::currency::CurrencyId;
use verus_sdk::network::{key_address, ChainInfo, ChainReader, ContentValue, RpcError};
use verus_sdk::verus_keys::{hash160, Address, AddressKind};
use verus_sdk::verus_wire::hash::sha256d;

use crate::cli::sdk_rev;
use crate::config::Settings;
use crate::node::{self, Node, NodeError};
use crate::ui::{fmt, Panel, Text, Ui};

/// A tip older than this is worth pointing at: VRSCTEST aims for a block a
/// minute, so ten of them missing means the node is stuck or syncing.
const STALE_AFTER: Duration = Duration::from_secs(600);

/// What the node told us, or why it did not.
struct NodeReport {
    chain: ChainInfo,
    latency: Duration,
    /// `None` when the endpoint refuses `getrawmempool` — public nodes sit
    /// behind a method allowlist, and that is not a failure of the wallet.
    mempool: Option<usize>,
    /// Unix seconds of the tip block, when the node would say.
    tip_time: Option<u64>,
    /// The chain's DeFi kill switch, when its notification oracle publishes
    /// one. `None` is both the ordinary case and what a node that will not
    /// serve `getidentity` gets — a diagnostic that guessed would be worse
    /// than one that stayed quiet.
    defi: Option<DefiSwitch>,
}

/// A published `disabledefi` upgrade, and whether the tip has reached it.
struct DefiSwitch {
    /// `upgradeBlockHeight` — the height the switch takes effect at.
    block: u64,
    /// Whether the tip is at or past it. A future height is a warning, not a
    /// refusal: nothing is refused until the chain gets there.
    active: bool,
    /// The upgrade this descriptor named, kept so the JSON report can print
    /// the key alongside the name that derived it and a reader can re-run
    /// `getvdxfid` against both.
    upgrade_id: [u8; 20],
}

pub fn run(ui: &Ui, settings: &Settings) -> miette::Result<()> {
    let profile = &settings.profile;
    let report = probe(profile);

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

fn probe(profile: &crate::config::Profile) -> Result<NodeReport, NodeError> {
    let url = &profile.node;
    let node = node::connect(profile)?;

    let started = Instant::now();
    let chain = node
        .chain_info()
        .map_err(|source| NodeError::request("asking for chain info", url, source))?;
    let latency = started.elapsed();

    // All three of these are extras. A node that answered `getinfo` and refuses
    // the rest is still a working node for most of what this tool does, so none
    // of them is allowed to turn a healthy report into a failure.
    let mempool = match node.mempool() {
        Ok(ids) => Some(ids.len()),
        Err(RpcError::MethodUnavailable { .. }) | Err(_) => None,
    };
    let tip_time = node
        .block(&chain.blocks.to_string())
        .ok()
        .and_then(|block| block.get("time").and_then(serde_json::Value::as_u64));
    // Read after `latency` is measured, so that figure keeps meaning `getinfo`
    // alone rather than however many calls this function grows.
    let defi = defi_switch(&node, &chain);

    Ok(NodeReport {
        chain,
        latency,
        mempool,
        tip_time,
        defi,
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

/// The VDXF name a chain's upgrade signals are published under.
///
/// A name, not a key. `verus getvdxfid "vrsc::system.upgradedata"
/// '{"vdxfkey":"<chain id>"}'` re-derives what [`upgrade_data_key`] computes,
/// on any chain. A checked-in `iH51dFy7…` would be a constant nobody could
/// re-derive, and would be wrong on every chain but VRSCTEST — the key is
/// combined with the chain's own id by construction.
const UPGRADE_DATA: &str = "vrsc::system.upgradedata";

/// `CConnectedChains::DisableDeFiKey()` — the signal that refuses every
/// currency launch and every conversion while it is in effect.
///
/// Unlike the key above this one is *not* chain-specific: it is a plain
/// `vrsc::`-namespaced data key, and derives to the same twenty bytes on VRSC
/// and VRSCTEST alike.
const DISABLE_DEFI: &str = "vrsc::system.upgradedata.disabledefi";

/// The only `CUpgradeDescriptor` layout this build knows how to read.
const DESCRIPTOR_VERSION: u64 = 1;

/// One decoded `CUpgradeDescriptor` — the two fields the diagnostic uses.
struct UpgradeDescriptor {
    /// Which signal this is: `disabledefi`, or one of its siblings.
    upgrade_id: [u8; 20],
    /// `upgradeBlockHeight` — the height the signal takes effect at.
    block: u64,
}

/// The chain's own currency id, from the `i` address `getinfo` reports.
///
/// `None` rather than a panic: this is parsing something a remote node said,
/// and an endpoint that answers `getinfo` with a `chain_id` that is not an
/// i-address should cost the report one row, not the whole run.
fn chain_currency_id(chain: &ChainInfo) -> Option<CurrencyId> {
    let address: Address = chain.chain_id.parse().ok()?;
    (address.kind() == AddressKind::Identity).then(|| CurrencyId::from_bytes(address.hash()))
}

/// `CCrossChainRPCData::GetConditionID` — one key combined with a system id.
///
/// The system id goes **first**. Reversed, this derives a key nobody publishes,
/// the row silently never appears and nothing fails; that is why the unit tests
/// assert the daemon's own `getvdxfid` answers on two chains rather than
/// round-tripping our own output.
fn condition_id(system: [u8; 20], condition: [u8; 20]) -> [u8; 20] {
    let mut joined = [0u8; 40];
    joined[..20].copy_from_slice(&system);
    joined[20..].copy_from_slice(&condition);
    hash160(&sha256d(&joined))
}

/// The `contentmultimap` key this chain's upgrade descriptors live under.
///
/// `CConnectedChains::UpgradeDataKey` (`pbaas.h:1301`): the id of
/// [`UPGRADE_DATA`] combined with the chain's own id, which is why the same
/// name lands on a different key on every chain.
fn upgrade_data_key(chain: &ChainInfo) -> Option<[u8; 20]> {
    let id = chain_currency_id(chain)?;
    let name = verus_sdk::vdxf::qualified_key(UPGRADE_DATA, &chain.name, id).ok()?;
    Some(condition_id(id.to_bytes(), name))
}

/// Whether this chain has switched DeFi off, read from its notification oracle.
///
/// The oracle is a VerusID the daemon watches for signals (`-notificationoracle`,
/// `init.cpp:566`); for a chain it defaults to the chain's own id, which is why
/// this lives on `VRSCTEST@` and not in any currency definition. The chain id is
/// used for both the fetch and the key combination, so the two cannot disagree —
/// a chain run with a *different* oracle is not read at all, and that is honest
/// silence rather than a wrong answer.
///
/// Every failure yields `None`. A node that answered `getinfo` and refuses
/// `getidentity` still gives a passing `doctor`, and nothing anywhere in this
/// program is gated on the reading: a chain can re-enable DeFi between the
/// check and the broadcast, and a stale "disabled" that refused a legitimate
/// launch would be worse than saying nothing.
fn defi_switch(node: &Node, chain: &ChainInfo) -> Option<DefiSwitch> {
    let id = chain_currency_id(chain)?;
    let key = upgrade_data_key(chain)?;
    let disable_defi = verus_sdk::vdxf::qualified_key(DISABLE_DEFI, &chain.name, id).ok()?;
    let values = verus_sdk::network::read(node, &chain.chain_id, key).ok()?;
    let block = defi_block(&values, disable_defi)?;
    Some(DefiSwitch {
        block,
        active: u64::from(chain.blocks) >= block,
        upgrade_id: disable_defi,
    })
}

/// The DeFi switch among the values published under the upgrade key.
///
/// **Every** value is decoded, not just the first: the sibling signals
/// (`disablepbaascrosschain`, `disableearnednotarizations`, and the rest,
/// `pbaas.h:1308-1460`) share this one key as separate values, so taking the
/// first would miss DeFi on any chain that publishes another signal ahead of
/// it. Pure, so the selection is tested with no node.
fn defi_block(values: &[ContentValue], disable_defi: [u8; 20]) -> Option<u64> {
    values
        .iter()
        // `None` for a structured value: the daemon renders keys it recognises
        // as objects, and there are no bytes in the reply to decode.
        .filter_map(ContentValue::as_bytes)
        .filter_map(decode_upgrade_descriptor)
        .find(|descriptor| descriptor.upgrade_id == disable_defi)
        .map(|descriptor| descriptor.block)
}

/// Satoshi's `ReadVarInt` — base-128, big-endian, with the continuation bit
/// meaning "and add one", so every value has exactly one encoding.
///
/// Not `CompactSize`: the two agree below 128 and disagree on everything above
/// it, so reading this one with the other decodes the live descriptors into
/// confident nonsense.
fn read_varint(bytes: &[u8], at: &mut usize) -> Option<u64> {
    let mut value: u64 = 0;
    loop {
        let byte = *bytes.get(*at)?;
        *at += 1;
        value = value
            .checked_mul(128)?
            .checked_add(u64::from(byte & 0x7f))?;
        if byte & 0x80 == 0 {
            return Some(value);
        }
        value = value.checked_add(1)?;
    }
}

/// One `CUpgradeDescriptor` (`pbaas.h:81-99`), or `None` if these are not
/// those bytes:
///
/// ```text
/// VARINT version · VARINT minDaemonVersion · uint160 upgradeID
///                · VARINT upgradeBlockHeight · VARINT upgradeTargetTime
/// ```
///
/// Three refusals, each deliberate. A `version` this build does not know is
/// refused rather than read against the version 1 layout, because that prints a
/// confidently *wrong* height. Trailing bytes are refused, because consuming
/// the value exactly is what proves the layout was read correctly. And
/// `upgradeTargetTime` is parsed but not interpreted: consensus can also
/// activate on a wall-clock target, mapping one onto a height needs block times
/// this command does not fetch, and every descriptor on VRSC and VRSCTEST today
/// carries zero. A chain that activated purely on time would go unreported.
fn decode_upgrade_descriptor(bytes: &[u8]) -> Option<UpgradeDescriptor> {
    let mut at = 0usize;
    if read_varint(bytes, &mut at)? != DESCRIPTOR_VERSION {
        return None;
    }
    let _min_daemon_version = read_varint(bytes, &mut at)?;
    let upgrade_id: [u8; 20] = bytes.get(at..at.checked_add(20)?)?.try_into().ok()?;
    at += 20;
    let block = read_varint(bytes, &mut at)?;
    let _upgrade_target_time = read_varint(bytes, &mut at)?;
    if at != bytes.len() {
        return None;
    }
    Some(UpgradeDescriptor { upgrade_id, block })
}

/// The `defi` row: a refusal that is already in force, or one that is coming.
fn defi_row(defi: &DefiSwitch, ui: &Ui) -> Text {
    let palette = ui.theme.palette;
    let glyphs = ui.theme.glyphs;
    if defi.active {
        Text::of(glyphs.danger, palette.danger).space().push(
            format!(
                "disabled since block {} — launches and conversions are refused",
                fmt::height(defi.block)
            ),
            palette.danger,
        )
    } else {
        Text::of(glyphs.warn, palette.warn).space().push(
            format!(
                "switching off at block {} — launches and conversions still work",
                fmt::height(defi.block)
            ),
            palette.warn,
        )
    }
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

    let key_count = paths.key_count();
    let mut panel = Panel::new("LOCAL")
        .row("profile", Text::of(&profile.name, palette.accent))
        .row("node", Text::of(&profile.node, palette.value))
        .row("currency", Text::of(&profile.currency, palette.value))
        .row("spending", spend)
        .path("config", &paths.config_file())
        .path("keys", &paths.keys_dir())
        .row(
            "stored keys",
            Text::of(fmt::plural(key_count, "key", "keys"), palette.value),
        )
        .row(
            "reply cap",
            Text::of(format!("{} MiB", profile.max_response_mb), palette.value),
        )
        .row(
            "timeout",
            Text::of(format!("{} s", profile.timeout_secs), palette.value),
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

    let mut panel = panel
        .row("chain", Text::of(&chain.name, palette.accent))
        .row("daemon", Text::of(&chain.version, palette.value))
        .row("tip", tip)
        .row("sync", sync);
    // No row at all when nothing is published, which is the ordinary case on
    // every chain that has not switched anything off.
    if let Some(defi) = &report.defi {
        panel = panel.row("defi", defi_row(defi, ui));
    }

    panel
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
            // The name as well as the key it derived to, so a reader can put
            // both through `getvdxfid` and check us. `null` when the chain's
            // oracle publishes no such signal, or would not be asked.
            "defi": report.defi.as_ref().map(|defi| json!({
                "disabled": defi.active,
                "block": defi.block,
                "upgrade_id": key_address(defi.upgrade_id),
                "upgrade_name": DISABLE_DEFI,
            })),
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
            "max_response_mb": profile.max_response_mb,
            "timeout_secs": profile.timeout_secs,
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

    // `doctor` is one of the three commands that print a document *and* fail.
    // The top-level `error` a script switches on is not added here: the
    // document is handed over rather than printed, and `failure::finish` folds
    // the failure into it on the way out. `node.error` above stays what it was
    // — a sentence about reachability, one level down.
    crate::failure::document(&document);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::Theme as ThemeFlag;

    /// `getidentity VRSCTEST@` → `contentmultimap`, the sole value under
    /// `iH51dFy7vF3LTRuVQvCTVu6QSbYfhTjek8`, captured 2026-08-22.
    const LIVE_VRSCTEST: &str = "018787a1035a9bd4179a3e0538ba9f90be7f231b69b0b588bac7b83800";

    /// The same, from `getidentity VRSC@` under
    /// `iSJ38vYX7qoCtotc9wBHb1vZdR3oTgoHCX`. One byte longer, and a different
    /// `minDaemonVersion`, which is what makes it a second sample rather than
    /// a copy.
    const LIVE_VRSC: &str = "018787a1025a9bd4179a3e0538ba9f90be7f231b69b0b588ba80fd8a5b00";

    fn bytes(hex: &str) -> Vec<u8> {
        hex::decode(hex).expect("the captured value is hex")
    }

    /// Enough of a [`ChainInfo`] to derive keys from. The other fields play no
    /// part in the derivation.
    fn chain_info(name: &str, chain_id: &str) -> ChainInfo {
        ChainInfo {
            name: name.to_string(),
            chain_id: chain_id.to_string(),
            blocks: 0,
            longest_chain: 0,
            version: String::new(),
        }
    }

    fn vrsctest() -> ChainInfo {
        chain_info("VRSCTEST", "iJhCezBExJHvtyH3fGhNnt2NhU4Ztkf2yq")
    }

    fn vrsc() -> ChainInfo {
        chain_info("VRSC", "i5w5MuNik5NtLcYmNzcvaoixooEebB6MGV")
    }

    fn disable_defi(chain: &ChainInfo) -> [u8; 20] {
        let id = chain_currency_id(chain).expect("the chain id is an i-address");
        verus_sdk::vdxf::qualified_key(DISABLE_DEFI, &chain.name, id).expect("a derivable name")
    }

    /// A well-formed descriptor for `upgrade_id` at `block`, encoded the way
    /// the daemon encodes one, so the negative tests are run against bytes the
    /// decoder would otherwise accept.
    fn descriptor(upgrade_id: [u8; 20], block: u64) -> Vec<u8> {
        fn varint(mut value: u64, out: &mut Vec<u8>) {
            let mut tmp = vec![u8::try_from(value & 0x7f).expect("seven bits")];
            while value >= 128 {
                value = value / 128 - 1;
                tmp.push(u8::try_from(value & 0x7f).expect("seven bits") | 0x80);
            }
            tmp.reverse();
            out.extend_from_slice(&tmp);
        }
        let mut out = Vec::new();
        varint(DESCRIPTOR_VERSION, &mut out);
        varint(16_912_643, &mut out);
        out.extend_from_slice(&upgrade_id);
        varint(block, &mut out);
        varint(0, &mut out);
        out
    }

    #[test]
    fn the_upgrade_data_key_is_the_name_combined_with_the_chain_id() {
        let chain = vrsctest();
        let key = upgrade_data_key(&chain).expect("a derivable key");
        // `verus -chain=VRSCTEST getvdxfid "vrsc::system.upgradedata"
        //  '{"vdxfkey":"iJhCezBExJHvtyH3fGhNnt2NhU4Ztkf2yq"}'`
        assert_eq!(key_address(key), "iH51dFy7vF3LTRuVQvCTVu6QSbYfhTjek8");
        // And not the bare name's key, which is what looking the name up
        // without the combining id gives and which is in nobody's multimap.
        // This is the assertion that fails if `condition_id` ever loses its
        // combination or reverses its operands.
        assert_ne!(key_address(key), "i3v5LmzfeZ9vE8FCUQrDytk3bntXyCrpoX");
    }

    #[test]
    fn the_same_derivation_finds_mainnets_key_too() {
        let key = upgrade_data_key(&vrsc()).expect("a derivable key");
        // The key `getidentity VRSC@` actually publishes under. Two chains and
        // two different answers out of one derivation is what proves the key is
        // combined rather than a constant somebody could check in.
        assert_eq!(key_address(key), "iSJ38vYX7qoCtotc9wBHb1vZdR3oTgoHCX");
    }

    #[test]
    fn the_disable_defi_key_is_the_same_on_every_chain() {
        let testnet = disable_defi(&vrsctest());
        let mainnet = disable_defi(&vrsc());
        // The asymmetry the comments claim: the *content* key is chain-specific,
        // the *upgrade id* inside the value is not.
        assert_eq!(testnet, mainnet);
        assert_eq!(key_address(testnet), "iBjcvVdXQ3UMpF57DRLRvw6W75pdyWgPmw");
    }

    #[test]
    fn the_live_vrsctest_descriptor_decodes_to_the_defi_switch_at_1_187_000() {
        let decoded = decode_upgrade_descriptor(&bytes(LIVE_VRSCTEST)).expect("a descriptor");
        assert_eq!(decoded.block, 1_187_000);
        assert_eq!(decoded.upgrade_id, disable_defi(&vrsctest()));
    }

    #[test]
    fn the_live_vrsc_descriptor_carries_the_same_switch_at_a_different_height() {
        let decoded = decode_upgrade_descriptor(&bytes(LIVE_VRSC)).expect("a descriptor");
        assert_eq!(decoded.block, 4_163_035);
        assert_eq!(decoded.upgrade_id, disable_defi(&vrsc()));
    }

    #[test]
    fn a_descriptor_with_bytes_left_over_is_refused_rather_than_half_read() {
        let mut value = bytes(LIVE_VRSCTEST);
        value.push(0);
        assert!(decode_upgrade_descriptor(&value).is_none());
    }

    #[test]
    fn a_truncated_descriptor_is_refused() {
        let value = bytes(LIVE_VRSCTEST);
        for cut in 0..value.len() {
            assert!(
                decode_upgrade_descriptor(&value[..cut]).is_none(),
                "{cut} bytes decoded as a whole descriptor"
            );
        }
    }

    #[test]
    fn a_descriptor_version_this_build_does_not_know_is_refused_rather_than_guessed() {
        let mut value = bytes(LIVE_VRSCTEST);
        // Reading a version 2 body against the version 1 layout would print a
        // confidently wrong height, which is worse than printing nothing.
        value[0] = 2;
        assert!(decode_upgrade_descriptor(&value).is_none());
    }

    #[test]
    fn a_descriptor_for_another_upgrade_is_not_read_as_the_defi_switch() {
        let chain = vrsctest();
        let id = chain_currency_id(&chain).expect("the chain id is an i-address");
        let sibling = verus_sdk::vdxf::qualified_key(
            "vrsc::system.upgradedata.disablepbaascrosschain",
            &chain.name,
            id,
        )
        .expect("a derivable name");
        let values = vec![ContentValue::Bytes(descriptor(sibling, 1_187_000))];
        // The siblings ride the same mechanism under the same content key.
        // Reporting one of them as DeFi would be a false alarm on a working
        // chain.
        assert_eq!(defi_block(&values, disable_defi(&chain)), None);
    }

    #[test]
    fn every_value_under_the_key_is_read_not_only_the_first() {
        let chain = vrsctest();
        let id = chain_currency_id(&chain).expect("the chain id is an i-address");
        let sibling = verus_sdk::vdxf::qualified_key(
            "vrsc::system.upgradedata.disableearnednotarizations",
            &chain.name,
            id,
        )
        .expect("a derivable name");
        let values = vec![
            ContentValue::Bytes(descriptor(sibling, 42)),
            ContentValue::Bytes(bytes(LIVE_VRSCTEST)),
        ];
        // The assertion that fails if the selection ever becomes `.first()`.
        assert_eq!(defi_block(&values, disable_defi(&chain)), Some(1_187_000));
    }

    #[test]
    fn a_structured_value_is_skipped_rather_than_mistaken_for_bytes() {
        let values = vec![
            ContentValue::Structured(serde_json::json!({ "version": 1 })),
            ContentValue::Bytes(bytes(LIVE_VRSCTEST)),
        ];
        assert_eq!(
            defi_block(&values, disable_defi(&vrsctest())),
            Some(1_187_000)
        );
    }

    #[test]
    fn an_oracle_that_publishes_nothing_produces_no_switch() {
        let chain = vrsctest();
        let id = chain_currency_id(&chain).expect("the chain id is an i-address");
        let sibling = verus_sdk::vdxf::qualified_key(
            "vrsc::system.upgradedata.optionalpbaasupgrade",
            &chain.name,
            id,
        )
        .expect("a derivable name");
        // Both spellings of "nothing to say": no values at all, and values that
        // are all about something else. This is what keeps the panel unchanged
        // on every chain that has not switched anything off.
        assert_eq!(defi_block(&[], disable_defi(&chain)), None);
        let values = vec![ContentValue::Bytes(descriptor(sibling, 10))];
        assert_eq!(defi_block(&values, disable_defi(&chain)), None);
    }

    #[test]
    fn a_switch_at_a_future_height_is_scheduled_rather_than_active() {
        let ui = Ui::new(ThemeFlag::Phosphor, false, false);
        let upgrade_id = disable_defi(&vrsctest());
        let active = defi_row(
            &DefiSwitch {
                block: 1_187_000,
                active: true,
                upgrade_id,
            },
            &ui,
        )
        .render();
        let scheduled = defi_row(
            &DefiSwitch {
                block: 1_187_000,
                active: false,
                upgrade_id,
            },
            &ui,
        )
        .render();

        assert_ne!(active, scheduled);
        assert!(active.contains("1,187,000"), "{active}");
        assert!(scheduled.contains("1,187,000"), "{scheduled}");
        // A height the chain has not reached refuses nothing yet, and a row
        // that said otherwise would be the same wrong answer this issue is
        // about, pointing the other way.
        assert!(active.contains("are refused"), "{active}");
        assert!(!scheduled.contains("are refused"), "{scheduled}");
        assert!(scheduled.contains("still work"), "{scheduled}");
    }
}
