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
use verus_sdk::convert::ConversionKind;
use verus_sdk::currency::{option, CurrencyDefinition, CurrencyId};
use verus_sdk::money::{Amount, SATS_PER_COIN as SATOSHIDEN};
use verus_sdk::network::{
    plan_conversion, prepare_conversion, prepare_launch, prepare_mint, spendable, ChainReader,
    CurrencySummary, FlowError,
};
use verus_sdk::verus_keys::{Address, AddressKind};

use crate::cli::{
    CurrencyConvertArgs, CurrencyLaunchArgs, CurrencyMintArgs, CurrencyPreconvertArgs, Globals,
};
use crate::config::Settings;
use crate::keystore::{self, Envelope, Keystore};
use crate::node;
use crate::ui::{fmt, Panel, Text, Ui};

/// How much of a node-supplied name is ever printed.
const NAME_BUDGET: usize = 40;

#[derive(Debug, Error, Diagnostic)]
pub enum CurrencyError {
    #[error("`{name}@` is an identity, but it has not defined a currency")]
    #[diagnostic(
        code(pecu::not_a_currency_yet),
        help("every identity starts this way — a currency is something an identity becomes, and `pecu currency launch {name}@` is what makes it one. `pecu id show {name}@` reads the identity itself")
    )]
    NotACurrencyYet { name: String },

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

    #[error("--start-in {start_in} counts past the last block there can be")]
    #[diagnostic(
        code(pecu::start_block_unreachable),
        help(
            "the start block is the tip plus --start-in, and {tip} + {start_in} does not fit \
             the 32-bit height the chain counts in. Nothing was spent. A start block is worth \
             measuring in blocks rather than aeons — --start-in 20 is about twenty minutes on \
             VRSCTEST — or name an absolute height with --start-block"
        )
    )]
    StartBlockUnreachable { tip: u32, start_in: u32 },

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

    #[error("--{what} names some reserves but not `{reserve}`, which caps it at zero")]
    #[diagnostic(
        code(pecu::reserve_capped_at_zero),
        help("a cap of zero means nothing is accepted into that reserve, not that it is unlimited — and a fractional basket refunds the entire launch unless every reserve receives a contribution. So this definition cannot launch, and it cannot be changed once it is on chain. Name every reserve, or name none: an omitted vector is never consulted and leaves them all uncapped")
    )]
    ReserveCappedAtZero { reserve: String, what: &'static str },

    #[error("that is more than `{name}` accepts into `{spend}`")]
    #[diagnostic(
        code(pecu::over_the_preconvert_cap),
        help("the cap is {cap} and {already} is already in, leaving room for {room}. Consensus refunds a contribution over the cap **whole** rather than trimming it to fit, so this would land as nothing — and a fractional basket refunds the entire launch unless every reserve receives something, so an oversized contribution can lose everyone else's too. Send {room} or less")
    )]
    OverThePreconvertCap {
        spend: String,
        name: String,
        cap: String,
        already: String,
        room: String,
    },

    #[error("`{name}` accepts nothing into `{spend}`")]
    #[diagnostic(
        code(pecu::reserve_accepts_nothing),
        help("that reserve's maxpreconversion is zero, so consensus refunds every contribution to it rather than refusing them — the money would leave, wait for the import, and come back. The definition is on chain and cannot be changed, so this basket can only be funded through its other reserves, and a fractional basket that does not fund all of them refunds the whole launch")
    )]
    ReserveAcceptsNothing { spend: String, name: String },

    #[error("neither `{spend}` nor `{into}` is a basket")]
    #[diagnostic(
        code(pecu::neither_is_a_basket),
        help("a conversion needs a fractional basket somewhere: spend a reserve to get the basket, spend the basket to get a reserve, or pass --via <basket@> to go from one of its reserves to another")
    )]
    NeitherIsABasket { spend: String, into: String },

    #[error("`{name}` launches at block {start_block}, and the tip is {tip}")]
    #[diagnostic(
        code(pecu::not_launched_yet),
        help("a basket has no reserves to price against until it launches, so consensus refuses a conversion through it. Before the start block the thing that works is `pecu currency preconvert`, which buys at the launch price — the two are never both valid")
    )]
    NotLaunchedYet {
        name: String,
        start_block: u32,
        tip: u32,
    },

    #[error("`{name}` refunded its launch and holds nothing")]
    #[diagnostic(
        code(pecu::launch_refunded),
        help("its launch did not meet the conditions, so every contribution went back and the basket has no reserves — a fractional basket refunds unless each of its reserves receives one before the start block. The definition still reads as a currency, but nothing can be converted through it, ever")
    )]
    LaunchRefunded { name: String },

    #[error("converting nothing is not a transaction")]
    #[diagnostic(code(pecu::converts_nothing), help("--amount is how much to spend"))]
    ConvertsNothing,

    #[error("a conversion pays a transparent address, and `{value}` is not one")]
    #[diagnostic(
        code(pecu::convert_needs_r_address),
        help("the destination is written as a bare key hash, so an identity here would pay the R-address with the same hash — one nobody holds a key to. Consensus supports paying an identity; the SDK cannot build it yet")
    )]
    ConvertNeedsTransparentRecipient { value: String },

    #[error("`{name}` launched at block {start_block}, and the tip is {tip}")]
    #[diagnostic(
        code(pecu::already_launched),
        help("a preconversion buys at the launch price and is only accepted before the start block. Afterwards the currency has reserves and an ordinary conversion is the thing that works — the two are never both valid")
    )]
    AlreadyLaunched {
        name: String,
        start_block: u32,
        tip: u32,
    },

    #[error("`{spend}` is not one of `{name}`'s {reserves} reserves")]
    #[diagnostic(
        code(pecu::not_one_of_its_reserves),
        help("a preconversion has to be paid in something the currency is actually backed by. Consensus refunds anything else rather than refusing it, so the mistake costs a wait — `pecu currency show` lists the reserves")
    )]
    NotOneOfItsReserves {
        spend: String,
        name: String,
        reserves: usize,
    },

    #[error("no outputs at {address} carry `{spend}`")]
    #[diagnostic(
        code(pecu::no_token_to_spend),
        help("the signing key holds none of that currency — `pecu wallet balance --key <label>` lists what it does hold")
    )]
    NoTokenToSpend { spend: String, address: String },

    #[error("`{value}` is not an address")]
    #[diagnostic(
        code(pecu::not_an_address),
        help("a transparent R-address, which is what a mint pays")
    )]
    NotAnAddress { value: String },

    #[error("a mint pays a transparent address, and `{value}` is not one")]
    #[diagnostic(
        code(pecu::mint_needs_r_address),
        help("the destination is written as a bare key hash, so an identity here would silently pay the R-address with the same hash — one nobody holds a key to. Consensus does support minting to an identity; the SDK cannot build it yet. Note that paying one of the identity's primary addresses is NOT equivalent: those tokens belong to whoever holds that key, not to the identity")
    )]
    MintNeedsTransparentRecipient { value: String },

    #[error("minting nothing is not a transaction")]
    #[diagnostic(
        code(pecu::mints_nothing),
        help("--amount is how much new supply to create")
    )]
    MintsNothing,

    #[error("`{name}` is decentralized, so its supply is fixed")]
    #[diagnostic(
        code(pecu::not_centralized),
        help("only a currency with `proofprotocol` 2 can mint, and that is decided once at launch by --mintable. There is no authority that could add to this one — which is the property its holders can verify")
    )]
    NotCentralized { name: String },

    #[error("`{name}` is a fractional basket, and a basket is not minted")]
    #[diagnostic(
        code(pecu::cannot_mint_a_basket),
        help("a basket's supply moves by conversion: it grows when reserves convert in and shrinks when they convert out. There is no issuer to mint it")
    )]
    CannotMintABasket { name: String },

    #[error("consensus refused the NFT launch")]
    #[diagnostic(
        code(pecu::nft_unsupported),
        help(
            "this is an SDK gap, not a mistake in what you asked for. An identity with tokenized \
              control carries a second destination on its recovery condition — the \
              EVAL_IDENTITY_RECOVER contract key — and the SDK's identity output script does not \
              emit it, so consensus derives a different script and refuses the transaction. \
              Nothing was spent. Tracked upstream; every other currency kind launches"
        )
    )]
    NftScriptGap,

    #[error("--contribute would declare reserve backing that nothing funds")]
    #[diagnostic(
        code(pecu::contribution_unfunded),
        help(
            "this is an SDK gap, not a mistake in what you asked for. A contribution is an extra \
              value-bearing output funding the reserve, and the seven the launch builder emits \
              never include it — so not one satoshi would leave the signing key. Worse, the launch \
              notarization published in the same transaction states the reserves hold nothing, so \
              the definition would claim backing it does not have, permanently and unchangeably. \
              Launch without --contribute and seed each reserve with `pecu currency preconvert`, \
              which does spend, before the start block — remembering that a fractional basket \
              refunds the entire launch unless every one of its reserves receives something. The \
              SDK has since learnt to build the funding output; this refusal goes when the pin \
              moves"
        )
    )]
    ContributionUnfunded,

    #[error("--conversion writes a launch price consensus derives and ignores")]
    #[diagnostic(
        code(pecu::launch_price_derived),
        help(
            "a fractional basket's pre-launch price is not a number in its definition. Consensus \
              computes it at launch, into the launch notarization published in the same \
              transaction, as SATOSHIDEN^3 divided by (initial supply x weight) — so \
              `conversions` is read by nothing, and the daemon's own captures settle it: a \
              definition created by passing [4.0] comes back carrying [0.0]. Honouring the flag \
              would put a number on chain permanently and unchangeably, in a byte shape no daemon \
              has ever written, while the price stayed whatever the supply and the weights make \
              it. The figure that does move it is --supply, the denominator every reserve price \
              divides by. Launch without --conversion"
        )
    )]
    LaunchPriceDerived,

    #[error("{what} failed")]
    #[diagnostic(code(pecu::flow_failed), help("{advice}"))]
    Flow {
        what: &'static str,
        advice: String,
        #[source]
        source: Box<FlowError>,
    },
}

/// A launch refused at `bad-txns-failed-precheck` means something different when
/// the definition is an NFT: the transaction the SDK builds cannot be accepted,
/// however the flags are set. Saying so beats sending the user to `pecu doctor`
/// for a node that is working perfectly.
fn nft_aware(nft: bool, what: &'static str, source: FlowError) -> CurrencyError {
    use verus_sdk::network::RpcError;
    if nft {
        if let FlowError::Rpc(RpcError::Node { code: -25, message }) = &source {
            if message.contains("failed-precheck") {
                return CurrencyError::NftScriptGap;
            }
        }
    }
    flow(what, source)
}

fn flow(what: &'static str, source: FlowError) -> CurrencyError {
    use verus_sdk::network::RpcError;
    use verus_sdk::verus_tx::TxError;
    let advice = match &source {
        // The defining identity has to exist before it can define anything, and
        // the fix is one flag away. Sending this to `pecu doctor` blames a node
        // that answered the question correctly.
        FlowError::Rpc(RpcError::Node { code: -5, message }) if message.contains("Identity") => {
            "a currency is defined by an identity, and that name is not on this chain yet. \
             Add --register to create it first, for 100 VRSCTEST on top of the launch fee, or \
             register it separately with `pecu id register <name>`"
                .to_string()
        }
        // Coin selection reads confirmed outputs only, so a transaction sent
        // moments ago has not removed the coins it spent from the candidate
        // list. Tracked upstream; a block clears it.
        FlowError::Rpc(RpcError::Node { code: -26, message })
            if message.contains("inputs-spent") =>
        {
            "this picked coins that an earlier transaction of yours already spent — coin \
             selection does not see the mempool yet, so two spends in quick succession can \
             collide. Nothing was sent and nothing is lost: wait for a block and run it again"
                .to_string()
        }
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
        // The other NotReady worth naming. The flow re-reads the tip and
        // refuses `start_block <= tip`, so anything that measured the height
        // earlier than the flow does arrives here: a --start-block that has
        // since passed, --start-in 0, or a block mined while the passphrase was
        // being typed. `pecu doctor` blames a node that answered correctly, and
        // no retry against a different node helps.
        FlowError::NotReady(message) if message.contains("start_block") => {
            "conversions cannot open at a block that has already passed. Nothing was spent — \
             this is checked before the launch fee — and running it again measures the start \
             block from a fresh tip. Leave more room with --start-in, or name a height ahead \
             of the tip with --start-block"
                .to_string()
        }
        FlowError::Tx(TxError::NotAPrimaryAddress { .. }) => {
            "the signing key must be one of the defining identity's primary addresses — \
             `pecu id show <name@>` lists them"
                .to_string()
        }
        // Both variants: the builder's, and the flow's own funding check, which
        // is the one that actually fires here. Matching only the first sent
        // this to "run `pecu doctor`" — the node is fine, the key is empty.
        FlowError::Tx(TxError::InsufficientFunds { required, .. }) => format!(
            "the launch fee comes from the signing key, not from the identity. It needs at \
             least {}",
            fmt::sats(*required)
        ),
        FlowError::InsufficientFunds {
            needed, address, ..
        } => format!(
            "the launch fee comes from the signing key, not from the identity — {address} \
             needs {needed}. `pecu send --to {address} --amount …` tops it up"
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

/// A reserve's share, to two decimals.
///
/// [`scaled_percent`] prints what consensus stores exactly, which is right for
/// echoing back what a caller asked for — `62.5%` must not become `62.50%`. A
/// *live* weight is a computed ratio, so it arrives as `33.333334%`, and six
/// decimals of a number that drifts with every conversion is noise.
fn weight_percent(scaled: u64) -> String {
    // Nearest, not ceiling: 33.333334% is 33.33%, and rounding it up to 33.34%
    // reports a share the chain does not hold.
    let scale = u128::from(SATOSHIDEN);
    let hundredths = (u128::from(scaled) * 10_000 + scale / 2) / scale;
    let whole = hundredths / 100;
    let rest = hundredths % 100;
    if rest == 0 {
        format!("{whole}%")
    } else {
        format!("{whole}.{rest:02}%")
            .replace("0%", "%")
            .replace(".%", "%")
    }
}

/// The block conversions open at: `--start-block` as given, or `tip` plus
/// `--start-in`.
///
/// Takes the tip rather than reading it, because *when* it is read is the whole
/// point: with `--register` the height that matters is the one after the
/// registration was mined, not the one at the start of the command.
///
/// `checked_add` rather than `+` or `saturating_add`. `--start-in` is a u32 a
/// caller types, and neither of the others fails usefully: a wrap lands in the
/// past, where the flow refuses it and the help blames the node, and a clamp
/// lands at the last block there can ever be — which is *after* the tip, so it
/// is accepted, and 200 VRSCTEST buys a currency whose conversions never open.
fn start_block_for(explicit: Option<u32>, start_in: u32, tip: u32) -> Result<u32, CurrencyError> {
    match explicit {
        Some(height) => Ok(height),
        None => tip
            .checked_add(start_in)
            .ok_or(CurrencyError::StartBlockUnreachable { tip, start_in }),
    }
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
    let found = read_currency(ui, &node, name)?;
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

/// A coin amount out of the raw definition, in satoshis.
///
/// The daemon renders money as a JSON float, which is the one representation
/// the SDK refuses to hold — so it is converted back at the boundary rather
/// than carried around or printed as-is. `1e-08` and `1000000.0` are both
/// exactly representable at this scale, and the round trip is well inside
/// f64's 53 bits for any supply a chain can hold.
fn coins_to_sat(value: &serde_json::Value) -> u64 {
    let coins = value.as_f64().unwrap_or(0.0).max(0.0);
    (coins * SATOSHIDEN as f64).round() as u64
}

fn definition_coins(found: &CurrencySummary, field: &str) -> u64 {
    found.definition.get(field).map_or(0, coins_to_sat)
}

/// The first reserve a non-empty cap vector caps at zero.
///
/// Zero is "nothing accepted", not "no limit": consensus refunds anything over
/// the cap, and once the vector exists at all a reserve nobody named is a zero
/// rather than an absence. An empty vector is never consulted, so naming no
/// reserves leaves every one of them uncapped — which is why this only looks at
/// a vector that is already non-empty.
fn capped_at_zero(caps: &[Amount]) -> Option<usize> {
    caps.iter().position(|cap| *cap == Amount::ZERO)
}

/// Whether a basket refuses every contribution paid in `source`.
///
/// Same rule as [`capped_at_zero`], asked from the other side: at preconvert
/// time the definition is already on chain, so the only useful question is
/// whether this particular payment can land.
fn reserve_accepts_nothing(reserves: &[String], caps: &[u64], source: &str) -> bool {
    if caps.is_empty() {
        return false;
    }
    reserves
        .iter()
        .position(|id| id == source)
        .and_then(|index| caps.get(index).copied())
        == Some(0)
}

/// How much more this reserve can take before the cap refunds a contribution,
/// or `None` when no cap applies to it.
///
/// Consensus compares the **cumulative** total against the cap and refunds the
/// whole transfer that crosses it — it does not trim to fit. So the useful
/// question is not "is my amount under the cap" but "is what is already in,
/// plus mine, under it".
///
/// `None` for an absent cap vector and for a cap of zero: the first means
/// uncapped, and the second is a different refusal with a different
/// explanation, handled before this is asked.
fn cap_room(reserves: &[String], caps: &[u64], source: &str, already: u64) -> Option<u64> {
    if caps.is_empty() {
        return None;
    }
    let index = reserves.iter().position(|id| id == source)?;
    let cap = caps.get(index).copied().filter(|cap| *cap != 0)?;
    Some(cap.saturating_sub(already))
}

/// What a reserve has taken in so far, in satoshis.
fn reserve_holds(found: &CurrencySummary, reserve: &str) -> u64 {
    found
        .definition
        .get("bestcurrencystate")
        .or_else(|| found.definition.get("lastconfirmedcurrencystate"))
        .and_then(|state| state.get("currencies"))
        .and_then(|legs| legs.get(reserve))
        .and_then(|leg| leg.get("reservein"))
        .map_or(0, coins_to_sat)
}

/// The reserve currency ids a definition carries, if any.
fn target_currencies(found: &CurrencySummary) -> Option<Vec<String>> {
    let list: Vec<String> = found
        .definition
        .get("currencies")?
        .as_array()?
        .iter()
        .filter_map(|v| v.as_str().map(str::to_string))
        .collect();
    (!list.is_empty()).then_some(list)
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

    // The *live* supply, not the one the definition was launched with. For a
    // mintable currency those diverge the moment it mints, and the launch
    // figure below is then the wrong answer to "how much of this exists".
    // Already in the payload `currency_definition` returned — no extra request.
    if let Some(supply) = found
        .definition
        .get("bestcurrencystate")
        .or_else(|| found.definition.get("lastconfirmedcurrencystate"))
        .and_then(|state| state.get("supply"))
    {
        let now = coins_to_sat(supply);
        let at_launch = found
            .definition
            .get("preallocations")
            .and_then(|v| v.as_array())
            .map(|list| {
                list.iter()
                    .filter_map(serde_json::Value::as_object)
                    .flat_map(|map| map.values())
                    .map(coins_to_sat)
                    .sum::<u64>()
            })
            .unwrap_or(0);
        let mut row = Text::of(fmt::amount(Amount::from_sat(now)), palette.value);
        if now != at_launch {
            row = row.push(
                format!("  {} at launch", fmt::amount(Amount::from_sat(at_launch))),
                palette.muted,
            );
        }
        panel = panel.row("supply", row);
    }
    if found.end_block != 0 {
        panel = panel.row(
            "ends",
            Text::of(
                format!("block {}", fmt::height(found.end_block.into())),
                palette.value,
            ),
        );
    }

    // A basket's reserves, which are the whole of what backs it — and the only
    // way to know what `preconvert --spend` will accept, since paying in
    // anything else is refunded rather than refused. `currencynames` comes back
    // in the same reply, so naming them costs no extra request.
    if let Some(list) = target_currencies(found) {
        panel = panel.section("RESERVES");
        let names = found.definition.get("currencynames");
        let weights: Vec<u64> = found
            .definition
            .get("weights")
            .and_then(|v| v.as_array())
            .map(|w| w.iter().map(coins_to_sat).collect())
            .unwrap_or_default();

        // Once a basket is live the state carries what it actually holds and
        // what a unit costs in each reserve. Keyed by currency id rather than
        // by position: `reservecurrencies` is not in the definition's order,
        // and reading it positionally would price each reserve as its
        // neighbour.
        let live: std::collections::BTreeMap<String, &serde_json::Value> = found
            .definition
            .get("bestcurrencystate")
            .or_else(|| found.definition.get("lastconfirmedcurrencystate"))
            .and_then(|state| state.get("reservecurrencies"))
            .and_then(|v| v.as_array())
            .map(|list| {
                list.iter()
                    .filter_map(|leg| {
                        leg.get("currencyid")
                            .and_then(serde_json::Value::as_str)
                            .map(|id| (id.to_string(), leg))
                    })
                    .collect()
            })
            .unwrap_or_default();

        // So the amounts line up rather than stepping with the name lengths.
        let widest = list
            .iter()
            .filter_map(|id| names.and_then(|map| map.get(id)))
            .filter_map(serde_json::Value::as_str)
            .map(|name| {
                unicode_width::UnicodeWidthStr::width(
                    fmt::untrusted(name, NAME_BUDGET, glyphs.ellipsis).as_str(),
                )
            })
            .max()
            .unwrap_or(0);

        for (index, id) in list.iter().enumerate() {
            let named = names
                .and_then(|map| map.get(id))
                .and_then(serde_json::Value::as_str);
            let leg = live.get(id);

            // The live weight when there is one: a basket's ratios drift from
            // the definition as people convert, and the definition's figure is
            // then the target rather than the truth.
            let share = leg
                .and_then(|leg| leg.get("weight"))
                .map(coins_to_sat)
                .or_else(|| weights.get(index).copied())
                .filter(|scaled| *scaled != 0)
                .map(weight_percent)
                .unwrap_or_default();

            let mut row = Text::of(share, palette.value);
            if let Some(name) = named {
                let shown = fmt::untrusted(name, NAME_BUDGET, glyphs.ellipsis);
                let pad = " ".repeat(
                    widest.saturating_sub(unicode_width::UnicodeWidthStr::width(shown.as_str())),
                );
                row = row
                    .push("  ", palette.muted)
                    .push(shown, palette.accent)
                    .push(pad, palette.muted);
            }
            if let Some(leg) = leg {
                let held = leg.get("reserves").map_or(0, coins_to_sat);
                row = row.push(
                    format!("  holds {}", fmt::amount(Amount::from_sat(held))),
                    palette.muted,
                );
                // What one unit of the basket costs in this reserve — the
                // number anyone asking "what is it worth" wants, and the one
                // the panel had no answer for.
                let price = leg.get("priceinreserve").map_or(0, coins_to_sat);
                if price != 0 {
                    row = row.push(
                        format!(
                            "  ·  1 {} = {}",
                            fmt::untrusted(&found.name, NAME_BUDGET, glyphs.ellipsis),
                            fmt::amount(Amount::from_sat(price))
                        ),
                        palette.value,
                    );
                }
            } else {
                row = row
                    .push("  ", palette.muted)
                    .push(fmt::address(id, glyphs.ellipsis), palette.muted);
            }
            panel = panel.row("", row);
        }
    }

    // The sub-identity policy: what registering a name under this currency
    // costs, how deep referrals pay, and whether one is required at all. It is
    // the whole point of a currency defined to govern registrations, and the
    // launch panel already shows it — leaving it off `show` meant the one
    // command for "what is this" could not answer the question it was made for.
    let fee = definition_coins(found, "idregistrationfees");
    let levels = found
        .definition
        .get("idreferrallevels")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(0);
    let import = definition_coins(found, "idimportfees");
    if fee != 0 || levels != 0 || import != 0 {
        panel = panel.section("SUB-IDENTITIES");
        if fee != 0 {
            panel = panel.row(
                "registration",
                Text::of(fmt::amount(Amount::from_sat(fee)), palette.value),
            );
        }
        if levels != 0 {
            panel = panel.row(
                "referrals",
                Text::of(
                    fmt::plural(levels as usize, "level", "levels"),
                    palette.value,
                )
                .push("  ", palette.muted)
                .push(
                    if found.options & option::ID_REFERRALREQUIRED != 0 {
                        "and one is mandatory"
                    } else {
                        "optional"
                    },
                    palette.muted,
                ),
            );
        }
        if import != 0 {
            panel = panel.row(
                "import",
                Text::of(fmt::amount(Amount::from_sat(import)), palette.value),
            );
        }
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
            let mut total = 0u64;
            for entry in list {
                let Some(map) = entry.as_object() else {
                    continue;
                };
                for (recipient, amount) in map {
                    let sats = coins_to_sat(amount);
                    total += sats;
                    panel = panel.row(
                        "",
                        Text::of(fmt::amount(Amount::from_sat(sats)), palette.value)
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
                format!(
                    "{} in total at launch",
                    fmt::amount(Amount::from_sat(total))
                ),
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

    // Refused here, with the other local refusals, for the reason above: a flag
    // that can never be honoured should cost neither a passphrase prompt nor a
    // node. The SDK's launch builder emits seven outputs and none of them funds
    // a contribution, and the launch notarization it publishes in the same
    // transaction says the reserves hold nothing — so honouring the flag would
    // put a permanent claim of backing on chain with nothing behind it.
    //
    // The guard reads `args.contribute` and never `definition.initial_contributions`:
    // the --nft path sets that field to `[Amount::ZERO]` for byte-shape parity,
    // and consensus refuses an NFT definition whose per-reserve vectors are
    // absent. A guard on the field would break every NFT launch, and both NFT
    // tests are #[ignore]d, so nothing offline would catch it.
    if !args.contribute.is_empty() {
        return Err(CurrencyError::ContributionUnfunded.into());
    }

    // The same shape of refusal one flag along, for a different reason: this one
    // is consensus rather than an SDK gap, so unlike --contribute it never goes
    // away. A basket's pre-launch price is computed at launch from
    // `initial_supply` and the weights — the formula BasketNeedsSupply names a
    // few lines up — and written into the launch notarization;
    // `definition.conversions` is not an input to it, and the daemon zeroes the
    // field on the way in. Honouring the flag would write a number nothing reads
    // into a definition that can never be changed, while the panel reported it
    // back as the price.
    //
    // Reads `args.conversion` and never `definition.conversions`, for the same
    // reason as the guard above: the --nft path sets that field to
    // `[Amount::ZERO]` for byte-shape parity, and a guard on the field would
    // refuse every NFT launch — both NFT tests are #[ignore]d, so nothing
    // offline would catch it.
    if !args.conversion.is_empty() {
        return Err(CurrencyError::LaunchPriceDerived.into());
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
    // Needed by both the `--supply` sugar and `--nft`, so read once.
    // Before anything reads the identity, since with --register there may not
    // be one yet. A no-op when it already exists, so a re-run costs one lookup.
    if args.register {
        super::id::ensure_exists(
            ui,
            settings,
            globals,
            &node,
            &args.name,
            args.from.as_deref(),
            args.register_timeout,
        )?;
    }

    // Read here, below the registration, rather than up with the other chain
    // reads. `--register` blocks for up to `--register-timeout` twice over —
    // once for the commitment to confirm, once for the registration to be
    // mined — so a height read above it is minutes and blocks stale by the time
    // the definition is built against it. `prepare_launch` re-reads the tip and
    // refuses `start_block <= tip`, which is how a launch measured from the
    // older height came back as "start_block N is not after the tip M" for a
    // chain that had simply moved on, on the default --start-in as well.
    ui.sdk("node.block_count()");
    let tip = node.block_count().map_err(|source| {
        node::NodeError::request("reading the tip", &settings.profile.node, source)
    })?;
    ui.sdk_result(fmt::height(tip.into()));

    // A launch cannot start in the past, and the flow refuses one that would.
    // The default leaves room for the transaction to be mined and for a human
    // to notice a mistake before conversions open.
    let start_block = start_block_for(args.start_block, args.start_in, tip)?;

    let defining = {
        ui.sdk(format!("node.identity({:?})", args.name));
        let record = node
            .identity(&args.name)
            .map_err(|source| flow("reading the defining identity", FlowError::Rpc(source)))?;
        ui.sdk_result(record.identity_address.clone());
        record
            .identity_address
            .parse::<Address>()
            .map_err(|_| CurrencyError::NotFound {
                name: args.name.clone(),
            })?
            .hash()
    };

    if let Some(amount) = supply.filter(|_| !fractional) {
        preallocations.push(verus_sdk::currency::Preallocation {
            recipient: defining,
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
        // `conversions` is deliberately not among these: the launch price is
        // derived at launch from the supply and the weights, so the field is
        // read by nothing and the daemon zeroes it. Left at the empty vector
        // `token()` builds, which is what every basket this has launched wrote.
        definition.min_preconversion = per_reserve(&args.min_preconvert, &names, "min-preconvert")?;
        definition.max_preconversion = per_reserve(&args.max_preconvert, &names, "max-preconvert")?;

        // A basket that names --max-preconvert for some reserves and not others
        // cannot launch, and this is the last moment anyone can say so: after
        // the definition is on chain nothing about it can be changed.
        //
        // A cap of zero is not "no limit", it is "nothing accepted" — consensus
        // refunds any contribution over the cap, and a missing entry is a zero
        // rather than an absence once the vector exists at all. Meanwhile a
        // fractional basket refunds the *whole launch* unless every one of its
        // reserves receives something. So one unnamed reserve here is a launch
        // that is already lost, 200 VRSCTEST included, and it fails silently
        // hours later at the start block rather than now.
        //
        // Naming none of them is fine and common: an empty vector is never
        // consulted, so every reserve is uncapped.
        if let Some(starved) = capped_at_zero(&definition.max_preconversion) {
            return Err(CurrencyError::ReserveCappedAtZero {
                reserve: names[starved].to_string(),
                what: "max-preconvert",
            }
            .into());
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

        // One satoshi, which is the whole of what makes it non-fungible.
        definition.preallocations = vec![verus_sdk::currency::Preallocation {
            recipient: defining,
            amount: Amount::from_sat(1),
        }];

        // An NFT is a *currency-mapped* token, not a bare one: it carries the
        // parent in `currencies` while leaving FRACTIONAL clear. Every NFT on
        // VRSCTEST looks like this, and without it consensus refuses the whole
        // transaction as `bad-txns-failed-precheck` — a precheck naming neither
        // the field nor what it wanted.
        //
        // The per-reserve vectors go alongside at the same length, since
        // `serialize_definition` takes zero or one entry per currency.
        definition.currencies = vec![parent];
        definition.conversions = vec![Amount::ZERO];
        definition.max_preconversion = vec![Amount::ZERO];
        definition = definition.with_contributions(vec![Amount::ZERO]);
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

    // An NFT does not cost what a currency costs. The parent's
    // `currencyregistrationfee` — 200 native, which is what the flow reads when
    // nothing is pinned — is the price of a token or a basket; an NFT is
    // charged the parent's `idimportfees` instead, 0.02 on VRSCTEST. Paying the
    // wrong one builds a definition consensus refuses as
    // `bad-txns-failed-precheck`, naming neither the fee nor the amount.
    let pinned_fee = if args.nft {
        let parent_name = Address::new(AddressKind::Identity, parent.to_bytes()).to_string();
        ui.sdk(format!("node.currency({parent_name:?}).id_import_fee"));
        let fee = node
            .currency(&parent_name)
            .map_err(|source| flow("reading the parent's fee policy", FlowError::Rpc(source)))?
            .id_import_fee;
        ui.sdk_result(fmt::amount(fee));
        Some(fee)
    } else {
        None
    };

    let secret = keystore::passphrase(&format!("passphrase for `{}`", envelope.label), false)?;
    let key = envelope.unlock(&secret)?;

    ui.sdk(format!(
        "verus_sdk::network::prepare_launch(&node, &[&key], {:?}, &definition, {:?})",
        args.name, pinned_fee
    ));
    let unsent = prepare_launch(&node, &[&key], &args.name, &definition, pinned_fee)
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
            .map_err(|source| nft_aware(args.nft, "broadcasting the launch", source))?;
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
        .map_err(|source| nft_aware(args.nft, "broadcasting the launch", source))?;
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
            // preconversion limit is money and belongs where the ratio is
            // rather than in a separate list to cross-refer.
            for (label, values) in [
                ("min", &definition.min_preconversion),
                ("max", &definition.max_preconversion),
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

/// `pecu currency mint`.
///
/// # Why minting is a property of the identity, not of the token
///
/// A centralized currency is one whose `proofprotocol` is 2 — `CHAINID` — and
/// that single number is the whole permission system. Consensus accepts new
/// supply only from a transaction that **spends an output the controlling
/// identity holds**, and the controlling identity is the currency: same
/// i-address. So a mint is not "call mint on the token", it is "prove you are
/// the identity by spending its coins".
///
/// That has a consequence worth stating before a user meets it as a rejection:
/// **the identity pays, not the signing key.** A wallet with a well-funded key
/// and an empty identity cannot mint. `pecu send --to <name@>` is the fix, and
/// the error below says so rather than reporting an empty balance.
pub fn mint(
    ui: &Ui,
    settings: &Settings,
    globals: &Globals,
    args: &CurrencyMintArgs,
) -> miette::Result<()> {
    if !settings.profile.allow_spend {
        return Err(CurrencyError::SpendingDisabled {
            profile: settings.profile.name.clone(),
        }
        .into());
    }

    // Parsed before a node is asked or a passphrase prompted: a typo'd amount
    // should not cost a round trip, and it certainly should not cost a
    // passphrase.
    let amount = Amount::from_coins_str(&args.amount).map_err(|_| CurrencyError::BadAmount {
        value: args.amount.clone(),
    })?;
    if amount == Amount::ZERO {
        return Err(CurrencyError::MintsNothing.into());
    }
    let fee = Amount::from_coins_str(&args.fee).map_err(|_| CurrencyError::BadAmount {
        value: args.fee.clone(),
    })?;

    // The SDK refuses a non-R recipient, and it is right to: the destination is
    // written as a bare key hash, so an i-address would silently pay the
    // R-address with the same hash — an address nobody holds the key to. Caught
    // here so the message can name the flag rather than the field.
    // A VerusID name is the likeliest wrong answer here, and it does not parse
    // as an address at all — so it has to be recognised as a name rather than
    // falling through to "that is not an address", which is true but unhelpful.
    let looks_like_an_identity = args.to.ends_with('@') || args.to.contains('.');
    let wrong_kind = CurrencyError::MintNeedsTransparentRecipient {
        value: args.to.clone(),
    };
    match args.to.parse::<Address>() {
        Ok(recipient) if recipient.kind() == AddressKind::PubKeyHash => {}
        Ok(_) => return Err(wrong_kind.into()),
        Err(_) if looks_like_an_identity => return Err(wrong_kind.into()),
        Err(_) => {
            return Err(CurrencyError::NotAnAddress {
                value: args.to.clone(),
            }
            .into())
        }
    }

    let node = node::connect(&settings.profile)?;
    let looked_up = args.currency.strip_suffix('@').unwrap_or(&args.currency);
    ui.sdk(format!("node.currency_definition({looked_up:?})"));
    let found = node
        .currency_definition(looked_up)
        .map_err(|_| CurrencyError::NotFound {
            name: args.currency.clone(),
        })?;
    ui.sdk_result(format!(
        "CurrencySummary {{ {}, proof_protocol: {} }}",
        found.currency_id, found.proof_protocol
    ));

    // Both refusals are local and permanent — no key, no node round, no
    // "try again". A decentralized token's supply is fixed by its definition
    // and there is no authority that could add to it.
    if found.options & option::FRACTIONAL != 0 {
        return Err(CurrencyError::CannotMintABasket {
            name: found.name.clone(),
        }
        .into());
    }
    if found.proof_protocol != 2 {
        return Err(CurrencyError::NotCentralized {
            name: found.name.clone(),
        }
        .into());
    }

    let store = Keystore::new(&settings.paths);
    let envelope = choose_key(&store, args.from.as_deref())?;
    let secret = keystore::passphrase(&format!("passphrase for `{}`", envelope.label), false)?;
    let key = envelope.unlock(&secret)?;

    // The i-address, not the name: `identity_held` parses this as an address to
    // build the pay-to-identity script it looks for, and a `name@` does not
    // parse.
    ui.sdk(format!(
        "verus_sdk::network::prepare_mint(&node, &key, {:?}, {}, {:?}, {})",
        found.currency_id,
        amount.to_sat(),
        args.to,
        fee.to_sat()
    ));
    let unsent = prepare_mint(&node, &key, &found.currency_id, amount, &args.to, fee)
        .map_err(|source| mint_flow(&found.name, source))?;
    ui.sdk_result(format!("Unsent<Sent> {{ txid: {} }}", unsent.txid));

    let review = mint_panel(
        ui,
        &found,
        amount,
        &args.to,
        &envelope,
        fee,
        &unsent.txid,
        globals.dry_run,
    );

    if ui.is_json() {
        let document = serde_json::json!({
            "currency": found.name,
            "currency_id": found.currency_id,
            "amount": amount.to_sat(),
            "to": args.to,
            "fee": fee.to_sat(),
            "txid": unsent.txid,
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
            .map_err(|source| mint_flow(&found.name, source))?;
        emit(&serde_json::json!({
            "currency": found.name,
            "currency_id": found.currency_id,
            "amount": amount.to_sat(),
            "to": args.to,
            "fee": done.fee.to_sat(),
            "txid": done.txid,
            "broadcast": true,
        }));
        return Ok(());
    }

    ui.panel(&review);
    if globals.dry_run {
        ui.blank();
        ui.note("nothing was sent. Drop --dry-run to mint it");
        ui.explain_panel();
        return Ok(());
    }
    if !globals.yes {
        confirm(ui)?;
    }

    ui.sdk("unsent.broadcast(&node)");
    let done = unsent
        .broadcast(&node)
        .map_err(|source| mint_flow(&found.name, source))?;
    ui.blank();
    ui.ok(format!("minted — txid {}", done.txid));
    ui.note(format!("https://testex.verus.io/tx/{}", done.txid));
    ui.explain_panel();
    Ok(())
}

/// The prechecks `prepare_mint` makes are the chain's, and every one of them
/// has a specific thing the user should do next. Sending any of them to
/// `pecu doctor` would be wrong twice: the node is fine, and no retry helps.
fn mint_flow(name: &str, source: FlowError) -> CurrencyError {
    use verus_sdk::verus_tx::TxError;
    let advice = match &source {
        // The commonest failure by far, and the least guessable: the signing
        // key's balance is irrelevant, because a mint is authorised by what the
        // *identity* spends.
        FlowError::NotReady(message) if message.contains("no spendable outputs") => format!(
            "a mint is paid for by the identity, not by the signing key — consensus accepts new \
             supply only from a transaction that spends what the identity holds. Send it some \
             native coins first: `pecu send --to {name}@ --amount 1`"
        ),
        FlowError::Tx(TxError::NotAPrimaryAddress { .. }) => format!(
            "the signing key must be one of {name}@'s primary addresses — `pecu id show {name}@` \
             lists them, and --from picks which stored key signs"
        ),
        FlowError::Tx(TxError::AlreadyRevoked) => format!(
            "{name}@ is revoked, and a revoked identity mints nothing. Recover it first — \
             `pecu id recover {name}@`"
        ),
        FlowError::Tx(TxError::NotEnoughSigners { required, .. }) => format!(
            "{name}@ needs {required} signatures and this signs with one. Multi-signature \
             minting is not wired up here yet"
        ),
        FlowError::NoSuchIdentity(_) => format!(
            "a currency is controlled by the identity that defined it, and nothing on this chain \
             is called {name}@"
        ),
        _ => "run `pecu doctor`, or point somewhere else with --node".to_string(),
    };
    CurrencyError::Flow {
        what: "minting",
        advice,
        source: Box::new(source),
    }
}

#[allow(clippy::too_many_arguments)]
fn mint_panel(
    ui: &Ui,
    found: &CurrencySummary,
    amount: Amount,
    to: &str,
    envelope: &Envelope,
    fee: Amount,
    txid: &str,
    dry_run: bool,
) -> Panel {
    let palette = ui.theme.palette;
    let glyphs = ui.theme.glyphs;

    Panel::new(if dry_run { "WOULD MINT" } else { "MINT" })
        .row(
            "currency",
            Text::of(
                fmt::untrusted(&found.fully_qualified_name, NAME_BUDGET, glyphs.ellipsis),
                palette.accent,
            ),
        )
        .row("currency id", Text::of(&found.currency_id, palette.value))
        .row(
            "amount",
            Text::of(fmt::amount(amount), palette.accent)
                .push("  new supply, created by this", palette.muted),
        )
        .row("to", Text::of(to, palette.value))
        // Named separately from the signer because they are different things
        // and the difference is the one that trips people: the identity's coins
        // pay, the key merely proves it may.
        .row(
            "paid by",
            Text::of(
                fmt::untrusted(&found.name, NAME_BUDGET, glyphs.ellipsis),
                palette.value,
            )
            .push("  the identity's own coins, not the key's", palette.muted),
        )
        .row(
            "signed by",
            Text::of(&envelope.label, palette.value)
                .push("  ", palette.muted)
                .push(
                    fmt::address(&envelope.address, glyphs.ellipsis),
                    palette.muted,
                ),
        )
        .row("fee", Text::of(fmt::amount(fee), palette.value))
        .row("txid", Text::of(txid, palette.muted))
        .note(Text::of(
            "this currency is centralized — its supply is whatever its identity decides, and \
             every holder is trusting that",
            palette.muted,
        ))
}

/// `pecu currency preconvert`.
///
/// # Buying something that does not exist yet
///
/// A preconversion is not a purchase at a price. A launching currency has no
/// reserves, so there is nothing to price against: what you receive is decided
/// **at launch**, from the final ratio of everyone's contributions together.
/// Convert twice with identical arguments a day apart and the two can pay out
/// differently, because other people contributed in between.
///
/// Two consequences the panel has to state rather than imply:
///
/// * **There is no estimate, and no slippage floor.** The SDK refuses a
///   `min_expected` here by name, because a floor could only be checked against
///   a number nobody produced. Anything this printed as "you will receive"
///   would be invented.
/// * **A failed launch refunds you.** If the currency misses its
///   `min_preconversion` by its start block, every contribution goes back —
///   which is what makes contributing early safe, and why the refund address
///   matters more here than anywhere else.
///
/// Preconvert and convert are not interchangeable at any height: before the
/// start block a plain conversion is refused for want of reserves, and after it
/// a preconversion is refused in turn.
pub fn preconvert(
    ui: &Ui,
    settings: &Settings,
    globals: &Globals,
    args: &CurrencyPreconvertArgs,
) -> miette::Result<()> {
    if !settings.profile.allow_spend {
        return Err(CurrencyError::SpendingDisabled {
            profile: settings.profile.name.clone(),
        }
        .into());
    }

    let amount = Amount::from_coins_str(&args.amount).map_err(|_| CurrencyError::BadAmount {
        value: args.amount.clone(),
    })?;
    if amount == Amount::ZERO {
        return Err(CurrencyError::ConvertsNothing.into());
    }
    let fee = Amount::from_coins_str(&args.fee).map_err(|_| CurrencyError::BadAmount {
        value: args.fee.clone(),
    })?;

    let store = Keystore::new(&settings.paths);
    let envelope = choose_key(&store, args.from.as_deref())?;

    // Defaults to the paying key, which is what somebody buying for themselves
    // wants. Checked whether given or defaulted: the SDK writes the recipient
    // as a bare key hash, so an identity would pay the R-form of the same bytes.
    let recipient = args.to.clone().unwrap_or_else(|| envelope.address.clone());
    let looks_like_an_identity = recipient.ends_with('@') || recipient.contains('.');
    let wrong_kind = CurrencyError::ConvertNeedsTransparentRecipient {
        value: recipient.clone(),
    };
    match recipient.parse::<Address>() {
        Ok(parsed) if parsed.kind() == AddressKind::PubKeyHash => {}
        Ok(_) => return Err(wrong_kind.into()),
        Err(_) if looks_like_an_identity => return Err(wrong_kind.into()),
        Err(_) => {
            return Err(CurrencyError::NotAnAddress {
                value: recipient.clone(),
            }
            .into())
        }
    }

    let node = node::connect(&settings.profile)?;
    let target = read_currency(ui, &node, &args.currency)?;
    ui.sdk_result(format!(
        "CurrencySummary {{ {}, start_block: {} }}",
        target.currency_id, target.start_block
    ));

    // The rule that decides which of the two commands is even legal, so it is
    // worth answering locally with the block number rather than letting the
    // chain refuse anonymously.
    ui.sdk("node.block_count()");
    let tip = node
        .block_count()
        .map_err(|source| flow("reading the tip", FlowError::Rpc(source)))?;
    ui.sdk_result(tip.to_string());
    if u64::from(target.start_block) <= u64::from(tip) {
        return Err(CurrencyError::AlreadyLaunched {
            name: target.name.clone(),
            start_block: target.start_block,
            tip,
        }
        .into());
    }

    // What is being spent, and whether it is even one of this currency's
    // reserves. Consensus refunds a preconversion in a currency the target does
    // not hold, which costs a round trip and a wait to discover.
    let spend = match &args.spend {
        Some(name) => name.clone(),
        None => settings.profile.currency.clone(),
    };
    let source_id = resolve_reserve(ui, &node, &spend)?;
    let reserves: Vec<String> = target
        .definition
        .get("currencies")
        .and_then(|v| v.as_array())
        .map(|list| {
            list.iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    let source_text = Address::new(AddressKind::Identity, source_id.to_bytes()).to_string();
    if !reserves.is_empty() && !reserves.contains(&source_text) {
        return Err(CurrencyError::NotOneOfItsReserves {
            spend: spend.clone(),
            name: target.name.clone(),
            reserves: reserves.len(),
        }
        .into());
    }

    // A cap of zero on this reserve means every satoshi sent here comes back.
    // Consensus refunds rather than refuses, so without this the money leaves,
    // sits until the import, and returns — with nothing said in between.
    let caps: Vec<u64> = target
        .definition
        .get("maxpreconversion")
        .and_then(|v| v.as_array())
        .map(|list| list.iter().map(coins_to_sat).collect())
        .unwrap_or_default();
    if reserve_accepts_nothing(&reserves, &caps, &source_text) {
        return Err(CurrencyError::ReserveAcceptsNothing {
            spend: spend.clone(),
            name: target.name.clone(),
        }
        .into());
    }

    // Over the cap is refunded **whole**, not trimmed. Send 100 against a cap
    // of 50 and the leg ends with nothing in it — and a fractional basket
    // refunds the entire launch unless every reserve receives something, so one
    // oversized contribution loses everyone's.
    //
    // Best effort by nature: the cap is on the cumulative total, and another
    // contribution already in flight is invisible here. Catching the case where
    // the arithmetic is plainly wrong is still worth far more than it costs.
    let already = reserve_holds(&target, &source_text);
    if let Some(room) = cap_room(&reserves, &caps, &source_text, already) {
        if amount.to_sat() > room {
            let cap = caps[reserves
                .iter()
                .position(|id| *id == source_text)
                .unwrap_or(0)];
            return Err(CurrencyError::OverThePreconvertCap {
                spend: spend.clone(),
                name: target.name.clone(),
                cap: fmt::amount(Amount::from_sat(cap)),
                already: fmt::amount(Amount::from_sat(already)),
                room: fmt::amount(Amount::from_sat(room)),
            }
            .into());
        }
    }

    let secret = keystore::passphrase(&format!("passphrase for `{}`", envelope.label), false)?;
    let key = envelope.unlock(&secret)?;

    // Empty when spending the chain's own currency; otherwise every output
    // carrying the token, since each is spent whole and the surplus returns as
    // change. A token left out is a token destroyed.
    let token_funding = if source_id == native_currency_id(&node, ui)? {
        Vec::new()
    } else {
        ui.sdk(format!(
            "verus_sdk::network::spendable(&node, {:?})",
            envelope.address
        ));
        let funding = spendable(&node, &envelope.address)
            .map_err(|source| flow("reading the address's outputs", source))?;
        let held = super::send::carrying(&funding.other, source_id);
        if held.is_empty() {
            return Err(CurrencyError::NoTokenToSpend {
                spend: spend.clone(),
                address: envelope.address.clone(),
            }
            .into());
        }
        ui.sdk_result(format!("{} output(s) carrying {}", held.len(), spend));
        held
    };

    ui.sdk(format!(
        "verus_sdk::network::prepare_conversion(&node, &key, {source_text:?}, {}, \
         ConversionKind::Preconvert {{ fractional: {} }}, {recipient:?}, {}, None, &funding)",
        amount.to_sat(),
        target.currency_id,
        fee.to_sat()
    ));
    let unsent = prepare_conversion(
        &node,
        &key,
        // The i-address, not the name the user typed: `currency_of` parses this
        // as an address, so `VRSCTEST` reaches it as a failed base58 decode
        // rather than as the chain's own currency.
        &source_text,
        amount,
        // `None` for the floor, and not by omission: a pre-launch currency has
        // no market, so the SDK refuses a `min_expected` here rather than check
        // one against a fabricated estimate.
        ConversionKind::Preconvert {
            fractional: CurrencyId::from_bytes(
                target
                    .currency_id
                    .parse::<Address>()
                    .map_err(|_| CurrencyError::NotFound {
                        name: args.currency.clone(),
                    })?
                    .hash(),
            ),
        },
        &recipient,
        fee,
        None,
        &token_funding,
    )
    .map_err(|source| convert_flow(&target.name, source))?;
    ui.sdk_result(format!("Unsent<Sent> {{ txid: {} }}", unsent.txid));

    let review = preconvert_panel(
        ui,
        &target,
        &spend,
        &source_text,
        amount,
        &recipient,
        &envelope,
        fee,
        &unsent.txid,
        tip,
        globals.dry_run,
    );

    if ui.is_json() {
        let document = serde_json::json!({
            "currency": target.name,
            "currency_id": target.currency_id,
            "spend": spend,
            "amount": amount.to_sat(),
            "to": recipient,
            "fee": fee.to_sat(),
            "start_block": target.start_block,
            "txid": unsent.txid,
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
            .map_err(|source| convert_flow(&target.name, source))?;
        emit(&serde_json::json!({
            "currency": target.name,
            "currency_id": target.currency_id,
            "spend": spend,
            "amount": amount.to_sat(),
            "to": recipient,
            "fee": done.fee.to_sat(),
            "start_block": target.start_block,
            "txid": done.txid,
            "broadcast": true,
        }));
        return Ok(());
    }

    ui.panel(&review);
    if globals.dry_run {
        ui.blank();
        ui.note("nothing was sent. Drop --dry-run to preconvert");
        ui.explain_panel();
        return Ok(());
    }
    if !globals.yes {
        confirm(ui)?;
    }

    ui.sdk("unsent.broadcast(&node)");
    let done = unsent
        .broadcast(&node)
        .map_err(|source| convert_flow(&target.name, source))?;
    ui.blank();
    ui.ok(format!("preconverted — txid {}", done.txid));
    ui.note(format!(
        "what this pays out is settled at block {}",
        fmt::height(target.start_block.into())
    ));
    ui.note(format!("https://testex.verus.io/tx/{}", done.txid));
    ui.explain_panel();
    Ok(())
}

/// The chain's own currency id, for deciding whether a source needs token
/// funding at all.
fn native_currency_id(node: &crate::node::Node, ui: &Ui) -> Result<CurrencyId, miette::Report> {
    ui.sdk("node.chain_info().chain_id");
    let info = node
        .chain_info()
        .map_err(|source| flow("reading the chain id", FlowError::Rpc(source)))?;
    let address: Address = info.chain_id.parse().map_err(|_| CurrencyError::NotFound {
        name: info.chain_id.clone(),
    })?;
    Ok(CurrencyId::from_bytes(address.hash()))
}

fn convert_flow(name: &str, source: FlowError) -> CurrencyError {
    use verus_sdk::verus_tx::TxError;
    let advice = match &source {
        FlowError::Tx(TxError::InsufficientFunds { .. }) | FlowError::InsufficientFunds { .. } => {
            "the amount, the reserve transfer fee and the miner fee all come from the signing \
             key — `pecu wallet balance --key <label>` shows what it holds"
                .to_string()
        }
        // The floor is a convert-only concept, and this is the one refusal here
        // that a caller can act on directly.
        FlowError::NotReady(message) if message.starts_with("the node expects") => {
            "the node's estimate is below --min-out, so nothing was signed. A basket's price \
             moves with every conversion that lands, so this can change on its own — lower the \
             floor to accept the current price, or try again later"
                .to_string()
        }
        FlowError::NotReady(message) if message.contains("floor") => {
            "a preconversion has no price to check a floor against; this is a bug in pecu if you \
             see it, since it never sets one"
                .to_string()
        }
        _ => format!(
            "what {name} pays out is decided by the chain, not here, so there is nothing to \
             retry into a better answer. `pecu doctor` if the node itself looks wrong"
        ),
    };
    CurrencyError::Flow {
        what: "preconverting",
        advice,
        source: Box::new(source),
    }
}

#[allow(clippy::too_many_arguments)]
fn preconvert_panel(
    ui: &Ui,
    target: &CurrencySummary,
    spend: &str,
    source_id: &str,
    amount: Amount,
    recipient: &str,
    envelope: &Envelope,
    fee: Amount,
    txid: &str,
    tip: u32,
    dry_run: bool,
) -> Panel {
    let palette = ui.theme.palette;
    let glyphs = ui.theme.glyphs;

    let mut panel = Panel::new(if dry_run {
        "WOULD PRECONVERT"
    } else {
        "PRECONVERT"
    })
    .row(
        "into",
        Text::of(
            fmt::untrusted(&target.fully_qualified_name, NAME_BUDGET, glyphs.ellipsis),
            palette.accent,
        ),
    )
    .row("currency id", Text::of(&target.currency_id, palette.value))
    .row(
        "spending",
        Text::of(fmt::amount(amount), palette.accent)
            .push("  ", palette.muted)
            .push(
                fmt::untrusted(spend, NAME_BUDGET, glyphs.ellipsis),
                palette.muted,
            ),
    )
    // No "you will receive" row, deliberately. There is no estimate to put in
    // it, and inventing one is worse than leaving the question open.
    .row(
        "you receive",
        Text::of("settled at launch", palette.warn).push(
            "  from the final ratio of every contribution",
            palette.muted,
        ),
    )
    .row("to", Text::of(recipient, palette.value))
    .row(
        "launches",
        Text::of(
            format!("block {}", fmt::height(target.start_block.into())),
            palette.value,
        )
        .push(
            format!(
                "  {} to go",
                fmt::plural(
                    u64::from(target.start_block).saturating_sub(u64::from(tip)) as usize,
                    "block",
                    "blocks"
                )
            ),
            palette.muted,
        ),
    )
    .row(
        "signed by",
        Text::of(&envelope.label, palette.value)
            .push("  ", palette.muted)
            .push(
                fmt::address(&envelope.address, glyphs.ellipsis),
                palette.muted,
            ),
    )
    .row("fee", Text::of(fmt::amount(fee), palette.value))
    .row("txid", Text::of(txid, palette.muted));

    // Which legs are already funded, and which are not. A fractional basket
    // refunds the *entire* launch unless every reserve receives something, so a
    // reserve sitting at zero is the difference between this contribution
    // working and coming back — and it is invisible unless something says so.
    let state = target
        .definition
        .get("bestcurrencystate")
        .or_else(|| target.definition.get("lastconfirmedcurrencystate"));
    if let (Some(reserves), Some(legs)) = (
        target_currencies(target),
        state
            .and_then(|s| s.get("currencies"))
            .and_then(|c| c.as_object()),
    ) {
        let names = target.definition.get("currencynames");
        let mut empty = Vec::new();
        panel = panel.section("RESERVES SO FAR");
        for id in &reserves {
            let held = legs
                .get(id)
                .and_then(|leg| leg.get("reservein"))
                .map_or(0, coins_to_sat);
            let label = names
                .and_then(|map| map.get(id))
                .and_then(serde_json::Value::as_str)
                .unwrap_or(id);
            if held == 0 {
                empty.push(fmt::untrusted(label, NAME_BUDGET, glyphs.ellipsis));
            }
            panel = panel.row(
                "",
                Text::of(
                    fmt::amount(Amount::from_sat(held)),
                    if held == 0 {
                        palette.warn
                    } else {
                        palette.value
                    },
                )
                .push("  ", palette.muted)
                .push(
                    fmt::untrusted(label, NAME_BUDGET, glyphs.ellipsis),
                    palette.muted,
                ),
            );
        }
        // Excludes the reserve this transaction funds: it is about to stop
        // being empty, and warning about it would be noise.
        let still_empty: Vec<_> = empty
            .iter()
            .filter(|label| label.as_str() != spend)
            .cloned()
            .collect();
        if !still_empty.is_empty() {
            panel = panel.note(Text::of(
                format!(
                    "nothing has gone into {} yet, and a basket refunds the whole launch unless                      every reserve receives something before its start block",
                    still_empty.join(", ")
                ),
                palette.warn,
            ));
        }
    }

    // The floor the launch has to clear, and the ceiling past which a
    // contribution comes back. Both are per reserve and both change what this
    // transaction is worth, so they belong in front of the confirmation.
    //
    // For **this** reserve, not summed across all of them: the limits are
    // enforced per reserve, so a total was a number consensus never compares
    // anything against and read as headroom that does not exist.
    let index = target_currencies(target)
        .and_then(|ids| ids.iter().position(|id| id == source_id).map(|i| (ids, i)));
    if let Some((_, index)) = index {
        for (field, label) in [
            ("minpreconversion", "this leg needs"),
            ("maxpreconversion", "this leg accepts"),
        ] {
            let value = target
                .definition
                .get(field)
                .and_then(|v| v.as_array())
                .and_then(|list| list.get(index))
                .map_or(0, coins_to_sat);
            if value != 0 {
                panel = panel.row(
                    label,
                    Text::of(fmt::amount(Amount::from_sat(value)), palette.value)
                        .push(format!("  of {spend}"), palette.muted),
                );
            }
        }
    }

    panel
        .note(Text::of(
            "if the launch misses its minimum, every contribution is refunded — including this \
             one, to the paying key",
            palette.muted,
        ))
        .note(Text::of(
            "over the maximum is refunded too, rather than refused, so this can come back even \
             if the launch succeeds",
            palette.muted,
        ))
}

/// `pecu currency convert`.
///
/// # Three shapes, one call
///
/// After a basket launches, value moves three ways and consensus writes all of
/// them as the same `CReserveTransfer`, differing only in which currency each
/// slot names:
///
/// * a reserve **into** the basket — `--spend VRSCTEST mybasket@`
/// * the basket back **into** a reserve — `--spend mybasket@ VRSCTEST`
/// * one reserve **through** the basket into another — `--via mybasket@`
///
/// Which one is meant is inferable from the definitions rather than something a
/// caller should have to name, so it is inferred and then stated on the panel.
/// Guessing silently would be worse than asking; saying which guess was made is
/// better than both.
///
/// # Unlike a preconversion, this has a price
///
/// A launched basket has reserves, so the node can estimate. That is what makes
/// `--min-out` meaningful here and impossible before launch — and the floor is
/// checked **before signing and never again**: the chain does not enforce it, so
/// if the price moves after broadcast the conversion still happens.
pub fn convert(
    ui: &Ui,
    settings: &Settings,
    globals: &Globals,
    args: &CurrencyConvertArgs,
) -> miette::Result<()> {
    if !settings.profile.allow_spend {
        return Err(CurrencyError::SpendingDisabled {
            profile: settings.profile.name.clone(),
        }
        .into());
    }

    let amount = Amount::from_coins_str(&args.amount).map_err(|_| CurrencyError::BadAmount {
        value: args.amount.clone(),
    })?;
    if amount == Amount::ZERO {
        return Err(CurrencyError::ConvertsNothing.into());
    }
    let fee = Amount::from_coins_str(&args.fee).map_err(|_| CurrencyError::BadAmount {
        value: args.fee.clone(),
    })?;
    let min_out = match &args.min_out {
        None => None,
        Some(value) => {
            Some(
                Amount::from_coins_str(value).map_err(|_| CurrencyError::BadAmount {
                    value: value.clone(),
                })?,
            )
        }
    };

    let store = Keystore::new(&settings.paths);
    let envelope = choose_key(&store, args.from.as_deref())?;
    let recipient = args.to.clone().unwrap_or_else(|| envelope.address.clone());
    check_transparent(&recipient)?;

    let node = node::connect(&settings.profile)?;
    let target = read_currency(ui, &node, &args.currency)?;
    let spend = match &args.spend {
        Some(name) => name.clone(),
        None => settings.profile.currency.clone(),
    };
    let source = read_currency(ui, &node, &spend)?;

    // Which of the three shapes this is, and which currency is the basket doing
    // the work — they are not the same for all three, and every check below is
    // about the basket rather than about what the caller happened to name.
    let (kind, basket) = match &args.via {
        Some(via) => {
            let routed = read_currency(ui, &node, via)?;
            let kind = ConversionKind::ReserveToReserve {
                via: currency_id_of(&routed)?,
                target: currency_id_of(&target)?,
            };
            (kind, routed)
        }
        None if target.options & option::FRACTIONAL != 0 => (
            ConversionKind::IntoFractional {
                fractional: currency_id_of(&target)?,
            },
            target.clone(),
        ),
        None if source.options & option::FRACTIONAL != 0 => (
            ConversionKind::IntoReserve {
                reserve: currency_id_of(&target)?,
            },
            source.clone(),
        ),
        None => {
            return Err(CurrencyError::NeitherIsABasket {
                spend: source.name.clone(),
                into: target.name.clone(),
            }
            .into())
        }
    };

    // A basket that has not launched cannot be converted through — there are no
    // reserves to price against, and consensus refuses. The mirror of the check
    // `preconvert` makes, and the reason the two commands are never both valid.
    ui.sdk("node.block_count()");
    let tip = node
        .block_count()
        .map_err(|source| flow("reading the tip", FlowError::Rpc(source)))?;
    ui.sdk_result(tip.to_string());
    if u64::from(basket.start_block) > u64::from(tip) {
        return Err(CurrencyError::NotLaunchedYet {
            name: basket.name.clone(),
            start_block: basket.start_block,
            tip,
        }
        .into());
    }

    // A basket whose launch refunded holds nothing and never will. Its reserves
    // read as a live definition, so without this the only signal is an estimate
    // of zero — or a transfer that goes out and comes back.
    if launch_refunded(&basket) {
        return Err(CurrencyError::LaunchRefunded {
            name: basket.name.clone(),
        }
        .into());
    }

    let secret = keystore::passphrase(&format!("passphrase for `{}`", envelope.label), false)?;
    let key = envelope.unlock(&secret)?;

    let source_id = currency_id_of(&source)?;
    let source_text = Address::new(AddressKind::Identity, source_id.to_bytes()).to_string();
    let native = native_currency_id(&node, ui)?;
    let token_funding = if source_id == native {
        Vec::new()
    } else {
        ui.sdk(format!(
            "verus_sdk::network::spendable(&node, {:?})",
            envelope.address
        ));
        let funding = spendable(&node, &envelope.address)
            .map_err(|source| flow("reading the address's outputs", source))?;
        let held = super::send::carrying(&funding.other, source_id);
        if held.is_empty() {
            return Err(CurrencyError::NoTokenToSpend {
                spend: spend.clone(),
                address: envelope.address.clone(),
            }
            .into());
        }
        ui.sdk_result(format!("{} output(s) carrying {}", held.len(), spend));
        held
    };

    // Priced before it is built, so the panel can show what the node expects
    // rather than only what is being spent. `prepare_conversion` plans again
    // internally and checks the floor there; this is the same read, and the
    // number a user is actually deciding on.
    ui.sdk(format!(
        "verus_sdk::network::plan_conversion(&node, {source_text:?}, {}, {kind:?}, ...)",
        amount.to_sat()
    ));
    let plan = plan_conversion(
        &node,
        &source_text,
        amount,
        kind.clone(),
        &recipient,
        envelope
            .address
            .parse::<Address>()
            .map_err(|_| CurrencyError::NotAnAddress {
                value: envelope.address.clone(),
            })?,
        fee,
        min_out,
    )
    .map_err(|source| convert_flow(&basket.name, source))?;
    ui.sdk_result(format!("estimated_out {}", plan.estimated_out.to_sat()));

    ui.sdk("verus_sdk::network::prepare_conversion(&node, &key, ...)");
    let unsent = prepare_conversion(
        &node,
        &key,
        &source_text,
        amount,
        kind,
        &recipient,
        fee,
        min_out,
        &token_funding,
    )
    .map_err(|source| convert_flow(&basket.name, source))?;
    ui.sdk_result(format!("Unsent<Sent> {{ txid: {} }}", unsent.txid));

    let review = convert_panel(
        ui,
        &ConvertReview {
            target: &target,
            source: &source,
            basket: &basket,
            routed: args.via.is_some(),
            amount,
            estimated_out: plan.estimated_out,
            min_out,
            recipient: &recipient,
            envelope: &envelope,
            fee,
            txid: &unsent.txid,
        },
        globals.dry_run,
    );

    if ui.is_json() {
        let document = serde_json::json!({
            "into": target.name,
            "into_id": target.currency_id,
            "spend": source.name,
            "via": args.via.as_ref().map(|_| basket.name.clone()),
            "amount": amount.to_sat(),
            "estimated_out": plan.estimated_out.to_sat(),
            "min_out": min_out.map(Amount::to_sat),
            "to": recipient,
            "fee": fee.to_sat(),
            "txid": unsent.txid,
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
            .map_err(|source| convert_flow(&basket.name, source))?;
        emit(&serde_json::json!({
            "into": target.name,
            "into_id": target.currency_id,
            "spend": source.name,
            "via": args.via.as_ref().map(|_| basket.name.clone()),
            "amount": amount.to_sat(),
            "estimated_out": plan.estimated_out.to_sat(),
            "min_out": min_out.map(Amount::to_sat),
            "to": recipient,
            "fee": done.fee.to_sat(),
            "txid": done.txid,
            "broadcast": true,
        }));
        return Ok(());
    }

    ui.panel(&review);
    if globals.dry_run {
        ui.blank();
        ui.note("nothing was sent. Drop --dry-run to convert");
        ui.explain_panel();
        return Ok(());
    }
    if !globals.yes {
        confirm(ui)?;
    }

    ui.sdk("unsent.broadcast(&node)");
    let done = unsent
        .broadcast(&node)
        .map_err(|source| convert_flow(&basket.name, source))?;
    ui.blank();
    ui.ok(format!("converted — txid {}", done.txid));
    ui.note(format!("https://testex.verus.io/tx/{}", done.txid));
    ui.explain_panel();
    Ok(())
}

/// Everything the convert panel renders, so the function does not take ten
/// positional arguments that are easy to transpose.
struct ConvertReview<'a> {
    target: &'a CurrencySummary,
    source: &'a CurrencySummary,
    basket: &'a CurrencySummary,
    routed: bool,
    amount: Amount,
    estimated_out: Amount,
    min_out: Option<Amount>,
    recipient: &'a str,
    envelope: &'a Envelope,
    fee: Amount,
    txid: &'a str,
}

fn convert_panel(ui: &Ui, r: &ConvertReview, dry_run: bool) -> Panel {
    let palette = ui.theme.palette;
    let glyphs = ui.theme.glyphs;
    let show = |name: &str| fmt::untrusted(name, NAME_BUDGET, glyphs.ellipsis);

    let mut panel = Panel::new(if dry_run { "WOULD CONVERT" } else { "CONVERT" })
        .row(
            "spending",
            Text::of(fmt::amount(r.amount), palette.accent)
                .push("  ", palette.muted)
                .push(show(&r.source.name), palette.muted),
        )
        .row(
            "into",
            Text::of(show(&r.target.name), palette.accent)
                .push("  ", palette.muted)
                .push(&r.target.currency_id, palette.muted),
        );

    if r.routed {
        panel = panel.row(
            "through",
            Text::of(show(&r.basket.name), palette.value).push(
                "  one reserve into another, priced by the basket",
                palette.muted,
            ),
        );
    }

    panel = panel.row(
        "you receive",
        // The node's estimate, and it is only that: the price moves with every
        // conversion that lands before this one.
        Text::of(fmt::amount(r.estimated_out), palette.value)
            .push("  estimated, not guaranteed", palette.muted),
    );
    if let Some(floor) = r.min_out {
        panel = panel.row(
            "at least",
            Text::of(fmt::amount(floor), palette.value)
                .push("  checked now, not by the chain", palette.muted),
        );
    }

    panel
        .row("to", Text::of(r.recipient, palette.value))
        .row(
            "signed by",
            Text::of(&r.envelope.label, palette.value)
                .push("  ", palette.muted)
                .push(
                    fmt::address(&r.envelope.address, glyphs.ellipsis),
                    palette.muted,
                ),
        )
        .row("fee", Text::of(fmt::amount(r.fee), palette.value))
        .row("txid", Text::of(r.txid, palette.muted))
        .note(Text::of(
            "the price is whatever the basket's reserves make it when this lands, which is not \
             necessarily what is shown above — --min-out refuses to sign below a floor, but no \
             floor survives broadcast",
            palette.muted,
        ))
}

/// Whether a basket's launch refunded, leaving a definition that reads live and
/// holds nothing.
fn launch_refunded(found: &CurrencySummary) -> bool {
    const FLAG_REFUNDING: u64 = 4;
    found
        .definition
        .get("bestcurrencystate")
        .or_else(|| found.definition.get("lastconfirmedcurrencystate"))
        .and_then(|state| state.get("flags"))
        .and_then(serde_json::Value::as_u64)
        .is_some_and(|flags| flags & FLAG_REFUNDING != 0)
}

fn currency_id_of(found: &CurrencySummary) -> Result<CurrencyId, CurrencyError> {
    found
        .currency_id
        .parse::<Address>()
        .map(|address| CurrencyId::from_bytes(address.hash()))
        .map_err(|_| CurrencyError::NotFound {
            name: found.currency_id.clone(),
        })
}

fn read_currency(
    ui: &Ui,
    node: &crate::node::Node,
    name: &str,
) -> Result<CurrencySummary, CurrencyError> {
    let looked_up = name.strip_suffix('@').unwrap_or(name);
    ui.sdk(format!("node.currency_definition({looked_up:?})"));
    match node.currency_definition(looked_up) {
        Ok(found) => {
            ui.sdk_result(found.currency_id.clone());
            Ok(found)
        }
        // "Nothing on this chain is called that" is false, and misleading, when
        // an identity of that name exists and simply has not defined a currency
        // yet — which is the ordinary state of every identity, and exactly the
        // moment somebody reaches for `currency launch`. Costs one request, on
        // the failure path only.
        Err(_) if node.identity(name).is_ok() => Err(CurrencyError::NotACurrencyYet {
            name: name.trim_end_matches('@').to_string(),
        }),
        Err(_) => Err(CurrencyError::NotFound {
            name: name.to_string(),
        }),
    }
}

/// The recipient rule shared by every conversion: a bare key hash on the wire,
/// so an identity would pay the R-form of the same bytes.
fn check_transparent(recipient: &str) -> Result<(), CurrencyError> {
    let looks_like_an_identity = recipient.ends_with('@') || recipient.contains('.');
    let wrong_kind = CurrencyError::ConvertNeedsTransparentRecipient {
        value: recipient.to_string(),
    };
    match recipient.parse::<Address>() {
        Ok(parsed) if parsed.kind() == AddressKind::PubKeyHash => Ok(()),
        Ok(_) => Err(wrong_kind),
        Err(_) if looks_like_an_identity => Err(wrong_kind),
        Err(_) => Err(CurrencyError::NotAnAddress {
            value: recipient.to_string(),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The start block is a function of the tip it is handed and nothing else,
    /// which is the whole reason the tip is an argument: with `--register` the
    /// tip that must be passed is the one read *after* the registration was
    /// mined, not the one at the start of the command.
    #[test]
    fn the_start_block_is_measured_from_the_tip_it_is_handed() {
        assert_eq!(start_block_for(None, 20, 1_178_000).unwrap(), 1_178_020);
        // The same offset, nine blocks later — as after a registration wait.
        assert_eq!(start_block_for(None, 20, 1_178_009).unwrap(), 1_178_029);
        // An explicit --start-block is a height, not an offset, and ignores both.
        assert_eq!(
            start_block_for(Some(1_200_000), 20, 1_178_000).unwrap(),
            1_200_000
        );
    }

    /// A clamp would be worse than the overflow it patches: `u32::MAX` is
    /// *after* the tip, so both the flow and consensus accept it, and 200
    /// VRSCTEST buys a currency whose conversions never open.
    #[test]
    fn an_offset_past_the_last_block_there_can_be_is_refused_rather_than_clamped() {
        let refused = start_block_for(None, u32::MAX, 1_178_000);
        assert!(matches!(
            refused,
            Err(CurrencyError::StartBlockUnreachable {
                tip: 1_178_000,
                start_in: u32::MAX
            })
        ));
        assert!(
            !matches!(refused, Ok(height) if height == u32::MAX),
            "a clamped start block is still after the tip, so it is accepted and paid for"
        );
    }

    /// The misdirection this fix is about: a start block already passed is a
    /// question about the flags, and the catch-all sends it to `pecu doctor`
    /// for a node that answered correctly.
    #[test]
    fn a_start_block_in_the_past_points_at_the_flags_not_at_the_node() {
        let refused = flow(
            "building the launch",
            FlowError::NotReady(
                "start_block 1178005 is not after the tip 1178009; the chain refuses a launch \
                 in the past"
                    .into(),
            ),
        );
        let CurrencyError::Flow { advice, .. } = refused else {
            panic!("a flow refusal is a CurrencyError::Flow");
        };
        assert!(advice.contains("--start-in"));
        assert!(advice.contains("--start-block"));
        assert!(!advice.contains("doctor"));
    }

    /// Arm ordering: the one-way rule is also a `NotReady`, and being swallowed
    /// by the new arm is the only way this change could regress it.
    #[test]
    fn an_identity_that_already_defines_a_currency_keeps_its_own_advice() {
        let refused = flow(
            "building the launch",
            FlowError::NotReady(
                "iK2k8 already defines a currency; an identity defines exactly one".into(),
            ),
        );
        let CurrencyError::Flow { advice, .. } = refused else {
            panic!("a flow refusal is a CurrencyError::Flow");
        };
        assert!(advice.contains("defines a currency once and never again"));
    }

    /// The trap that cost a second launch: 100 sent against a cap of 50 is
    /// refunded **whole**, so the leg ends with nothing — and a basket refunds
    /// the entire launch unless every reserve receives something.
    #[test]
    fn room_is_what_is_left_under_the_cap_not_the_cap() {
        let reserves = vec!["iAAA".to_string(), "iBBB".to_string()];
        // 50-satoshi cap, nothing in yet: room is 50, so 100 is refused.
        assert_eq!(cap_room(&reserves, &[1000, 50], "iBBB", 0), Some(50));
        // 30 already in leaves 20, even though the cap is still 50.
        assert_eq!(cap_room(&reserves, &[1000, 50], "iBBB", 30), Some(20));
    }

    /// Already at or over the cap leaves no room, and must not underflow.
    #[test]
    fn a_full_reserve_has_no_room_left() {
        let reserves = vec!["iAAA".to_string()];
        assert_eq!(cap_room(&reserves, &[50], "iAAA", 50), Some(0));
        assert_eq!(cap_room(&reserves, &[50], "iAAA", 90), Some(0));
    }

    /// No cap vector means uncapped; a zero cap is a different refusal handled
    /// before this is reached. Neither is a room of zero.
    #[test]
    fn an_uncapped_reserve_has_no_ceiling_to_check() {
        let reserves = vec!["iAAA".to_string()];
        assert_eq!(cap_room(&reserves, &[], "iAAA", 0), None);
        assert_eq!(cap_room(&reserves, &[0], "iAAA", 0), None);
    }

    /// A live weight is a computed ratio and arrives with six decimals; a
    /// weight a caller typed must come back exactly as typed.
    #[test]
    fn a_live_weight_rounds_to_nearest_and_a_typed_one_survives() {
        assert_eq!(weight_percent(33_333_334), "33.33%");
        assert_eq!(weight_percent(33_333_333), "33.33%");
        assert_eq!(weight_percent(62_500_000), "62.5%");
        assert_eq!(weight_percent(50_000_000), "50%");
        assert_eq!(weight_percent(100_000_000), "100%");
        assert_eq!(weight_percent(5_000_000), "5%");
    }

    /// The rule that cost a real launch: `--max-preconvert VRSCTEST:1000` on a
    /// two-reserve basket caps the other reserve at zero, nothing can be paid
    /// into it, and a fractional basket refunds the whole launch unless every
    /// reserve receives something.
    #[test]
    fn a_partly_named_cap_starves_the_reserves_nobody_named() {
        let caps = vec![Amount::from_sat(100_000_000_000), Amount::ZERO];
        assert_eq!(capped_at_zero(&caps), Some(1));
    }

    /// Naming none of them is the safe case and must stay allowed: an empty
    /// vector is never consulted, so every reserve is uncapped.
    #[test]
    fn naming_no_caps_at_all_is_not_a_starved_reserve() {
        assert_eq!(capped_at_zero(&[]), None);
    }

    #[test]
    fn every_reserve_named_is_fine() {
        let caps = vec![Amount::from_sat(1), Amount::from_sat(2)];
        assert_eq!(capped_at_zero(&caps), None);
    }

    /// Asked from the paying side, once the definition is already on chain.
    #[test]
    fn a_reserve_capped_at_zero_accepts_nothing() {
        let reserves = vec!["iAAA".to_string(), "iBBB".to_string()];
        assert!(reserve_accepts_nothing(&reserves, &[1000, 0], "iBBB"));
        assert!(!reserve_accepts_nothing(&reserves, &[1000, 0], "iAAA"));
    }

    /// No cap vector means no cap, not a cap of zero. Getting this backwards
    /// would refuse every preconversion into an uncapped basket.
    #[test]
    fn an_absent_cap_vector_accepts_everything() {
        let reserves = vec!["iAAA".to_string()];
        assert!(!reserve_accepts_nothing(&reserves, &[], "iAAA"));
    }

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
