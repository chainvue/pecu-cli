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

/// Make text that came from a node safe to print.
///
/// The SDK is explicit that currency and identity names are **untrusted display
/// text**: Verus permits far more in a name than it looks like it does, and the
/// node is simply repeating what somebody registered. Printed raw, a name can
/// carry ANSI escapes that repaint the terminal, a newline that forges an extra
/// row in a balance table, or characters chosen to read as an address or a
/// number.
///
/// So: control characters and escapes are replaced, everything is folded onto
/// one line, and the result is capped. What comes back is display text and
/// nothing more — the currency **id** is the part that identifies anything, and
/// it is always shown alongside.
pub fn untrusted(text: &str, max: usize, ellipsis: &str) -> String {
    let cleaned: String = text
        .chars()
        .map(|character| {
            if character.is_control() || character == '\u{7f}' {
                '·'
            } else {
                character
            }
        })
        .collect();
    fit(cleaned.trim(), max, ellipsis)
}

/// An address, shortened the way a wallet UI shortens one.
pub fn address(text: &str, ellipsis: &str) -> String {
    elide(text, 9, 4, ellipsis)
}

/// A 64-character hex hash, shortened.
pub fn hash(text: &str, ellipsis: &str) -> String {
    elide(text, 10, 6, ellipsis)
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
    fn untrusted_text_is_capped() {
        let long = "n".repeat(500);
        assert!(untrusted(&long, 20, "…").chars().count() <= 20);
    }

    #[test]
    fn ordinary_names_pass_through_untouched() {
        assert_eq!(untrusted("Bridge.vETH", 40, "…"), "Bridge.vETH");
    }

    #[test]
    fn elision_counts_characters_not_bytes() {
        // Would panic on a byte slice.
        let text = "ééééééééééééééééééééééé";
        assert_eq!(elide(text, 3, 2, "…").chars().count(), 6);
    }
}
