//! Turning SDK values into something worth reading.
//!
//! The SDK's own `Amount: Display` trims trailing zeros — `312.5`, not
//! `312.50000000`. That is right for a log line and wrong for a ledger, where
//! the decimal points have to line up. Everything here formats from the satoshi
//! integer; no float is constructed at any point.

use verus_sdk::money::{Amount, SATS_PER_COIN};

/// A wallet-column amount: always eight decimal places, so a stack of them
/// aligns on the point.
pub fn amount(value: Amount) -> String {
    let sats = value.to_sat();
    format!("{}.{:08}", sats / SATS_PER_COIN, sats % SATS_PER_COIN)
}

/// The same, from a raw satoshi count.
pub fn sats(value: u64) -> String {
    amount(Amount::from_sat(value))
}

/// A total that may not exist, because the numbers it was made of did not fit.
///
/// Every total on a panel here is built from figures somebody else chose —
/// output values out of bytes a counterparty handed over, preallocations out of
/// a node's JSON — and nothing upstream bounds them. A wrapped total is worse
/// than no total: `u64::MAX - 1e8` plus `2e8` prints as a plausible
/// `1.00000000` on the panel a reader consults before authorising a broadcast.
/// So where the sum could not be taken, this says so rather than printing a
/// number that is not the answer.
pub fn total(value: Option<Amount>) -> String {
    match value {
        Some(sum) => amount(sum),
        None => "more than can be represented".to_string(),
    }
}

/// A movement rather than a holding: always signed, so `+` and `-` line up in
/// the same column and a net of zero reads as `+0.00000000` rather than as an
/// amount that happens to be small.
///
/// Formatted from the magnitude so that `i64::MIN` cannot be negated into an
/// overflow, and so the digits after the point are never themselves negative.
pub fn signed(satoshis: i64) -> String {
    let sign = if satoshis < 0 { '-' } else { '+' };
    let magnitude = satoshis.unsigned_abs();
    format!(
        "{sign}{}.{:08}",
        magnitude / SATS_PER_COIN,
        magnitude % SATS_PER_COIN
    )
}

/// A block height, grouped: `3481207` reads as `3,481,207`.
///
/// Amounts deliberately do *not* get separators — a grouped coin value is one
/// glance away from being misread as a different number.
pub fn height(value: u64) -> String {
    let digits = value.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (index, digit) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index).is_multiple_of(3) {
            out.push(',');
        }
        out.push(digit);
    }
    out
}

/// Shorten a long identifier to `head…tail`, keeping both ends because both
/// ends are what a human compares.
///
/// Anything already short enough is returned untouched.
pub fn elide(text: &str, head: usize, tail: usize, ellipsis: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    if chars.len() <= head + tail + ellipsis.chars().count() {
        return text.to_string();
    }
    let start: String = chars[..head].iter().collect();
    let end: String = chars[chars.len() - tail..].iter().collect();
    format!("{start}{ellipsis}{end}")
}

/// Shorten from the middle so `text` fits in `max` cells.
///
/// For paths and long identifiers, where the tail carries the meaning: the last
/// two path components say more than the first ten. Returns `text` untouched
/// when it already fits.
pub fn fit(text: &str, max: usize, ellipsis: &str) -> String {
    let length = text.chars().count();
    let marker = ellipsis.chars().count();
    if length <= max || max <= marker + 2 {
        return text.to_string();
    }
    let head = (max - marker) / 3;
    elide(text, head, max - marker - head, ellipsis)
}

/// Characters that are invisible, or that reorder what is printed after them.
///
/// Not one of these is `is_control()` — they are Unicode format characters
/// (general category Cf) and the two line separators (Zl, Zp), so the Cc check
/// in `untrusted` walks straight past them. Every one of them defeats reading a
/// name. `RLO` reverses display order, so a name registered as
/// `evil\u{202e}gnp.tnecconni` renders as `innocent.png` on the panel that asks
/// for `yes`, while the bytes say otherwise; the zero-width set makes two names
/// that differ in the bytes render identically, on a panel whose whole job is
/// telling two names apart; U+2028 and U+2029 break a line without being
/// controls, and `untrusted`'s promise that everything is folded onto one line
/// has to stay true.
///
/// The ranges are written out because `std` has no general-category test and one
/// display filter does not justify a Unicode-tables dependency. This is Cf as of
/// Unicode 16; a later version adding to that category would want adding here
/// too.
fn deceptive(character: char) -> bool {
    matches!(
        character,
        '\u{ad}'                        // soft hyphen
            | '\u{600}'..='\u{605}'     // arabic number signs
            | '\u{61c}'                 // arabic letter mark
            | '\u{6dd}'                 // arabic end of ayah
            | '\u{70f}'                 // syriac abbreviation mark
            | '\u{890}'..='\u{891}'     // arabic pound and piastre marks
            | '\u{8e2}'                 // arabic disputed end of ayah
            | '\u{180e}'                // mongolian vowel separator
            | '\u{200b}'..='\u{200f}'   // zero-width set, LRM, RLM
            | '\u{2028}'..='\u{2029}'   // line and paragraph separators
            | '\u{202a}'..='\u{202e}'   // bidi embeddings and overrides
            | '\u{2060}'..='\u{2064}'   // word joiner, invisible operators
            | '\u{2066}'..='\u{206f}'   // bidi isolates, deprecated shaping
            | '\u{feff}'                // BOM / zero-width no-break space
            | '\u{fff9}'..='\u{fffb}'   // interlinear annotation
            | '\u{110bd}'               // kaithi number sign
            | '\u{110cd}'               // kaithi number sign above
            | '\u{13430}'..='\u{1343f}' // egyptian hieroglyph format controls
            | '\u{1bca0}'..='\u{1bca3}' // shorthand format controls
            | '\u{1d173}'..='\u{1d17a}' // musical beam and phrase controls
            | '\u{e0001}'               // language tag
            | '\u{e0020}'..='\u{e007f}' // tag characters
    )
}

/// Make text that came from a node safe to print.
///
/// The SDK is explicit that currency and identity names are **untrusted display
/// text**: Verus permits far more in a name than it looks like it does, and the
/// node is simply repeating what somebody registered. Printed raw, a name can
/// carry ANSI escapes that repaint the terminal, a newline that forges an extra
/// row in a balance table, or characters chosen to read as an address, as a
/// number, or as somebody else's name.
///
/// So: control characters and escapes, and the invisible and direction-changing
/// characters that let one name read as another, are replaced; everything is
/// folded onto one line; and the result is capped. What comes back is display
/// text and nothing more — the currency **id** is the part that identifies
/// anything, and it is always shown alongside.
pub fn untrusted(text: &str, max: usize, ellipsis: &str) -> String {
    fit(neutralise(text).trim(), max, ellipsis)
}

/// [`untrusted`]'s filtering without its budget: safe to print, and whole.
///
/// For the one caller that has to spend the budget itself. `id list` cuts a
/// name from the **end** rather than from the middle, because [`fit`] keeps the
/// tail and the last character of a VerusID name is the `@` that says it is a
/// whole one — so a name cut to fit came out still wearing the mark of a name
/// another command would accept, and `pecu id show` answers "nothing on this
/// chain is called that" for it. Filtering and budgeting are separated here so
/// that the cut can drop the `@` with everything else it drops.
pub fn neutralised(text: &str) -> String {
    neutralise(text).trim().to_string()
}

/// Replace everything that cannot be allowed onto a terminal, one character for
/// one character.
///
/// The 1:1 map is the point and not an implementation detail: `fit`'s budget,
/// every `NAME_BUDGET`, and the column widths `cmd::currency` measures over the
/// result all count characters, and deleting rather than replacing would
/// desynchronise all three — as well as hiding from the reader that anything
/// was taken out.
fn neutralise(text: &str) -> String {
    text.chars()
        .map(|character| {
            if character.is_control() || character == '\u{7f}' || deceptive(character) {
                '·'
            } else {
                character
            }
        })
        .collect()
}

/// The width of a Verus address, and it is not a rounded-up guess. All three
/// kinds carry a one-byte version (`0x3c`, `0x66`, `0x55`), so base58check runs
/// over 25 bytes, and every value those version bytes admit falls inside
/// [58^33, 58^34) — an i-address is *exactly* 34 characters, never 33 and never
/// 35. So this budget cannot elide the middle of an honest one, and is not one
/// character short of the thing it budgets for.
const ID_BUDGET: usize = 34;

/// An identifier the node handed over — a currency's i-address, an identity's.
///
/// Printed in full, but not on trust. The name beside it is filtered because a
/// registrant chose it; this is filtered because a *node* chose it, and nothing
/// between the socket and the frame checks its shape. A well-formed address is
/// under budget and contains nothing the filter maps, so it comes back
/// untouched; a string longer than an address is not one, whatever field it
/// arrived in.
pub fn id(text: &str, ellipsis: &str) -> String {
    fit(neutralise(text).trim(), ID_BUDGET, ellipsis)
}

/// An address, shortened the way a wallet UI shortens one.
///
/// Neutralised first: the callers that reach here with a node's string do so on
/// the path where parsing it as an `Address` already failed — `wallet history`
/// falls back to this for a `net_currencies` key it could not parse — and
/// `elide` keeps the first nine characters verbatim, which is room for an
/// escape run. Base58 contains nothing the filter maps, so an honest address is
/// unchanged, character for character.
pub fn address(text: &str, ellipsis: &str) -> String {
    elide(
        neutralise(text).trim(),
        ADDRESS_HEAD,
        ADDRESS_TAIL,
        ellipsis,
    )
}

/// What [`address`] keeps: enough of the front to recognise, enough of the back
/// to tell two apart.
const ADDRESS_HEAD: usize = 9;
const ADDRESS_TAIL: usize = 4;

/// How wide [`address`] renders, in characters.
///
/// This is the floor a table column holding an id may be shortened to, and the
/// reason it is a function rather than a number at the call site: below the
/// short form an i-address stops being a handle. Every one of them opens with
/// `i`, so `i…dK9f` is four informative characters — it cannot be copied,
/// pasted or looked up, which is the whole of what the column was for.
pub fn address_width(ellipsis: &str) -> usize {
    ADDRESS_HEAD + ellipsis.chars().count() + ADDRESS_TAIL
}

/// A 64-character hex hash, shortened. Neutralised first, for the same reason
/// [`address`] is.
pub fn hash(text: &str, ellipsis: &str) -> String {
    elide(neutralise(text).trim(), 10, 6, ellipsis)
}

/// A rough elapsed time: `42s`, `7m 12s`, `3h 04m`, `2d 11h`.
///
/// Rough on purpose. This is used for "how stale is the chain tip", where the
/// answer that matters is the order of magnitude.
pub fn duration(seconds: u64) -> String {
    match seconds {
        0..=99 => format!("{seconds}s"),
        100..=3599 => format!("{}m {:02}s", seconds / 60, seconds % 60),
        3600..=86_399 => format!("{}h {:02}m", seconds / 3600, (seconds % 3600) / 60),
        _ => format!("{}d {:02}h", seconds / 86_400, (seconds % 86_400) / 3600),
    }
}

/// A unix timestamp as `2026-08-05 14:32 UTC`.
///
/// UTC, always, and said so in the string. A block time rendered in local time
/// is a number two readers in different places disagree about, and the chain
/// has one answer.
///
/// The date arithmetic is Howard Hinnant's `civil_from_days`, which is a dozen
/// lines and avoids taking on a date library for one display string. Times
/// before 1970 cannot occur here — they would be a block mined before the unix
/// epoch — so a negative timestamp renders as the epoch rather than failing.
pub fn timestamp(unix: i64) -> String {
    let seconds = unix.max(0);
    let days = seconds / 86_400;
    let time_of_day = seconds % 86_400;

    // Shift the era so leap-day handling falls at the end of the cycle.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let day_of_era = z.rem_euclid(146_097);
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let shifted_month = (5 * day_of_year + 2) / 153;

    let day = day_of_year - (153 * shifted_month + 2) / 5 + 1;
    let month = if shifted_month < 10 {
        shifted_month + 3
    } else {
        shifted_month - 9
    };
    let year = year_of_era + era * 400 + i64::from(month <= 2);

    format!(
        "{year:04}-{month:02}-{day:02} {:02}:{:02} UTC",
        time_of_day / 3_600,
        (time_of_day % 3_600) / 60
    )
}

/// `1 output` / `4 outputs`, because "1 outputs" looks like a bug.
pub fn plural(count: usize, singular: &str, plural: &str) -> String {
    if count == 1 {
        format!("{count} {singular}")
    } else {
        format!("{count} {plural}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn amounts_keep_all_eight_places() {
        assert_eq!(sats(31_250_000_000), "312.50000000");
        assert_eq!(sats(1), "0.00000001");
        assert_eq!(sats(0), "0.00000000");
        assert_eq!(sats(100_000_000), "1.00000000");
    }

    #[test]
    fn amounts_are_never_grouped() {
        assert_eq!(sats(1_234_567_800_000_000), "12345678.00000000");
    }

    #[test]
    fn a_movement_always_carries_its_sign() {
        assert_eq!(signed(150_000_000), "+1.50000000");
        assert_eq!(signed(-150_000_000), "-1.50000000");
        // Not "0.00000000": the column is movements, and an unsigned entry in
        // it reads as a holding.
        assert_eq!(signed(0), "+0.00000000");
    }

    #[test]
    fn the_most_negative_amount_formats_rather_than_overflowing() {
        // `-i64::MIN` panics in debug and wraps in release. Formatting from the
        // magnitude is what keeps a hostile or broken reply from taking the
        // process down.
        assert_eq!(signed(i64::MIN), "-92233720368.54775808");
    }

    #[test]
    fn timestamps_render_as_utc_dates() {
        assert_eq!(timestamp(0), "1970-01-01 00:00 UTC");
        // A leap day, which is what the era arithmetic exists to get right.
        assert_eq!(timestamp(1_709_208_000), "2024-02-29 12:00 UTC");
        assert_eq!(timestamp(1_770_000_000), "2026-02-02 02:40 UTC");
        // Century years are leap only when divisible by 400.
        assert_eq!(timestamp(951_825_600), "2000-02-29 12:00 UTC");
    }

    #[test]
    fn a_timestamp_before_the_epoch_does_not_panic() {
        // Cannot happen from a block, but the field is signed and comes from a
        // node, so it must not be able to take the process down.
        assert_eq!(timestamp(-1), "1970-01-01 00:00 UTC");
    }

    #[test]
    fn heights_are_grouped_in_threes() {
        assert_eq!(height(0), "0");
        assert_eq!(height(999), "999");
        assert_eq!(height(1_000), "1,000");
        assert_eq!(height(3_481_207), "3,481,207");
    }

    #[test]
    fn elision_keeps_both_ends_and_leaves_short_text_alone() {
        assert_eq!(address("RXyz9k2mPqrstuv7Qa4", "…"), "RXyz9k2mP…7Qa4");
        assert_eq!(address("Rshort", "…"), "Rshort");
    }

    #[test]
    fn fitting_keeps_the_tail_and_respects_the_budget() {
        let path = "/Users/someone/Library/deeply/nested/verus-pecu/config.toml";
        let fitted = fit(path, 30, "…");
        assert_eq!(fitted.chars().count(), 30);
        assert!(fitted.ends_with("config.toml"), "{fitted}");
        assert_eq!(fit("short", 30, "…"), "short");
    }

    #[test]
    fn fitting_gives_up_rather_than_producing_nonsense_in_a_tiny_budget() {
        assert_eq!(fit("abcdefgh", 2, "…"), "abcdefgh");
    }

    #[test]
    fn untrusted_text_cannot_repaint_the_terminal_or_forge_a_row() {
        let hostile = "ok\u{1b}[31m\nSPENDABLE  999.00000000\r";
        let safe = untrusted(hostile, 60, "…");
        assert!(!safe.contains('\u{1b}'), "escape survived: {safe:?}");
        assert!(!safe.contains('\n'), "newline survived: {safe:?}");
        assert!(!safe.contains('\r'), "carriage return survived: {safe:?}");
    }

    #[test]
    fn untrusted_text_cannot_reverse_the_order_a_name_reads_in() {
        // The issue's own case. `RLO` reverses the display order of everything
        // after it, so these bytes render as `innocent.png` on the panel that
        // asks for `yes` — a name that reads as one thing and is another.
        let safe = untrusted("evil\u{202e}gnp.tnecconni", 40, "…");
        assert_eq!(safe, "evil·gnp.tnecconni");
        for character in ['\u{202a}', '\u{202b}', '\u{202c}', '\u{202d}', '\u{202e}']
            .into_iter()
            .chain(['\u{2066}', '\u{2067}', '\u{2068}', '\u{2069}'])
            .chain(['\u{200e}', '\u{200f}', '\u{61c}'])
        {
            let safe = untrusted(&format!("a{character}b"), 40, "…");
            assert_eq!(safe, "a·b", "{character:?} survived: {safe:?}");
        }
    }

    #[test]
    fn two_names_that_differ_only_by_an_invisible_character_do_not_print_the_same() {
        // The panel's whole job on these rows is telling two registrations
        // apart. A name that differs only by something with no glyph would
        // print identically to the one it is impersonating.
        for character in [
            '\u{200b}',
            '\u{200c}',
            '\u{200d}',
            '\u{2060}',
            '\u{feff}',
            '\u{ad}',
            '\u{61c}',
            '\u{e0041}',
        ] {
            let safe = untrusted(&format!("alice{character}bob"), 40, "…");
            assert_ne!(
                safe,
                untrusted("alicebob", 40, "…"),
                "{character:?} printed as nothing"
            );
            assert!(safe.contains('·'), "{character:?} survived: {safe:?}");
        }
    }

    #[test]
    fn untrusted_text_stays_on_one_line_even_when_the_break_is_not_a_control_character() {
        // U+2028 and U+2029 are Zl and Zp, not Cc, so `is_control()` walks past
        // them while a terminal still breaks the line. They are Unicode
        // White_Space, so `trim()` only ever reached them at the ends — one in
        // the middle forged an extra row.
        let hostile = "ok\u{2028}SPENDABLE  999.00000000\u{2029}more";
        let safe = untrusted(hostile, 60, "…");
        assert!(
            !safe.contains('\u{2028}'),
            "line separator survived: {safe:?}"
        );
        assert!(
            !safe.contains('\u{2029}'),
            "paragraph separator survived: {safe:?}"
        );
    }

    #[test]
    fn neutralising_a_character_leaves_one_the_reader_can_count() {
        // The map is 1:1: `fit`'s budget, every NAME_BUDGET, and the column
        // widths `currency` measures over the sanitized string all assume it.
        // Deleting rather than replacing would desynchronise all three, and
        // would hide from the reader that anything was there.
        let hostile = "a\u{202e}b\u{200b}c\u{2028}d\u{feff}e";
        assert_eq!(
            untrusted(hostile, 200, "…").chars().count(),
            hostile.chars().count()
        );
    }

    #[test]
    fn untrusted_text_is_capped() {
        let long = "n".repeat(500);
        assert!(untrusted(&long, 20, "…").chars().count() <= 20);
    }

    #[test]
    fn ordinary_names_pass_through_untouched() {
        // The risk of a wider filter is that it starts eating honest text.
        assert_eq!(untrusted("Bridge.vETH", 40, "…"), "Bridge.vETH");
        assert_eq!(untrusted("Ünïcødé.vRSC", 40, "…"), "Ünïcødé.vRSC");
        assert_eq!(untrusted("桥.vETH", 40, "…"), "桥.vETH");
        assert_eq!(untrusted("bridge-eth.vrsc", 40, "…"), "bridge-eth.vrsc");
    }

    #[test]
    fn an_id_the_node_supplied_cannot_repaint_the_terminal() {
        // The i-address rows are the node's word, not a registrant's, and
        // nothing between the socket and the frame checks their shape.
        let hostile = "i\u{1b}[31mK2k8\nYH1\u{202e}jfR\u{200b}7";
        let safe = id(hostile, "…");
        assert!(!safe.contains('\u{1b}'), "escape survived: {safe:?}");
        assert!(!safe.contains('\n'), "newline survived: {safe:?}");
        assert!(!safe.contains('\u{202e}'), "override survived: {safe:?}");
        assert!(!safe.contains('\u{200b}'), "zero width survived: {safe:?}");
        // Replaced rather than dropped, so the reader can see something was
        // taken out instead of the string quietly getting shorter.
        assert!(safe.contains('·'), "nothing marked as removed: {safe:?}");
    }

    #[test]
    fn an_honest_i_address_prints_in_full_and_unchanged() {
        // The polarity case, and the budget guard: an i-address is exactly 34
        // characters, so `ID_BUDGET` must not be one short of the thing it
        // budgets for — with either ellipsis, since the marker is counted
        // against the budget.
        let honest = "iK2k8YH1jfR7RLmEZ3zac2Mkx5rxSgbMqg";
        assert_eq!(honest.chars().count(), ID_BUDGET);
        assert_eq!(id(honest, "…"), honest);
        assert_eq!(id(honest, "..."), honest);
    }

    #[test]
    fn a_shortened_address_cannot_carry_an_escape_out_of_a_node() {
        // `wallet history` falls back to this for a `net_currencies` key it
        // could not parse as an `Address` — the parse having failed *is* the
        // hostile case — and `elide` keeps the first nine characters verbatim,
        // which is room for the whole escape.
        let safe = address("\u{1b}[31mRQr2cUkF46n7y8WRzDkd1iV9gHusSSQuzX", "…");
        assert!(!safe.contains('\u{1b}'), "escape survived: {safe:?}");
        // And the honest case still shortens to exactly what it did before.
        assert_eq!(address("RXyz9k2mPqrstuv7Qa4", "…"), "RXyz9k2mP…7Qa4");
    }

    #[test]
    fn a_shortened_hash_cannot_carry_an_escape_out_of_a_node() {
        let safe = hash(&format!("\u{1b}[31m{}", "a".repeat(64)), "…");
        assert!(!safe.contains('\u{1b}'), "escape survived: {safe:?}");
        // The 64-character hex a node reports, elided to exactly what it
        // elides to today.
        let txid = "df69640e4cfafe7cbe9cabd3c790ed3c556f7ee340e5f10ce73dd1b590f0556d";
        assert_eq!(hash(txid, "…"), "df69640e4c…f0556d");
    }

    #[test]
    fn elision_counts_characters_not_bytes() {
        // Would panic on a byte slice.
        let text = "ééééééééééééééééééééééé";
        assert_eq!(elide(text, 3, 2, "…").chars().count(), 6);
    }
}
