# Status


### Guards waiting on a chain, not on an SDK

Each of these is built here and refused here, with a named diagnostic — none
reaches the user as a bare node error, and none spends anything on the way to
failing.

They were refused because the SDK could not express them. **That stopped being
true on 7 August**, when every one of these closed upstream; the pin caught up
on the 22nd. What stands between them and working is no longer a dependency, it
is evidence: each is a money path, and the only tests that exercise one spend
real VRSCTEST against a public node. So the guards stay until somebody removes
one and watches it land.

| | closed upstream by | what pecu does today | what removing the guard takes |
|---|---|---|---|
| **Mint / preconvert / convert to a VerusID** — `build_conversion` wrote every recipient as `PubKeyHash` and dropped the kind, so an i-address would have paid the R-address sharing its hash | [#115](https://github.com/chainvue/verus-rust-sdk/issues/115) — `convert.rs` now maps `AddressKind::Identity` to `Destination::Identity` | refuses, and says paying a primary address is *not* the same thing | pay an identity a token and read it back off the chain as the identity's, not a key holder's |
| **Token send to a VerusID** — same root cause, so an identity could hold native coins but not tokens | [#115](https://github.com/chainvue/verus-rust-sdk/issues/115) | refuses with the native-vs-token asymmetry spelled out | as above; the two share a code path and should be proven together |
| **NFT launch** — the identity output omitted the tokenized-control recovery destination, so consensus derived a different script | [#111](https://github.com/chainvue/verus-rust-sdk/issues/111) — `identity_primary_script` now takes `has_tokenized_control` | `--nft` builds the definition, then reports the gap | launch one, and check consensus accepts the identity output rather than `bad-txns-failed-precheck` |
| **NFT launch fee** — charged `currencyregistrationfee` (200) instead of `idimportfees` (0.02) | [#112](https://github.com/chainvue/verus-rust-sdk/issues/112) | pins the right fee itself | confirm the fee the flow now reads matches the one pinned here, then stop pinning it |
| **NFT definition shape** — `NFT_TOKEN` on a `token()` could not express a valid one | [#113](https://github.com/chainvue/verus-rust-sdk/issues/113) — `CurrencyDefinition::nft()` exists | sets all five fields by hand | decode a launch built by the constructor against one built by hand; they should agree field for field |
| **Seeded contributions at launch** — the builder emitted no output funding a declared contribution, and the notarization in the same transaction said the reserves were zero | [#129](https://github.com/chainvue/verus-rust-sdk/issues/129) — the daemon's eighth output is built now | refuses `--contribute` and points at `preconvert` | launch a basket with a seeded reserve and confirm the reserve actually holds it at the start block |

Two rows left this table rather than moving down it.

**The expired commitment** ([#114](https://github.com/chainvue/verus-rust-sdk/issues/114)) is done. `CommitmentStatus::Expired` exists and `resume` matches it by name, before any broadcast — where the old string match on `expiring-soon` could only fire after one had already been attempted, and the two states need opposite actions. The string match is kept as a fallback for the same rejection arriving unclassified.

**Two payments in one block** ([#118](https://github.com/chainvue/verus-rust-sdk/issues/118)) needs no guard removed, because `pecu` never had one — the limitation was simply true. `funding` now reads the mempool and withholds coins an unconfirmed transaction already spends, so a second payment should build against different coins. Unproven here: it wants two sends in one block, watched.

### Built but unproven

Paths that exist and have never been exercised against the chain. They are not
claimed as working:

* **`pecu key --history` on a key with more than one revision.** The single
  revision case is covered; nothing has rotated yet.
* **Multi-signature identities.** `send`, `mint` and the whole lifecycle sign
  with one key and refuse an m-of-n identity by name. The air-gap trio is the
  natural home for this, once it learns identity inputs.

### Parked

* **`feat/m8-login-vdxf`** — `id login` and VDXF publish/read, deliberately kept
  off `main`. Well behind now; needs a rebase before it is worth looking at.

## Not in scope (yet)

Shielded/z-address operations, the currency launch wizard, marketplace offers, and a
live `watch` dashboard. They come after the transparent and identity paths are solid.

