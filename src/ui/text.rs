//! Styled text that still knows how wide it is.
//!
//! Padding a string that contains ANSI escapes by its byte length gives a
//! crooked frame, and padding by `char` count gives a crooked frame the moment
//! a token name contains anything CJK. So styled text is kept as spans until
//! the last moment: width is measured on the characters, escapes are added on
//! the way out.

use std::fmt::Write as _;

use anstyle::Style;
use unicode_width::UnicodeWidthStr;

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
}
