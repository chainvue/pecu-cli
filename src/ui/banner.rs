//! The wordmark.
//!
//! Shown by `pecu doctor` and by the widget gallery. Small on purpose: a banner
//! that fills the window is a banner you disable.

use crate::ui::text::Text;
use crate::ui::theme::Theme;

const WORDMARK: [&str; 3] = ["┌─┐┌─┐┌─┐┬ ┬", "├─┘├┤ │  │ │", "┴  └─┘└─┘└─┘"];

/// `subtitle` lines sit to the right of the wordmark; up to two are used.
pub fn render(theme: &Theme, subtitle: &[&str]) -> String {
    let palette = &theme.palette;

    if theme.is_plain() {
        let mut out = String::from("pecu");
        for line in subtitle {
            out.push_str(" — ");
            out.push_str(line);
        }
        out.push('\n');
        return out;
    }

    let mut out = String::new();
    for (index, art) in WORDMARK.iter().enumerate() {
        let mut line = Text::raw("  ").push(*art, palette.accent);
        if let Some(text) = subtitle.get(index) {
            line = line.push("   ", palette.muted).push(*text, palette.muted);
        }
        out.push_str(&line.render());
        out.push('\n');
    }
    out
}
