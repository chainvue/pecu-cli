//! `pecu currency show` · `pecu currency launch` — currencies defined by an identity.
//!
//! # A currency is a thing an identity becomes
//!
//! There is no separate object to create. A VerusID defines a currency, once
//! and forever: the currency's id *is* the identity's i-address, and consensus
//! marks the identity with `FLAG_ACTIVE_CURRENCY` so it can never define
//! another. Launching is therefore closer to registering a name than to
//! deploying a contract, and the same one-way thinking applies.
//!
//! # Options are a bitfield, and the bits are the currency's nature
//!
//! `options` decides whether the thing is a token, a fractional basket, an NFT,
//! whether identities may be registered under it, whether those registrations
//! pay referrals. `proofprotocol` decides who may mint. Neither is inferable
//! from the name, and both are what somebody asking "what is this" wants, so
//! `show` decodes them rather than printing a number.
//!
//! # What this launches, and what it does not
//!
//! Simple tokens. A fractional basket needs reserves, weights, conversion
//! rates and preconversion limits — six vectors indexed by the same reserve
//! list, where a short one silently attributes an amount to the wrong currency.
//! The SDK validates that, but choosing the numbers is a design exercise rather
//! than a flag, and half a basket is worse than none.

use miette::Diagnostic;
use thiserror::Error;
use verus_sdk::currency::{option, CurrencyDefinition, CurrencyId};
use verus_sdk::money::{Amount, SATS_PER_COIN as SATOSHIDEN};
use verus_sdk::network::{prepare_launch, ChainReader, CurrencySummary, FlowError};
use verus_sdk::verus_keys::{Address, AddressKind};

use crate::cli::{CurrencyLaunchArgs, Globals};
use crate::config::Settings;
use crate::keystore::{self, Envelope, Keystore};
use crate::node;
use crate::ui::{fmt, Panel, Text, Ui};

/// How much of a node-supplied name is ever printed.
const NAME_BUDGET: usize = 40;

#[derive(Debug, Error, Diagnostic)]
pub enum CurrencyError {
    #[error("nothing on this chain is called `{name}`")]
    #[diagnostic(
        code(pecu::no_such_currency),
        help("a currency name or its i-address — `pecu wallet balance` names the ones an address holds")
    )]
    NotFound { name: String },

    #[error("no key to sign with")]
    #[diagnostic(
        code(pecu::no_key),
        help("pass --from <label>, or make a key with `pecu key gen`")
    )]
    NoKey,

    #[error("the keystore holds {count} keys, so there is no obvious one to sign with")]
    #[diagnostic(
        code(pecu::ambiguous_key),
        help("name one with --from <label>; `pecu key list` shows them")
    )]
    AmbiguousKey { count: usize },

    #[error("the `{profile}` profile is not allowed to spend")]
    #[diagnostic(
        code(pecu::spending_disabled),
        help("launching a currency costs a real fee. Set `allow_spend = true` under [profiles.{profile}] in config.toml")
    )]
    SpendingDisabled { profile: String },

    #[error("`{value}` is not an amount")]
    #[diagnostic(
        code(pecu::bad_amount),
        help("a decimal number of coins, at most eight places")
    )]
    BadAmount { value: String },

    #[error("`{value}` is not `currency:amount` for --{what}")]
    #[diagnostic(
        code(pecu::bad_per_reserve),
        help("a per-reserve value names the reserve it belongs to, as `VRSCTEST:10` — keyed by name so it cannot land on the wrong currency")
    )]
    BadPerReserve { what: &'static str, value: String },

    #[error("`{name}` is not one of this basket's reserves, in --{what}")]
    #[diagnostic(
        code(pecu::not_a_reserve),
        help("every per-reserve value has to name a currency given with --reserve")
    )]
    NotAReserve { what: &'static str, name: String },

    #[error("`{value}` is not a percentage for --{what}")]
    #[diagnostic(code(pecu::bad_percent), help("a number of percent, as `10` or `2.5`"))]
    BadPercent { what: &'static str, value: String },

    #[error("`{value}` is not `currency:percent`")]
    #[diagnostic(
        code(pecu::bad_reserve),
        help("a reserve is a currency and its share of the basket, as `VRSCTEST:25`. The shares must total exactly 100")
    )]
    BadReserve { value: String },

    #[error("the reserve percentages total {total}%, not 100%")]
    #[diagnostic(
        code(pecu::weights_do_not_total),
        help("a fractional basket's weights are its reserve ratios and consensus reads them as fractions of one whole. Anything else prices the basket wrongly, permanently and with no way to correct it")
    )]
    WeightsDoNotTotal { total: String },

    #[error("a basket needs a supply to price against")]
    #[diagnostic(
        code(pecu::basket_needs_supply),
        help("the pre-launch price of each reserve is derived from the initial supply, and a supply of zero makes every one of them zero. Pass --supply")
    )]
    BasketNeedsSupply,

    #[error("`{value}` is not `address:amount`")]
    #[diagnostic(
        code(pecu::bad_preallocation),
        help("a preallocation is who gets it and how much, as `iAddress:100` — the recipient must be an identity")
    )]
    BadPreallocation { value: String },

    #[error("--json will not launch without --yes")]
    #[diagnostic(
        code(pecu::needs_yes),
        help("a launch is one-way and costs a real fee. --json is machine-readable output, not consent: the confirmation prompt would go to the stream you are parsing. Add --yes, or --dry-run to see the cost")
    )]
    NeedsYes,

    #[error("cannot ask for confirmation")]
    #[diagnostic(
        code(pecu::no_tty),
        help("there is no terminal to confirm on — pass --yes, or --dry-run to stop before broadcasting")
    )]
    CannotConfirm,

    #[error("cancelled")]
    #[diagnostic(code(pecu::cancelled), help("nothing was broadcast"))]
    Cancelled,

    #[error("{what} failed")]
    #[diagnostic(code(pecu::flow_failed), help("{advice}"))]
    Flow {
        what: &'static str,
        advice: String,
        #[source]
        source: Box<FlowError>,
    },
}

fn flow(what: &'static str, source: FlowError) -> CurrencyError {
    use verus_sdk::verus_tx::TxError;
    let advice = match &source {
        FlowError::NoSuchIdentity(_) => {
            "a currency is defined by an identity, and that identity has to exist first — \
             `pecu id register <name>`"
                .to_string()
        }
        // The one-way rule, and the commonest way to meet it. `NotReady`, not
        // `Content` — matching the wrong variant sent this to "run `pecu
        // doctor`", which is wrong twice: the node is fine and no retry helps.
        FlowError::NotReady(message) if message.contains("already") => {
            "an identity defines a currency once and never again. Register another identity \
             to define another currency"
                .to_string()
        }
        FlowError::Tx(TxError::NotAPrimaryAddress { .. }) => {
            "the signing key must be one of the defining identity's primary addresses — \
             `pecu id show <name@>` lists them"
                .to_string()
        }
        FlowError::Tx(TxError::InsufficientFunds { required, .. }) => format!(
            "the signing key pays the launch fee as well. It needs at least {}",
            fmt::sats(*required)
        ),
        FlowError::Content(_) => {
            "the definition was rejected before anything was signed".to_string()
        }
        _ => "run `pecu doctor`, or point somewhere else with --node".to_string(),
    };
    CurrencyError::Flow {
        what,
        advice,
        source: Box::new(source),
    }
}

/// `NAME@:PERCENT` — a reserve currency and its share of the basket.
///
/// Percentages rather than the raw weights consensus stores, which are
/// fractions of `SATOSHIDEN`. "25" is the number a person means; `25000000` is
/// the number the chain holds, and asking for the second invites an
/// off-by-a-factor that prices the basket wrongly forever.
fn split_reserve(entry: &str) -> Result<(&str, i32), CurrencyError> {
    let bad = || CurrencyError::BadReserve {
        value: entry.to_string(),
    };
    let (name, percent) = entry.rsplit_once(':').ok_or_else(bad)?;

    // A percentage of one whole, in satoshis: 25 becomes 0.25 * SATOSHIDEN.
    let scaled = Amount::from_coins_str(percent).map_err(|_| bad())?;
    // `i32` because that is what the definition holds. A weight over 100% is
    // caught by the total check rather than here, where the message would be
    // about a type.
    let weight = i32::try_from(scaled.to_sat() / 100).map_err(|_| bad())?;
    Ok((name, weight))
}

/// Resolve a reserve's name to the currency id the definition holds.
///
/// Separate from [`split_reserve`] so the syntax and the ratios can be checked
/// with no node at all: a typo'd percentage should not need a reachable chain
/// to be refused, and the total is what decides whether the basket is even
/// coherent.
fn resolve_reserve(
    ui: &Ui,
    node: &crate::node::Node,
    name: &str,
) -> Result<CurrencyId, miette::Report> {
    let bad = || CurrencyError::BadReserve {
        value: name.to_string(),
    };
    let id = match name.parse::<Address>() {
        Ok(address) => CurrencyId::from_bytes(address.hash()),
        Err(_) => {
            let looked_up = name.strip_suffix('@').unwrap_or(name);
            ui.sdk(format!("node.currency_definition({looked_up:?})"));
            let found =
                node.currency_definition(looked_up)
                    .map_err(|_| CurrencyError::NotFound {
                        name: name.to_string(),
                    })?;
            ui.sdk_result(found.currency_id.clone());
            CurrencyId::from_bytes(
                found
                    .currency_id
                    .parse::<Address>()
                    .map_err(|_| bad())?
                    .hash(),
            )
        }
    };
    Ok(id)
}

/// A per-reserve amount, keyed by the reserve's name rather than its position.
///
/// The definition stores these as vectors indexed by the reserve list, and
/// `serialize_definition` refuses one whose length disagrees — but a vector of
/// the *right* length with entries in the wrong order is accepted and prices
/// the basket against the wrong currencies. Keying by name and filling the gaps
/// with zero removes the possibility rather than checking for it.
fn per_reserve(
    entries: &[String],
    names: &[&str],
    what: &'static str,
) -> Result<Vec<Amount>, CurrencyError> {
    if entries.is_empty() {
        // Empty is the only safe "unset": a zero-filled vector of the right
        // length is a different statement, and not the one a caller who said
        // nothing was making.
        return Ok(Vec::new());
    }
    let mut amounts = vec![Amount::ZERO; names.len()];
    for entry in entries {
        let bad = || CurrencyError::BadPerReserve {
            what,
            value: entry.clone(),
        };
        let (name, value) = entry.rsplit_once(':').ok_or_else(bad)?;
        let slot = names
            .iter()
            .position(|reserve| {
                reserve
                    .trim_end_matches('@')
                    .eq_ignore_ascii_case(name.trim_end_matches('@'))
            })
            .ok_or_else(|| CurrencyError::NotAReserve {
                what,
                name: name.to_string(),
            })?;
        amounts[slot] = Amount::from_coins_str(value).map_err(|_| bad())?;
    }
    Ok(amounts)
}

/// The satoshi-scaled fraction back as the percentage somebody typed.
fn scaled_percent(scaled: u64) -> String {
    let percent = fmt::amount(Amount::from_sat(scaled.saturating_mul(100)));
    format!("{}%", percent.trim_end_matches('0').trim_end_matches('.'))
}

/// A percentage as the satoshi-scaled fraction consensus stores.
fn percent(value: &str, what: &'static str) -> Result<u64, CurrencyError> {
    let scaled = Amount::from_coins_str(value).map_err(|_| CurrencyError::BadPercent {
        what,
        value: value.to_string(),
    })?;
    Ok(scaled.to_sat() / 100)
}

/// Everything the option bitfield says, in words.
///
/// A currency's nature is in these bits and nowhere else — the name does not
/// carry it and neither does anything else on the panel. Printing `options: 32`
/// tells a reader nothing they can act on.
fn describe_options(options: u32) -> Vec<&'static str> {
    let mut found = Vec::new();
    for (bit, label) in [
        (option::FRACTIONAL, "fractional basket"),
        (option::TOKEN, "token"),
        (option::NFT_TOKEN, "NFT"),
        (option::SINGLECURRENCY, "single-currency basket"),
        (option::ID_RESTRICTED, "only identities may hold it"),
        (option::ID_STAKING, "identities may stake it"),
        (
            option::ID_REFERRALS,
            "sub-identity registrations pay referrals",
        ),
        (option::ID_REFERRALREQUIRED, "a referral is mandatory"),
        (option::NO_IDS, "no identities may be registered under it"),
        (option::GATEWAY, "gateway"),
        (option::PBAAS, "independent PBaaS chain"),
        (option::GATEWAY_CONVERTER, "gateway converter"),
        (option::GATEWAY_NAMECONTROLLER, "gateway name controller"),
    ] {
        if options & bit != 0 {
            found.push(label);
        }
    }
    found
}

/// Who may mint, and what that means for the supply.
///
/// `proofprotocol` decides who, but what it implies depends on the kind: a
/// decentralized *token* has a supply fixed at definition, while a
/// decentralized *basket* mints and burns continuously as reserves are
/// converted in and out. Saying "fixed" about a basket would be plainly wrong.
fn describe_control(proof_protocol: u32, fractional: bool) -> &'static str {
    match (proof_protocol, fractional) {
        (1, false) => "decentralized — supply is fixed by the definition",
        (1, true) => "decentralized — supply moves as reserves convert in and out",
        (2, _) => "centralized — the defining identity can mint more",
        (3, _) => "notarized from another chain",
        _ => "unrecognised proof protocol",
    }
}

/// `pecu currency show`.
pub fn show(ui: &Ui, settings: &Settings, name: &str) -> miette::Result<()> {
    let node = node::connect(&settings.profile)?;

    // One request. Before the SDK exposed this, the only route to a currency's
    // definition was `list_currencies`, which returns every currency on the
    // chain — 464 KB against this node to answer a question about one.
    // `getcurrency` takes a bare name — `TST`, not `TST@` — while every other
    // command here takes the `@` form, because that is how a VerusID is
    // written. A currency name cannot contain `@`, so stripping one is always
    // safe and saves the reader from knowing which command wants which.
    let looked_up = name.strip_suffix('@').unwrap_or(name);

    ui.sdk(format!("node.currency_definition({looked_up:?})"));
    let found = node
        .currency_definition(looked_up)
        .map_err(|_| CurrencyError::NotFound {
            name: name.to_string(),
        })?;
    ui.sdk_result(format!(
        "CurrencySummary {{ {}, options: {} }}",
        found.currency_id, found.options
    ));

    if ui.is_json() {
        emit(&serde_json::json!({
            "currency_id": found.currency_id,
            "name": found.name,
            "fully_qualified_name": found.fully_qualified_name,
            "parent": found.parent,
            "system_id": found.system_id,
            "start_block": found.start_block,
            "end_block": found.end_block,
            "options": found.options,
            // Decoded as well as raw: the number is what the chain holds, the
            // list is what it means, and a consumer should not have to know
            // the bit values to use this.
            "kinds": describe_options(found.options),
            "proof_protocol": found.proof_protocol,
            "control": describe_control(found.proof_protocol, found.options & option::FRACTIONAL != 0),
            "definition": found.definition,
        }));
        return Ok(());
    }

    ui.panel(&panel(ui, &node, &found));
    ui.explain_panel();
    Ok(())
}

/// Whether the currency has started, which decides whether its supply exists.
///
/// A launch names a future block, and until the chain reaches it the currency
/// is defined but not live — its preallocations are not holdable yet, which is
/// otherwise a confusing "I launched it and the balance is zero". The tip costs
/// one request and is what makes the difference sayable.
fn starts_row(ui: &Ui, node: &crate::node::Node, found: &CurrencySummary) -> Text {
    let palette = ui.theme.palette;
    let glyphs = ui.theme.glyphs;
    let row = Text::of(
        format!("block {}", fmt::height(found.start_block.into())),
        palette.value,
    );
    match node.block_count() {
        Ok(tip) if tip >= found.start_block => row
            .push("  ", palette.muted)
            .push(glyphs.ok, palette.ok)
            .space()
            .push("live", palette.ok),
        Ok(tip) => row.push(
            format!(
                "  {} not yet — {} to go",
                glyphs.warn,
                fmt::plural((found.start_block - tip) as usize, "block", "blocks")
            ),
            palette.warn,
        ),
        Err(_) => row,
    }
}

fn panel(ui: &Ui, node: &crate::node::Node, found: &CurrencySummary) -> Panel {
    let palette = ui.theme.palette;
    let glyphs = ui.theme.glyphs;

    let mut panel = Panel::new("CURRENCY")
        .row(
            "name",
            Text::of(
                fmt::untrusted(&found.fully_qualified_name, NAME_BUDGET, glyphs.ellipsis),
                palette.accent,
            ),
        )
        .row("currency id", Text::of(&found.currency_id, palette.value));

    if let Some(parent) = &found.parent {
        panel = panel.row(
            "parent",
            Text::of(
                fmt::untrusted(parent, NAME_BUDGET, glyphs.ellipsis),
                palette.value,
            ),
        );
    }

    let kinds = describe_options(found.options);
    panel = panel.row(
        "kind",
        if kinds.is_empty() {
            // Not "none": the bits are what the chain holds, and an empty
            // reading means this build does not know them rather than that the
            // currency has no nature.
            Text::of(
                format!("unrecognised (options {})", found.options),
                palette.warn,
            )
        } else {
            Text::of(kinds.join(", "), palette.value)
        },
    );
    panel = panel
        .row(
            "control",
            Text::of(
                describe_control(
                    found.proof_protocol,
                    found.options & option::FRACTIONAL != 0,
                ),
                palette.value,
            ),
        )
        .row("starts", starts_row(ui, node, found));
    if found.end_block != 0 {
        panel = panel.row(
            "ends",
            Text::of(
                format!("block {}", fmt::height(found.end_block.into())),
                palette.value,
            ),
        );
    }

    // Preallocations come from the raw definition: they are the supply that
    // exists at launch, and who holds it, which is the first thing anyone asks
    // about a token they did not make.
    if let Some(list) = found
        .definition
        .get("preallocations")
        .and_then(|v| v.as_array())
    {
        if !list.is_empty() {
            panel = panel.section("PREALLOCATED");
            let mut total = 0f64;
            for entry in list {
                let Some(map) = entry.as_object() else {
                    continue;
                };
                for (recipient, amount) in map {
                    total += amount.as_f64().unwrap_or(0.0);
                    panel = panel.row(
                        "",
                        Text::of(format!("{amount}"), palette.value)
                            .space()
                            // The currency being defined, not the chain's own.
                            // A preallocation of this token labelled VRSCTEST
                            // reads as a million coins of the wrong thing.
                            .push(
                                fmt::untrusted(&found.name, NAME_BUDGET, glyphs.ellipsis),
                                palette.muted,
                            )
                            .push("  ", palette.muted)
                            .push(fmt::address(recipient, glyphs.ellipsis), palette.muted),
                    );
                }
            }
            panel = panel.note(Text::of(
                format!("{total} in total at launch"),
                palette.muted,
            ));
        }
    }

    panel.note(Text::of(
        "a currency's id is the i-address of the identity that defined it, and an identity \
         defines one exactly once",
        palette.muted,
    ))
}

/// `pecu currency launch`.
pub fn launch(
    ui: &Ui,
    settings: &Settings,
    globals: &Globals,
    args: &CurrencyLaunchArgs,
) -> miette::Result<()> {
    if !settings.profile.allow_spend {
        return Err(CurrencyError::SpendingDisabled {
            profile: settings.profile.name.clone(),
        }
        .into());
    }

    // Parsed before a key is unlocked or a node is asked: a typo'd
    // preallocation should not cost a passphrase prompt.
    let mut preallocations = Vec::with_capacity(args.preallocate.len());
    for entry in &args.preallocate {
        preallocations.push(parse_preallocation(entry)?);
    }
    let supply = match &args.supply {
        None => None,
        Some(value) => {
            Some(
                Amount::from_coins_str(value).map_err(|_| CurrencyError::BadAmount {
                    value: value.clone(),
                })?,
            )
        }
    };

    // The reserves decide what is being launched, and their syntax and ratios
    // need no chain at all. Checked here so a typo'd percentage is refused
    // without a reachable node, a keystore, or a passphrase — the ratios are
    // also what decides whether the basket is coherent, and a wrong one prices
    // it wrongly forever.
    let mut names = Vec::with_capacity(args.reserve.len());
    let mut weights = Vec::with_capacity(args.reserve.len());
    for entry in &args.reserve {
        let (name, weight) = split_reserve(entry)?;
        names.push(name);
        weights.push(weight);
    }
    let fractional = !names.is_empty();

    if fractional {
        // Summed as i64: a handful of i32 weights cannot overflow it, and a
        // total that is wrong should read as wrong rather than wrap.
        let total: i64 = weights.iter().map(|w| i64::from(*w)).sum();
        if total != i64::try_from(SATOSHIDEN).unwrap_or(i64::MAX) {
            return Err(CurrencyError::WeightsDoNotTotal {
                // Back to the percentage the caller typed, not the fraction of
                // SATOSHIDEN the chain stores: the message should name the
                // number they wrote.
                total: fmt::amount(Amount::from_sat(
                    u64::try_from(total.saturating_mul(100)).unwrap_or(0),
                ))
                .trim_end_matches('0')
                .trim_end_matches('.')
                .to_string(),
            }
            .into());
        }
        // Every reserve price is `SATOSHIDEN^3 / (supply * weight)`, so a
        // supply of zero makes all of them zero.
        if supply.unwrap_or(Amount::ZERO) == Amount::ZERO {
            return Err(CurrencyError::BasketNeedsSupply.into());
        }
    }

    let store = Keystore::new(&settings.paths);
    let envelope = choose_key(&store, args.from.as_deref())?;
    let node = node::connect(&settings.profile)?;

    // The parent is the currency the identity lives under, which for a
    // top-level name is the chain itself. Read rather than assumed: it decides
    // the launch fee and has to match what the identity says.
    ui.sdk("verus_sdk::network::native_currency(&node)");
    let parent = verus_sdk::network::native_currency(&node)
        .map_err(|source| flow("reading the parent currency", source))?;
    ui.sdk_result(Address::new(AddressKind::Identity, parent.to_bytes()).to_string());

    ui.sdk("node.block_count()");
    let tip = node.block_count().map_err(|source| {
        node::NodeError::request("reading the tip", &settings.profile.node, source)
    })?;
    ui.sdk_result(fmt::height(tip.into()));

    // A launch cannot start in the past, and the flow refuses one that would.
    // The default leaves room for the transaction to be mined and for a human
    // to notice a mistake before conversions open.
    let start_block = args.start_block.unwrap_or(tip + args.start_in);

    // Names to ids, which is the only part that needs the chain.
    let mut reserves = Vec::with_capacity(names.len());
    for name in &names {
        reserves.push(resolve_reserve(ui, &node, name)?);
    }

    // `--supply` is sugar for preallocating to the defining identity for a
    // *token*, and has to be: a token's supply is the **sum of its
    // preallocations**, and `initial_supply` is read only for a fractional
    // currency. Setting that field on a token produces one with no supply.
    // A basket is the other way round, and takes the field directly.
    if let Some(amount) = supply.filter(|_| !fractional) {
        ui.sdk(format!("node.identity({:?})", args.name));
        let record = node
            .identity(&args.name)
            .map_err(|source| flow("reading the defining identity", FlowError::Rpc(source)))?;
        ui.sdk_result(record.identity_address.clone());
        let holder: Address =
            record
                .identity_address
                .parse()
                .map_err(|_| CurrencyError::NotFound {
                    name: args.name.clone(),
                })?;
        preallocations.push(verus_sdk::currency::Preallocation {
            recipient: holder.hash(),
            amount,
        });
    }

    let bare = args.name.trim_end_matches('@');
    let mut definition = CurrencyDefinition::token(parent, bare, start_block.into());
    definition.preallocations = preallocations;
    if fractional {
        definition.options |= option::FRACTIONAL;
        definition.initial_supply = supply.unwrap_or(Amount::ZERO);
        definition.currencies = reserves;
        definition.weights = weights;
        definition.conversions = per_reserve(&args.conversion, &names, "conversion")?;
        definition.min_preconversion = per_reserve(&args.min_preconvert, &names, "min-preconvert")?;
        definition.max_preconversion = per_reserve(&args.max_preconvert, &names, "max-preconvert")?;

        let contributions = per_reserve(&args.contribute, &names, "contribute")?;
        if !contributions.is_empty() {
            // `with_contributions` sets `preconverted` to match. The daemon
            // initialises the two equal and never reveals the second in its
            // RPC output, so setting one alone is invisible and wrong.
            definition = definition.with_contributions(contributions);
        }

        if let Some(value) = &args.prelaunch_discount {
            definition.prelaunch_discount = percent(value, "prelaunch-discount")?;
        }
        if let Some(value) = &args.prelaunch_carveout {
            // `i32` on the wire, not `i64`: a carveout over about 21% of a
            // whole would overflow it, which is a refusal rather than a wrap.
            definition.prelaunch_carveout = i32::try_from(percent(value, "prelaunch-carveout")?)
                .map_err(|_| CurrencyError::BadPercent {
                    what: "prelaunch-carveout",
                    value: value.clone(),
                })?;
        }
    }

    if let Some(value) = &args.id_registration_fee {
        definition.id_registration_fees = Amount::from_coins_str(value)
            .map_err(|_| CurrencyError::BadAmount {
                value: value.clone(),
            })?
            .to_sat();
    }
    if let Some(levels) = args.id_referral_levels {
        definition.id_referral_levels = u64::from(levels);
        // The count alone does nothing: consensus pays referrals only when the
        // option bit says to, so setting one without the other publishes a
        // policy that never applies.
        definition.options |= option::ID_REFERRALS;
        if args.id_referral_required {
            definition.options |= option::ID_REFERRALREQUIRED;
        }
    }
    if let Some(value) = &args.id_import_fee {
        definition.id_import_fees = Amount::from_coins_str(value)
            .map_err(|_| CurrencyError::BadAmount {
                value: value.clone(),
            })?
            .to_sat();
    }
    if let Some(height) = args.end_block {
        definition.end_block = u64::from(height);
    }
    if args.nft {
        // Also sets FLAG_TOKENIZED_CONTROL on the identity in the builder:
        // whoever holds the token controls the identity, and the revocation
        // and recovery authorities stop deciding.
        definition.options |= option::NFT_TOKEN;
    }
    if args.id_restricted {
        definition.options |= option::ID_RESTRICTED;
    }
    if args.id_staking {
        definition.options |= option::ID_STAKING;
    }
    if args.no_ids {
        definition.options |= option::NO_IDS;
    }
    if args.mintable {
        // `proofprotocol = 2` is what lets the defining identity mint more
        // later. It is not recoverable afterwards, so it is a flag rather than
        // a default.
        definition.proof_protocol = 2;
    }

    let secret = keystore::passphrase(&format!("passphrase for `{}`", envelope.label), false)?;
    let key = envelope.unlock(&secret)?;

    ui.sdk(format!(
        "verus_sdk::network::prepare_launch(&node, &[&key], {:?}, &definition, None)",
        args.name
    ));
    let unsent = prepare_launch(&node, &[&key], &args.name, &definition, None)
        .map_err(|source| flow("building the launch", source))?;
    ui.sdk_result(format!(
        "Unsent<Launched> {{ txid: {}, fee: {} }}",
        unsent.outcome.txid,
        fmt::amount(unsent.outcome.launch_fee)
    ));

    let review = launch_panel(
        ui,
        settings,
        args,
        &definition,
        &unsent.outcome,
        globals.dry_run,
    );

    if ui.is_json() {
        let document = serde_json::json!({
            "identity": args.name,
            "currency_id": Address::new(AddressKind::Identity, unsent.outcome.currency_id).to_string(),
            "txid": unsent.outcome.txid,
            "start_block": unsent.outcome.start_block,
            "launch_fee": unsent.outcome.launch_fee.to_sat(),
            "supply": definition.preallocations.iter().map(|p| p.amount.to_sat()).sum::<u64>(),
            "mintable": args.mintable,
            "broadcast": false,
        });
        if globals.dry_run {
            emit(&document);
            return Ok(());
        }
        if !globals.yes {
            return Err(CurrencyError::NeedsYes.into());
        }
        ui.sdk("unsent.broadcast(&node)");
        let done = unsent
            .broadcast(&node)
            .map_err(|source| flow("broadcasting the launch", source))?;
        emit(&serde_json::json!({
            "identity": args.name,
            "currency_id": Address::new(AddressKind::Identity, done.currency_id).to_string(),
            "txid": done.txid,
            "start_block": done.start_block,
            "launch_fee": done.launch_fee.to_sat(),
            "broadcast": true,
        }));
        return Ok(());
    }

    ui.panel(&review);
    if globals.dry_run {
        ui.blank();
        ui.note("nothing was sent. Drop --dry-run to launch it");
        ui.explain_panel();
        return Ok(());
    }
    if !globals.yes {
        confirm(ui)?;
    }

    ui.sdk("unsent.broadcast(&node)");
    let done = unsent
        .broadcast(&node)
        .map_err(|source| flow("broadcasting the launch", source))?;
    ui.sdk_result(format!("Launched {{ txid: {} }}", done.txid));

    ui.blank();
    ui.ok(format!("broadcast — txid {}", done.txid));
    ui.note(format!(
        "{}/tx/{}",
        settings.profile.explorer.trim_end_matches('/'),
        done.txid
    ));
    ui.explain_panel();
    Ok(())
}

fn launch_panel(
    ui: &Ui,
    settings: &Settings,
    args: &CurrencyLaunchArgs,
    definition: &CurrencyDefinition,
    outcome: &verus_sdk::network::Launched,
    dry_run: bool,
) -> Panel {
    let palette = ui.theme.palette;
    let glyphs = ui.theme.glyphs;
    let currency = &settings.profile.currency;

    let mut panel = Panel::new(if dry_run { "WOULD LAUNCH" } else { "LAUNCH" })
        .row(
            "identity",
            Text::of(
                fmt::untrusted(&args.name, NAME_BUDGET, glyphs.ellipsis),
                palette.accent,
            ),
        )
        .row(
            "currency id",
            Text::of(
                Address::new(AddressKind::Identity, outcome.currency_id).to_string(),
                palette.value,
            ),
        )
        .row(
            "kind",
            Text::of(
                describe_options(definition.options).join(", "),
                palette.value,
            ),
        )
        .row(
            "control",
            Text::of(
                describe_control(
                    u32::try_from(definition.proof_protocol).unwrap_or(0),
                    definition.options & option::FRACTIONAL != 0,
                ),
                palette.value,
            ),
        )
        .row(
            "starts",
            Text::of(
                format!("block {}", fmt::height(outcome.start_block)),
                palette.value,
            ),
        )
        .row(
            "fee",
            Text::of(fmt::amount(outcome.launch_fee), palette.accent)
                .space()
                .push(currency, palette.muted),
        )
        .row("txid", Text::of(&outcome.txid, palette.value));

    if !definition.currencies.is_empty() {
        panel = panel
            .row(
                "supply",
                Text::of(fmt::amount(definition.initial_supply), palette.accent)
                    .push("  the reserves are priced against this", palette.muted),
            )
            .section("RESERVES");
        for (index, (id, weight)) in definition
            .currencies
            .iter()
            .zip(&definition.weights)
            .enumerate()
        {
            // The percentage back, not the fraction of SATOSHIDEN stored: the
            // reader typed one and should be able to check it.
            let mut row = Text::of(
                format!("{:>6}%", (*weight as f64) * 100.0 / SATOSHIDEN as f64),
                palette.value,
            )
            .push("  ", palette.muted)
            .push(
                fmt::address(
                    &Address::new(AddressKind::Identity, id.to_bytes()).to_string(),
                    glyphs.ellipsis,
                ),
                palette.muted,
            );
            // Everything else this reserve carries, on its own line, because a
            // seed contribution and a preconversion limit are money and belong
            // where the ratio is rather than in a separate list to cross-refer.
            for (label, values) in [
                ("seeded", &definition.initial_contributions),
                ("min", &definition.min_preconversion),
                ("max", &definition.max_preconversion),
                ("rate", &definition.conversions),
            ] {
                if let Some(amount) = values.get(index).filter(|a| **a != Amount::ZERO) {
                    row = row.push(format!("  {label} {}", fmt::amount(*amount)), palette.muted);
                }
            }
            panel = panel.row("", row);
        }

        if definition.prelaunch_discount != 0 {
            panel = panel.row(
                "discount",
                Text::of(scaled_percent(definition.prelaunch_discount), palette.value)
                    .push("  to anyone converting before launch", palette.muted),
            );
        }
        if definition.prelaunch_carveout != 0 {
            panel = panel.row(
                "carveout",
                Text::of(
                    scaled_percent(u64::try_from(definition.prelaunch_carveout).unwrap_or(0)),
                    palette.warn,
                )
                .push("  of the launch, to this identity", palette.muted),
            );
        }
    }

    // What it will cost to register `alice.thiscurrency@`. Published in the
    // definition and unchangeable, so it belongs on the panel that approves it.
    if definition.id_registration_fees != 0
        || definition.id_referral_levels != 0
        || definition.id_import_fees != 0
        || definition.options & option::NO_IDS != 0
    {
        panel = panel.section("SUB-IDENTITIES");
        if definition.options & option::NO_IDS != 0 {
            panel = panel.row(
                "",
                Text::of("none may be registered under it", palette.warn),
            );
        }
        if definition.id_registration_fees != 0 {
            panel = panel.row(
                "registration",
                Text::of(
                    fmt::amount(Amount::from_sat(definition.id_registration_fees)),
                    palette.value,
                ),
            );
        }
        if definition.id_referral_levels != 0 {
            panel = panel.row(
                "referrals",
                Text::of(
                    fmt::plural(definition.id_referral_levels as usize, "level", "levels"),
                    palette.value,
                )
                .push(
                    if definition.options & option::ID_REFERRALREQUIRED != 0 {
                        "  and one is mandatory"
                    } else {
                        "  optional"
                    },
                    palette.muted,
                ),
            );
        }
        if definition.id_import_fees != 0 {
            panel = panel.row(
                "import",
                Text::of(
                    fmt::amount(Amount::from_sat(definition.id_import_fees)),
                    palette.value,
                ),
            );
        }
    }

    if !definition.preallocations.is_empty() {
        let total = definition
            .preallocations
            .iter()
            .fold(Amount::ZERO, |sum, entry| {
                sum.checked_add(entry.amount).unwrap_or(sum)
            });
        panel = panel
            .row(
                "supply",
                Text::of(fmt::amount(total), palette.accent)
                    .push("  the sum of the preallocations below", palette.muted),
            )
            .section("PREALLOCATED");
        for entry in &definition.preallocations {
            panel = panel.row(
                "",
                Text::of(fmt::amount(entry.amount), palette.value)
                    .space()
                    .push(
                        fmt::address(
                            &Address::new(AddressKind::Identity, entry.recipient).to_string(),
                            glyphs.ellipsis,
                        ),
                        palette.muted,
                    ),
            );
        }
    }

    panel
        .note(Text::of(
            "this identity will define this currency and can never define another — the \
             currency's id is the identity's own i-address",
            palette.warn,
        ))
        .note(match (args.mintable, !definition.currencies.is_empty()) {
            (true, _) => Text::of(
                "centralized: the identity can mint more afterwards. Holders are trusting it \
                 not to",
                palette.warn,
            ),
            // A basket is decentralized *and* its supply moves — saying "fixed"
            // here contradicted the control row two lines above it.
            (false, true) => Text::of(
                "the reserves and their ratios cannot be changed after this. A basket that \
                 prices wrongly prices wrongly forever",
                palette.warn,
            ),
            (false, false) => Text::of(
                "decentralized: the supply is fixed by this definition and nobody can add to it",
                palette.muted,
            ),
        })
}

/// `iAddress:amount`, which is who gets it and how much.
fn parse_preallocation(entry: &str) -> Result<verus_sdk::currency::Preallocation, CurrencyError> {
    let bad = || CurrencyError::BadPreallocation {
        value: entry.to_string(),
    };
    let (address, amount) = entry.rsplit_once(':').ok_or_else(bad)?;
    let parsed: Address = address.parse().map_err(|_| bad())?;
    // A preallocation names an identity, not a transparent address: the supply
    // is held by whoever controls the id.
    if parsed.kind() != AddressKind::Identity {
        return Err(bad());
    }
    Ok(verus_sdk::currency::Preallocation {
        recipient: parsed.hash(),
        amount: Amount::from_coins_str(amount).map_err(|_| CurrencyError::BadAmount {
            value: amount.to_string(),
        })?,
    })
}

fn confirm(ui: &Ui) -> Result<(), CurrencyError> {
    use std::io::{IsTerminal, Write};
    if !std::io::stdin().is_terminal() {
        return Err(CurrencyError::CannotConfirm);
    }
    ui.blank();
    print!("  type `launch` to go ahead: ");
    let _ = std::io::stdout().flush();
    let mut answer = String::new();
    std::io::stdin()
        .read_line(&mut answer)
        .map_err(|_| CurrencyError::CannotConfirm)?;
    if answer.trim() != "launch" {
        return Err(CurrencyError::Cancelled);
    }
    Ok(())
}

fn choose_key(store: &Keystore, label: Option<&str>) -> Result<Envelope, miette::Report> {
    if let Some(label) = label {
        return Ok(store.load(label)?);
    }
    let keys = store.list()?;
    match keys.len() {
        0 => Err(CurrencyError::NoKey.into()),
        1 => Ok(keys.into_iter().next().expect("just checked")),
        count => Err(CurrencyError::AmbiguousKey { count }.into()),
    }
}

fn emit(value: &serde_json::Value) {
    println!(
        "{}",
        serde_json::to_string_pretty(value).expect("the report is plain data")
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_option_bitfield_reads_as_words() {
        // `32` is what the chain holds for an ordinary token, and what a panel
        // printing the raw number would show a reader.
        assert_eq!(describe_options(option::TOKEN), vec!["token"]);
        assert_eq!(
            describe_options(option::TOKEN | option::NFT_TOKEN),
            vec!["token", "NFT"]
        );
        assert_eq!(
            describe_options(option::FRACTIONAL | option::TOKEN),
            vec!["fractional basket", "token"]
        );
        // Unknown bits read as nothing rather than as a wrong name.
        assert!(describe_options(0).is_empty());
    }

    #[test]
    fn who_may_mint_is_stated_rather_than_numbered() {
        assert!(describe_control(1, false).contains("fixed"));
        assert!(describe_control(2, false).contains("can mint"));
        assert!(describe_control(99, false).contains("unrecognised"));
    }

    #[test]
    fn a_token_carries_no_initial_supply_field() {
        // The trap this codifies: a token's supply is the sum of its
        // preallocations, and `initial_supply` is read only for a fractional
        // currency. Setting it on a token launches one with no supply.
        let parent = CurrencyId::from_bytes(
            "iJhCezBExJHvtyH3fGhNnt2NhU4Ztkf2yq"
                .parse::<Address>()
                .expect("an i-address")
                .hash(),
        );
        let definition = CurrencyDefinition::token(parent, "example", 1_000_000);
        assert_eq!(definition.initial_supply, Amount::ZERO);
        assert_eq!(definition.options, option::TOKEN);
        // Decentralized until asked otherwise: a fixed supply is the property
        // a holder can check, and `--mintable` is the deliberate opt-out.
        assert_eq!(definition.proof_protocol, 1);
    }

    #[test]
    fn a_baskets_control_is_not_described_as_a_fixed_supply() {
        // The same `proofprotocol` means different things either side of
        // FRACTIONAL: a token's supply is settled at definition, a basket's
        // moves every time somebody converts.
        assert!(describe_control(1, false).contains("fixed"));
        assert!(describe_control(1, true).contains("moves"));
        assert!(!describe_control(1, true).contains("fixed"));
        // Minting overrides both, because it is about who rather than what.
        assert!(describe_control(2, true).contains("can mint"));
        assert!(describe_control(2, false).contains("can mint"));
    }

    #[test]
    fn per_reserve_values_land_on_the_reserve_they_name() {
        let names = ["VRSCTEST", "TST", "SILQ"];
        // Given out of order and with a gap: the middle reserve gets nothing,
        // and the two that were named get their own amounts. Positional
        // vectors are how an amount ends up against the wrong currency, and
        // this is the shape that would do it.
        let got = per_reserve(
            &["SILQ:7".to_string(), "VRSCTEST:1".to_string()],
            &names,
            "contribute",
        )
        .expect("valid");
        assert_eq!(got.len(), 3);
        assert_eq!(got[0].to_sat(), 100_000_000);
        assert_eq!(got[1], Amount::ZERO);
        assert_eq!(got[2].to_sat(), 700_000_000);
    }

    #[test]
    fn nothing_given_stays_empty_rather_than_becoming_zeroes() {
        // An empty vector and a zero-filled one of the right length are
        // different statements, and only the first is what a caller who said
        // nothing meant.
        assert!(per_reserve(&[], &["VRSCTEST"], "contribute")
            .expect("valid")
            .is_empty());
    }

    #[test]
    fn a_per_reserve_value_for_something_that_is_not_a_reserve_is_refused() {
        assert!(per_reserve(&["SILQ:7".to_string()], &["VRSCTEST"], "contribute").is_err());
        assert!(per_reserve(&["VRSCTEST".to_string()], &["VRSCTEST"], "contribute").is_err());
        // The `@` form is how identities are written everywhere else here, so
        // it has to match a bare reserve name.
        assert!(per_reserve(&["VRSCTEST@:7".to_string()], &["VRSCTEST"], "contribute").is_ok());
    }

    #[test]
    fn percentages_round_trip_through_the_scaling_consensus_stores() {
        // 10% is 0.1 of one whole, which is 10_000_000 satoshi-scaled.
        assert_eq!(percent("10", "x").expect("valid"), 10_000_000);
        assert_eq!(percent("2.5", "x").expect("valid"), 2_500_000);
        assert_eq!(scaled_percent(10_000_000), "10%");
        assert_eq!(scaled_percent(2_500_000), "2.5%");
    }

    #[test]
    fn a_preallocation_needs_an_identity_and_an_amount() {
        let ok = parse_preallocation("i7r29bDQfrwjkTxjv4bcYD6B1ZV7WZ4kGo:100").expect("valid");
        assert_eq!(ok.amount.to_sat(), 10_000_000_000);

        // A transparent address cannot hold a preallocation: the supply goes to
        // whoever controls an identity.
        assert!(parse_preallocation("RComfCn4wHHsGR8vWBAU7T1r3tHHyxN9Hm:100").is_err());
        assert!(parse_preallocation("i7r29bDQfrwjkTxjv4bcYD6B1ZV7WZ4kGo").is_err());
        assert!(parse_preallocation("notanaddress:100").is_err());
        assert!(parse_preallocation("i7r29bDQfrwjkTxjv4bcYD6B1ZV7WZ4kGo:abc").is_err());
    }
}
