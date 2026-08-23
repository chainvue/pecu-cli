//! Everything that decides how `pecu` looks.
//!
//! Commands build [`Panel`]s and [`Table`]s and hand them to a [`Ui`]; nothing
//! outside this module writes an escape sequence or a box-drawing character.
//! That is what makes `--theme plain`, `NO_COLOR` and `--json` a single
//! decision each rather than a flag every command has to remember.

pub mod banner;
pub mod fmt;
pub mod panel;
pub mod qr;
pub mod table;
pub mod text;
pub mod theme;

use std::io::IsTerminal;

pub use panel::Panel;
pub use table::{Align, Column, Table};
pub use text::Text;
pub use theme::Theme;

use crate::cli::Theme as ThemeFlag;
use crate::explain::Explain;

/// The output surface. Holds the resolved theme, knows whether the caller asked
/// for JSON, and records SDK calls for `--explain`.
pub struct Ui {
    pub theme: Theme,
    json: bool,
    explain: Explain,
}

impl Ui {
    pub fn new(flag: ThemeFlag, json: bool, explain: bool) -> Self {
        Self {
            theme: Theme::resolve(flag, std::io::stdout().is_terminal()),
            json,
            explain: Explain::new(explain),
        }
    }

    /// Record an SDK call. A no-op unless `--explain` was given.
    pub fn sdk(&self, call: impl Into<String>) {
        self.explain.call(call);
    }

    /// Summarise what the last recorded call returned.
    pub fn sdk_result(&self, result: impl Into<String>) {
        self.explain.result(result);
    }

    /// Print the recorded calls, if there are any and the flag is on.
    ///
    /// Under `--json` it goes to stderr instead of being dropped. `--explain`
    /// is documented as working on any command, and stdout belongs to the
    /// document — but the panel is prose for a person, and stderr is where the
    /// prose on a `--json` run already lives.
    pub fn explain_panel(&self) {
        let Some(panel) = self.explain.panel(&self.theme) else {
            return;
        };
        if self.json {
            anstream::eprintln!();
            anstream::eprint!("{}", panel.render(&self.theme));
            return;
        }
        self.blank();
        self.panel(&panel);
    }

    /// Whether the caller wants machine-readable output. Commands check this
    /// and serialise instead of rendering; the renderer itself is never asked
    /// to produce JSON.
    pub fn is_json(&self) -> bool {
        self.json
    }

    /// In `--json` mode stdout belongs to the document and nothing else.
    ///
    /// Checked here rather than at every call site, which is how `key export
    /// --json` came to print 221 bytes of prose ahead of its refusal and break
    /// `| jq` on garbage instead of on emptiness (#49). Call sites still ask
    /// [`Ui::is_json`] to decide *what* to compute; this decides whether a
    /// rendered thing may be written at all, so the next `ui.note` before an
    /// `Err` cannot reintroduce the same leak.
    fn renders(&self) -> bool {
        !self.json
    }

    pub fn banner(&self, subtitle: &[&str]) {
        if !self.renders() {
            return;
        }
        anstream::print!("{}", banner::render(&self.theme, subtitle));
    }

    pub fn panel(&self, panel: &Panel) {
        if !self.renders() {
            return;
        }
        anstream::print!("{}", panel.render(&self.theme));
    }

    pub fn blank(&self) {
        if !self.renders() {
            return;
        }
        anstream::println!();
    }

    /// A line of prose outside any frame.
    pub fn line(&self, text: Text) {
        if !self.renders() {
            return;
        }
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
        if !self.renders() {
            return;
        }
        anstream::println!(
            "  {} {}",
            Text::of(glyph, style).render(),
            Text::of(message.as_ref().to_string(), self.theme.palette.value).render()
        );
    }
}
