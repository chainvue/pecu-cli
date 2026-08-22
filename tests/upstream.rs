//! What the guards in `src/cmd/` are still waiting for.
//!
//! Several commands refuse a flag that the chain would accept, because the SDK
//! could not build the transaction correctly. Every one of those gaps closed
//! upstream between 7 and 8 August, and the pin moved on the 22nd — so the
//! guards are no longer describing the SDK. They stay because removing one
//! spends real VRSCTEST to prove, and nothing has been watched onto the chain
//! yet.
//!
//! These tests close the half of that gap which does not cost anything: they
//! assert, against the pinned SDK, that the specific defect each guard was
//! written for is gone. That is not proof the launch lands — only a chain gives
//! that — but it is the precondition, and it is the part that can regress
//! silently. Move the pin backwards, or move it forward onto a regression, and
//! these go red before anybody spends a coin finding out.
//!
//! They are deliberately **not** `#[ignore]`d: no node, no key, no coins.
//!
//! Each test names the guard it is a precondition for. When one is removed,
//! delete the matching test with it — a precondition for a guard that no longer
//! exists is just trivia.

use verus_sdk::convert::{build_conversion, ConversionKind};
use verus_sdk::decode::Destination;
use verus_sdk::identity::identity_primary_script;
use verus_sdk::money::Amount;
use verus_sdk::send::CurrencyId;
use verus_sdk::verus_keys::{Address, AddressKind};

/// An identity id, a revocation authority and a recovery authority, distinct
/// enough that a script confusing two of them would not still pass.
const IDENTITY: [u8; 20] = [0x11; 20];
const REVOCATION: [u8; 20] = [0x22; 20];
const RECOVERY: [u8; 20] = [0x33; 20];

/// The key hash of the `EVAL_IDENTITY_RECOVER` contract pubkey.
///
/// Written out rather than imported. The SDK does not re-export it through
/// `verus_sdk`, and importing it would make this assert the constant against
/// itself — a test that passes however upstream changes it. The value is a
/// constant of the protocol, not of the SDK: the SDK's own note records it as
/// confirmed on chain, in output 0 of the VRSCTEST NFT launches `sdknftbeta`
/// (`4ad8fb14…7d7e`) and `kmerg` (`8d8671d4…b6b3`), which both end in it.
///
/// So if this ever stops matching, that is the finding.
const IDENTITY_RECOVER_KEYHASH: [u8; 20] = [
    0xb6, 0xaf, 0xf5, 0x98, 0xba, 0x59, 0x55, 0x62, 0xed, 0x96, 0xe7, 0xa4, 0x84, 0x19, 0x36, 0xed,
    0x23, 0x6c, 0xf3, 0xbd,
];

fn primary_script(tokenized_control: bool) -> Vec<u8> {
    identity_primary_script(
        IDENTITY,
        // The identity blob is opaque to the destination structure being
        // tested, so its contents do not matter — only that both calls get the
        // same one, or the scripts would differ for the wrong reason.
        vec![0xab; 32],
        REVOCATION,
        RECOVERY,
        tokenized_control,
    )
    .expect("a primary script for well-formed inputs")
}

/// Precondition for removing `CurrencyError::NftScriptGap` (`--nft`).
///
/// The guard exists because an identity with tokenized control carries a
/// *second* destination on its recovery condition — the `EVAL_IDENTITY_RECOVER`
/// contract key hash — and the SDK's identity output did not emit it, so
/// consensus derived a different script and refused the launch as
/// `-25: bad-txns-failed-precheck`, which names nothing.
///
/// It takes the flag now. This asserts the flag actually changes the script,
/// rather than being accepted and ignored — which is the failure mode that
/// would look fixed and land the same rejection.
#[test]
fn a_tokenized_control_identity_output_differs_from_an_ordinary_one() {
    assert_ne!(
        primary_script(true),
        primary_script(false),
        "tokenized control did not change the identity output at all, so the recovery \
         condition is the same one consensus already refused"
    );
}

/// Precondition for removing `CurrencyError::NftScriptGap` (`--nft`).
///
/// The stronger half: not merely that the script changed, but that it changed
/// by carrying *this* destination. A script that differed for some other reason
/// would satisfy the test above and still be refused on chain.
#[test]
fn the_tokenized_control_script_carries_the_recovery_contract_destination() {
    let with = primary_script(true);
    let without = primary_script(false);

    let needle = Destination::PubKeyHash(IDENTITY_RECOVER_KEYHASH);
    let hash = match needle {
        Destination::PubKeyHash(hash) => hash,
        _ => unreachable!("constructed as a pubkey hash"),
    };

    assert!(
        contains(&with, &hash),
        "the tokenized-control script does not carry the EVAL_IDENTITY_RECOVER key hash, \
         which is the destination whose absence the --nft guard was written for"
    );
    assert!(
        !contains(&without, &hash),
        "an ordinary identity output carries the recovery contract destination too, so its \
         presence proves nothing about tokenized control"
    );
}

/// Whether `haystack` contains `needle` as a contiguous run.
///
/// The script is a serialised condition, not a struct this crate can take
/// apart, and `decode_output_script` reports the output *kind* rather than the
/// destinations inside a nested condition. A byte search is enough for what is
/// being asserted: that a known 20-byte constant is present in one script and
/// absent from the other.
fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.windows(needle.len()).any(|run| run == needle)
}

/// Precondition for removing the VerusID-recipient refusals in `send`, `mint`,
/// `preconvert` and `convert`.
///
/// The guard exists because `build_conversion` wrote every recipient as a bare
/// key hash and discarded the address kind, so an i-address would have paid the
/// R-address sharing its hash — an address nobody holds a key to. Consensus
/// treats `DEST_ID` as a first-class reserve transfer destination and
/// `sendcurrency` pays identities routinely, so this was never a protocol
/// limit; it was the builder throwing the kind away.
///
/// The assertion is on the destination the builder produced, not merely on the
/// call succeeding. Succeeding while writing a `PubKeyHash` is exactly the old
/// behaviour, and it is the one failure mode that looks fixed and still sends
/// the money somewhere unspendable.
#[test]
fn a_conversion_can_name_a_verusid_and_keeps_it_an_identity() {
    let transfer = build_conversion(
        CurrencyId::from_bytes([0x01; 20]),
        Amount::from_sat(100_000_000),
        ConversionKind::IntoFractional {
            fractional: CurrencyId::from_bytes([0x02; 20]),
        },
        Address::new(AddressKind::Identity, IDENTITY),
        // The refund leg stays transparent: a refund is paid back to whoever
        // spent, and that is a key, not the identity being paid.
        Address::new(AddressKind::PubKeyHash, REVOCATION),
        CurrencyId::from_bytes([0x01; 20]),
        Amount::from_sat(20_000),
    )
    .expect("an identity is a valid conversion recipient");

    match transfer.destination.recipient {
        Destination::Identity(hash) => assert_eq!(
            hash, IDENTITY,
            "the recipient stayed an identity but became a different one"
        ),
        other => panic!(
            "an i-address recipient came back as {other:?} — the address kind was discarded, \
             which is the defect the VerusID refusals were written for"
        ),
    }
}
