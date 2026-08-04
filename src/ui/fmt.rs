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
    fn elision_counts_characters_not_bytes() {
        // Would panic on a byte slice.
        let text = "ééééééééééééééééééééééé";
        assert_eq!(elide(text, 3, 2, "…").chars().count(), 6);
    }
}
