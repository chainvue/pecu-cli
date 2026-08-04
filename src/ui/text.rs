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

#[cfg(test)]
mod tests {
    use super::*;

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
