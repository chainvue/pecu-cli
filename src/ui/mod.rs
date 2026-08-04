//! Everything that decides how `pecu` looks.
//!
//! Commands build [`Panel`]s and [`Table`]s and hand them to a [`Ui`]; nothing
//! outside this module writes an escape sequence or a box-drawing character.
//! That is what makes `--theme plain`, `NO_COLOR` and `--json` a single
//! decision each rather than a flag every command has to remember.

pub mod banner;
pub mod fmt;
pub mod panel;
pub mod table;
pub mod text;
pub mod theme;

use std::io::IsTerminal;

pub use panel::Panel;
pub use table::{Align, Column, Table};
pub use text::Text;
pub use theme::Theme;

use crate::cli::Theme as ThemeFlag;

/// The output surface. Holds the resolved theme and knows whether the caller
/// asked for JSON.
pub struct Ui {
    pub theme: Theme,
    json: bool,
}

impl Ui {
    pub fn new(flag: ThemeFlag, json: bool) -> Self {
        Self {
            theme: Theme::resolve(flag, std::io::stdout().is_terminal()),
            json,
        }
    }

    /// Whether the caller wants machine-readable output. Commands check this
    /// and serialise instead of rendering; the renderer itself is never asked
    /// to produce JSON.
    pub fn is_json(&self) -> bool {
        self.json
    }

    pub fn banner(&self, subtitle: &[&str]) {
        anstream::print!("{}", banner::render(&self.theme, subtitle));
    }

    pub fn panel(&self, panel: &Panel) {
        anstream::print!("{}", panel.render(&self.theme));
    }

    pub fn blank(&self) {
        anstream::println!();
    }

    /// A line of prose outside any frame.
    pub fn line(&self, text: Text) {
        anstream::println!("{}", text.render());
    }

    pub fn ok(&self, message: impl AsRef<str>) {
        self.status(self.theme.glyphs.ok, self.theme.palette.ok, message);
    }

    pub fn warn(&self, message: impl AsRef<str>) {
        self.status(self.theme.glyphs.warn, self.theme.palette.warn, message);
    }

    pub fn fail(&self, message: impl AsRef<str>) {
        self.status(self.theme.glyphs.danger, self.theme.palette.danger, message);
    }

    pub fn note(&self, message: impl AsRef<str>) {
        self.status(self.theme.glyphs.bullet, self.theme.palette.muted, message);
    }

    fn status(&self, glyph: &str, style: anstyle::Style, message: impl AsRef<str>) {
        anstream::println!(
            "  {} {}",
            Text::of(glyph, style).render(),
            Text::of(message.as_ref().to_string(), self.theme.palette.value).render()
        );
    }
}
