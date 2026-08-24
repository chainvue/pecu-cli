//! Styled text that still knows how wide it is.
//!
//! Padding a string that contains ANSI escapes by its byte length gives a
//! crooked frame, and padding by `char` count gives a crooked frame the moment
//! a token name contains anything CJK. So styled text is kept as spans until
//! the last moment: width is measured on the characters, escapes are added on
//! the way out.

use std::fmt::Write as _;

use anstyle::Style;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

#[derive(Debug, Clone)]
pub struct Span {
    pub text: String,
    pub style: Style,
}

/// A run of styled spans destined for one line.
#[derive(Debug, Clone, Default)]
pub struct Text {
    spans: Vec<Span>,
    /// Set when the text was assembled elsewhere and already contains escapes,
    /// so its width cannot be recovered from the string.
    known_width: Option<usize>,
}

impl Text {
    pub fn new() -> Self {
        Self::default()
    }

    /// A single styled run.
    pub fn of(text: impl Into<String>, style: Style) -> Self {
        Self::new().push(text, style)
    }

    /// Unstyled text.
    pub fn raw(text: impl Into<String>) -> Self {
        Self::of(text, Style::new())
    }

    #[must_use]
    pub fn push(mut self, text: impl Into<String>, style: Style) -> Self {
        let text = text.into();
        if !text.is_empty() {
            self.spans.push(Span { text, style });
        }
        self
    }

    /// Append a space, then more text. The common case when building a line.
    #[must_use]
    pub fn space(self) -> Self {
        self.push(" ", Style::new())
    }

    /// Adopt an already-escaped string whose display width the caller knows.
    /// Used by [`crate::ui::table`], which lays out its own columns.
    pub fn preformatted(escaped: String, width: usize) -> Self {
        Self {
            spans: vec![Span {
                text: escaped,
                style: Style::new(),
            }],
            known_width: Some(width),
        }
    }

    /// Display width in terminal cells.
    pub fn width(&self) -> usize {
        if let Some(width) = self.known_width {
            return width;
        }
        self.spans
            .iter()
            .map(|span| UnicodeWidthStr::width(span.text.as_str()))
            .sum()
    }

    /// Render with escapes, padded to `width` cells. Over-wide text is returned
    /// as-is rather than truncated — a broken frame is a better bug report than
    /// a silently cut address.
    pub fn render_padded(&self, width: usize) -> String {
        let mut out = self.render();
        let pad = width.saturating_sub(self.width());
        for _ in 0..pad {
            out.push(' ');
        }
        out
    }

    /// Render with escapes, right-aligned inside `width` cells.
    pub fn render_right(&self, width: usize) -> String {
        let pad = width.saturating_sub(self.width());
        let mut out = String::with_capacity(pad);
        for _ in 0..pad {
            out.push(' ');
        }
        out.push_str(&self.render());
        out
    }

    /// Break into lines no wider than `width`, keeping every span's style.
    ///
    /// Word-wrapped, because most of what needs this is prose describing an
    /// output. A single word wider than the whole line — a hash, a raw script —
    /// is hard-split rather than allowed to overflow: it has no spaces to break
    /// on, and letting it run would break the frame around it.
    ///
    /// Preformatted text is returned untouched. Its width is carried rather than
    /// measurable, so there is no honest way to split it.
    pub fn wrap(&self, width: usize) -> Vec<Text> {
        if width == 0 || self.known_width.is_some() {
            return vec![self.clone()];
        }
        // Text that already fits is returned untouched rather than taken apart
        // and put back together. Splitting on spaces and rejoining with single
        // ones would quietly destroy a deliberate run of them — a value aligned
        // by padding loses its alignment for no reason at all.
        if self.width() <= width {
            return vec![self.clone()];
        }

        let mut lines: Vec<Text> = Vec::new();
        let mut line = Text::new();
        let mut used = 0usize;
        let mut pending_space: Option<Style> = None;

        for (word, style) in self.words() {
            if word == " " {
                pending_space = Some(style);
                continue;
            }
            let word_width = UnicodeWidthStr::width(word.as_str());

            if used > 0 && used + usize::from(pending_space.is_some()) + word_width > width {
                lines.push(std::mem::take(&mut line));
                used = 0;
                pending_space = None;
            }
            if let Some(space) = pending_space.take() {
                if used > 0 {
                    line = line.push(" ", space);
                    used += 1;
                }
            }

            // Still too wide on a line of its own: chop it.
            if word_width > width {
                for chunk in chunks(&word, width) {
                    if used > 0 {
                        lines.push(std::mem::take(&mut line));
                        used = 0;
                    }
                    let chunk_width = UnicodeWidthStr::width(chunk.as_str());
                    line = line.push(chunk, style);
                    used += chunk_width;
                }
                continue;
            }

            line = line.push(word, style);
            used += word_width;
        }

        if used > 0 || lines.is_empty() {
            lines.push(line);
        }
        lines
    }

    /// Shorten from the middle so the text fits in `max` cells, keeping every
    /// span's style.
    ///
    /// [`crate::ui::fmt::fit`] does this to a plain string, which is no use to a
    /// table cell: an outpoint is a styled hash beside a differently styled
    /// `:vout`, and cutting the rendered string would cut through an escape
    /// sequence. So the cut is made on the spans and the escapes are re-emitted
    /// around what survives.
    ///
    /// Middle-out, like `fmt::fit`, because the tail is what tells two ids
    /// apart, and weighted towards the tail for the same reason. Text that
    /// already fits is returned untouched, so a column that has room keeps its
    /// ids whole and copyable.
    ///
    /// Preformatted text is returned untouched, for the same reason [`Text::wrap`]
    /// leaves it alone: its width is carried rather than measurable, so there is
    /// no honest way to cut it.
    pub fn fit(&self, max: usize, ellipsis: &str) -> Text {
        let marker = UnicodeWidthStr::width(ellipsis);
        // `max <= marker + 2` is `fmt::fit`'s own refusal: below that the result
        // is more ellipsis than text and says nothing at all.
        if self.known_width.is_some() || self.width() <= max || max <= marker + 2 {
            return self.clone();
        }
        let (before, after) = self.around(ellipsis);
        let content = max - marker;
        // Weighted towards the tail, like `fmt::fit`: the last characters of an
        // id are what tell two of them apart. Capped at what is actually there,
        // so a cut around an existing ellipsis does not reach across it.
        let tail = (content - content / 3).min(after);
        let head = (content - tail).min(before);
        let style = self
            .spans
            .first()
            .map(|span| span.style)
            .unwrap_or_default();

        let mut out = self.head(head);
        out = out.push(ellipsis, style);
        for Span { text, style } in self.tail(tail).spans {
            out = out.push(text, style);
        }
        out
    }

    /// The cells before the first ellipsis and the cells after the last one, or
    /// the whole width both ways when the text carries none.
    ///
    /// Text that has been elided once already is cut around the hole it has
    /// rather than given a second one. `wallet history` hands the table a txid
    /// that `fmt::hash` already shortened to `10…6`; cutting that as if it were
    /// one long string produces `9f9…f…9f9f9f`, which reads as two holes where
    /// there is only ever one.
    fn around(&self, ellipsis: &str) -> (usize, usize) {
        let plain: String = self.spans.iter().map(|span| span.text.as_str()).collect();
        if ellipsis.is_empty() {
            return (self.width(), self.width());
        }
        match (plain.find(ellipsis), plain.rfind(ellipsis)) {
            (Some(first), Some(last)) => (
                UnicodeWidthStr::width(&plain[..first]),
                UnicodeWidthStr::width(&plain[last + ellipsis.len()..]),
            ),
            _ => (self.width(), self.width()),
        }
    }

    /// The leading `cells` display cells. A double-width character that would
    /// straddle the boundary is dropped rather than half-printed.
    fn head(&self, cells: usize) -> Text {
        let mut out = Text::new();
        let mut used = 0usize;
        'spans: for span in &self.spans {
            let mut taken = String::new();
            for character in span.text.chars() {
                let step = UnicodeWidthChar::width(character).unwrap_or(0);
                if used + step > cells {
                    out = out.push(taken, span.style);
                    break 'spans;
                }
                taken.push(character);
                used += step;
            }
            out = out.push(taken, span.style);
        }
        out
    }

    /// The trailing `cells` display cells, with the same boundary rule.
    fn tail(&self, cells: usize) -> Text {
        let mut pieces: Vec<(String, Style)> = Vec::new();
        let mut used = 0usize;
        'spans: for span in self.spans.iter().rev() {
            let mut taken = String::new();
            for character in span.text.chars().rev() {
                let step = UnicodeWidthChar::width(character).unwrap_or(0);
                if used + step > cells {
                    pieces.push((taken.chars().rev().collect(), span.style));
                    break 'spans;
                }
                taken.push(character);
                used += step;
            }
            pieces.push((taken.chars().rev().collect(), span.style));
        }
        let mut out = Text::new();
        for (text, style) in pieces.into_iter().rev() {
            out = out.push(text, style);
        }
        out
    }

    /// Words and the single spaces between them, each carrying its span's style.
    fn words(&self) -> Vec<(String, Style)> {
        let mut pieces = Vec::new();
        for span in &self.spans {
            let mut parts = span.text.split(' ').peekable();
            while let Some(part) = parts.next() {
                if !part.is_empty() {
                    pieces.push((part.to_string(), span.style));
                }
                if parts.peek().is_some() {
                    pieces.push((" ".to_string(), span.style));
                }
            }
        }
        pieces
    }

    pub fn render(&self) -> String {
        let mut out = String::new();
        for span in &self.spans {
            if span.style == Style::new() {
                // Emitting a reset around unstyled text would litter the plain
                // theme — where every style is empty — with escapes.
                out.push_str(&span.text);
                continue;
            }
            // anstream strips these again downstream when the stream cannot
            // take them; emitting them here keeps this side simple.
            let _ = write!(
                out,
                "{}{}{}",
                span.style.render(),
                span.text,
                span.style.render_reset()
            );
        }
        out
    }
}

/// Drop SGR escapes so a rendered line can be measured the way a terminal would
/// see it. Only `ESC [ … m` is ever emitted here, so this does not need to be a
/// general ANSI parser.
#[cfg(test)]
pub fn strip_ansi(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut chars = line.chars();
    while let Some(character) = chars.next() {
        if character != '\u{1b}' {
            out.push(character);
            continue;
        }
        for escaped in chars.by_ref() {
            if escaped == 'm' {
                break;
            }
        }
    }
    out
}

/// Split a word into pieces of at most `width` cells.
///
/// Character-counted rather than width-counted: everything that reaches here is
/// a hash or a raw script, which is ASCII.
fn chunks(word: &str, width: usize) -> Vec<String> {
    word.chars()
        .collect::<Vec<_>>()
        .chunks(width.max(1))
        .map(|chunk| chunk.iter().collect())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plain(lines: &[Text]) -> Vec<String> {
        lines.iter().map(Text::render).collect()
    }

    #[test]
    fn wrapping_breaks_on_spaces_and_keeps_every_word() {
        let text = Text::raw("the quick brown fox jumps over the lazy dog");
        let lines = text.wrap(16);
        for line in &lines {
            assert!(line.width() <= 16, "too wide: {:?}", line.render());
        }
        assert_eq!(
            plain(&lines).join(" "),
            "the quick brown fox jumps over the lazy dog"
        );
    }

    #[test]
    fn wrapping_hard_splits_a_word_with_nowhere_to_break() {
        let hash = "a".repeat(50);
        let lines = Text::raw(&hash).wrap(20);
        assert_eq!(lines.len(), 3);
        for line in &lines {
            assert!(line.width() <= 20);
        }
        assert_eq!(plain(&lines).concat(), hash);
    }

    #[test]
    fn wrapping_preserves_styles_across_the_break() {
        let styled = Text::raw("aaaa ").push("bbbb", Style::new().bold());
        let lines = styled.wrap(4);
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0].render(), "aaaa");
        assert!(lines[1].render().contains("bbbb"));
        assert!(lines[1].render().contains('\u{1b}'), "style was dropped");
    }

    #[test]
    fn text_that_already_fits_comes_back_as_one_line() {
        assert_eq!(Text::raw("short").wrap(40).len(), 1);
    }

    #[test]
    fn deliberate_spacing_survives_when_no_wrapping_is_needed() {
        // A column aligned by padding. Tokenising and rejoining would collapse
        // the run to a single space and silently break the alignment.
        let aligned = Text::raw("3,481,207        node");
        let lines = aligned.wrap(60);
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].render(), "3,481,207        node");
    }

    #[test]
    fn width_ignores_escapes_and_counts_wide_characters() {
        let styled = Text::of("abc", Style::new().bold());
        assert_eq!(styled.width(), 3);
        assert!(styled.render().len() > 3, "escapes should be emitted");

        // A CJK glyph occupies two cells; counting chars would say one.
        assert_eq!(Text::raw("世界").width(), 4);
    }

    #[test]
    fn padding_is_measured_in_cells_not_bytes() {
        let padded = Text::raw("é").render_padded(4);
        assert_eq!(padded, "é   ");
    }

    #[test]
    fn over_wide_text_is_not_truncated() {
        assert_eq!(Text::raw("abcdef").render_padded(3), "abcdef");
    }

    #[test]
    fn fitting_cuts_the_middle_and_keeps_both_ends() {
        let id = "iHBwQo7LUmrTFsc9Kz7RTs2WYNXR44dK9f";
        let short = Text::raw(id).fit(14, "\u{2026}");
        assert_eq!(short.width(), 14);
        assert!(short.render().starts_with("iHBw"), "{}", short.render());
        assert!(short.render().ends_with("dK9f"), "{}", short.render());
        assert!(
            short.render().contains('\u{2026}'),
            "a cut that does not say it was cut is a different id: {}",
            short.render()
        );
    }

    #[test]
    fn fitting_leaves_text_that_already_fits_exactly_as_it_was() {
        // The half of this that matters: an id the frame has room for stays
        // copyable, character for character.
        let id = "RQC1EG3GhZ9pvT9YgCp3YvxyYBsdb4FYfH";
        assert_eq!(Text::raw(id).fit(40, "\u{2026}").render(), id);
        assert_eq!(Text::raw(id).fit(34, "\u{2026}").render(), id);
    }

    #[test]
    fn fitting_keeps_the_styles_on_both_sides_of_the_cut() {
        // An outpoint: a styled txid and a differently styled `:vout`. Cutting
        // the rendered string would cut through an escape sequence, and losing
        // the tail's style would lose the vout's colour.
        let outpoint = Text::of("9f2c1ab4de77605318bbcafe0021d4e9", Style::new().bold())
            .push(":137", Style::new().italic());
        let short = outpoint.fit(16, "\u{2026}");
        assert_eq!(short.width(), 16);
        let visible = strip_ansi(&short.render());
        assert!(visible.starts_with("9f2c"), "{visible}");
        assert!(
            visible.ends_with(":137"),
            "the vout is not decoration: {visible}"
        );
        assert!(short.render().contains("\u{1b}[1m"), "bold was dropped");
        assert!(short.render().contains("\u{1b}[3m"), "italic was dropped");
    }

    #[test]
    fn fitting_something_already_elided_does_not_give_it_a_second_hole() {
        // `wallet history` hands the table a txid that `fmt::hash` already cut
        // to `10\u{2026}6`. Cutting that as if it were one long string reaches
        // across the ellipsis and comes back with `9f9\u{2026}f\u{2026}9f9f9f`,
        // which reads as two holes where there is only ever one.
        let already = Text::raw("9f9f9f9f9f\u{2026}9f9f9f");
        for max in 8..=16 {
            let short = already.fit(max, "\u{2026}");
            let rendered = short.render();
            assert_eq!(
                rendered.matches('\u{2026}').count(),
                1,
                "two holes at {max} cells: {rendered}"
            );
            assert!(short.width() <= max, "{} cells at {max}", short.width());
            assert!(rendered.starts_with("9f"), "{rendered}");
            assert!(rendered.ends_with("9f"), "{rendered}");
        }
    }

    #[test]
    fn fitting_refuses_a_budget_too_small_to_say_anything() {
        // `fmt::fit`'s own refusal. Under it the answer is more ellipsis than
        // text, and a caller gets the untouched string and a ragged frame —
        // which is at least a legible bug.
        let id = "iHBwQo7LUmrTFsc9Kz7RTs2WYNXR44dK9f";
        assert_eq!(Text::raw(id).fit(3, "\u{2026}").render(), id);
    }

    #[test]
    fn fitting_never_splits_a_double_width_character_in_half() {
        // An odd budget against two-cell glyphs: the boundary falls inside a
        // character, and half a glyph is a cell of mojibake, not a narrower
        // cell.
        let wide = Text::raw("世界世界世界世界");
        let short = wide.fit(9, "\u{2026}");
        assert!(short.width() <= 9, "{} cells", short.width());
        assert!(!short.render().contains('\u{fffd}'));
    }
}
