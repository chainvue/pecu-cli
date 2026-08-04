//! The house style.
//!
//! Two skins. [`Skin::Phosphor`] is the one this tool is dressed in: green on
//! black, box-drawing frames, dim labels beside bright values. [`Skin::Plain`]
//! is the same information with the frames and colour taken out — what you get
//! when the output is piped somewhere, and what the snapshot tests read.
//!
//! Colour is decided twice on purpose. This module decides whether to *ask* for
//! colour; `anstream` decides whether the stream can take it, and strips escapes
//! when it cannot or when `NO_COLOR` is set. So `--theme phosphor | cat` still
//! draws its frames, just without the green.

use anstyle::{Ansi256Color, Color, Style};

use crate::cli::Theme as ThemeFlag;

/// Widest panel we will ever draw, however wide the terminal is. Long lines of
/// hex are hard to scan; the frame is not improved by being 200 columns wide.
const MAX_WIDTH: usize = 78;

/// Narrowest panel we will draw, however narrow the terminal is.
const MIN_WIDTH: usize = 48;

/// Assumed terminal width when there is no terminal to ask.
const FALLBACK_WIDTH: usize = 80;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Skin {
    Phosphor,
    Plain,
}

#[derive(Debug, Clone)]
pub struct Theme {
    pub skin: Skin,
    pub palette: Palette,
    pub glyphs: Glyphs,
    /// Inner width of a panel, excluding the frame characters.
    pub width: usize,
}

impl Theme {
    /// Resolve the `--theme` flag against the terminal we actually have.
    pub fn resolve(flag: ThemeFlag, is_terminal: bool) -> Self {
        let skin = match flag {
            ThemeFlag::Phosphor => Skin::Phosphor,
            ThemeFlag::Plain => Skin::Plain,
            ThemeFlag::Auto if is_terminal => Skin::Phosphor,
            ThemeFlag::Auto => Skin::Plain,
        };
        Self::with_skin(skin, terminal_width())
    }

    pub fn with_skin(skin: Skin, terminal_width: usize) -> Self {
        // Two columns for the frame itself, two more so the frame is not flush
        // against the right edge of the window.
        let usable = terminal_width.saturating_sub(4);
        Self {
            skin,
            palette: match skin {
                Skin::Phosphor => Palette::phosphor(),
                Skin::Plain => Palette::none(),
            },
            glyphs: match skin {
                Skin::Phosphor => Glyphs::unicode(),
                Skin::Plain => Glyphs::ascii(),
            },
            width: usable.clamp(MIN_WIDTH, MAX_WIDTH),
        }
    }

    pub fn is_plain(&self) -> bool {
        self.skin == Skin::Plain
    }
}

/// `$PECU_WIDTH` wins, then the real terminal, then a sane guess. The override
/// exists so a snapshot test can pin the width without owning a pty.
fn terminal_width() -> usize {
    if let Ok(forced) = std::env::var("PECU_WIDTH") {
        if let Ok(width) = forced.parse::<usize>() {
            return width;
        }
    }
    terminal_size::terminal_size()
        .map(|(terminal_size::Width(w), _)| usize::from(w))
        .unwrap_or(FALLBACK_WIDTH)
}

/// Every colour the tool uses. Nothing outside this struct picks a colour.
#[derive(Debug, Clone, Copy)]
pub struct Palette {
    /// The box-drawing characters themselves.
    pub frame: Style,
    /// A panel or section title.
    pub title: Style,
    /// The left-hand column: field names.
    pub label: Style,
    /// The right-hand column: the thing you came to read.
    pub value: Style,
    /// A value that matters more than the others on the line.
    pub accent: Style,
    /// Present but not important — truncated hashes, counts, units.
    pub muted: Style,
    pub ok: Style,
    pub warn: Style,
    pub danger: Style,
}

impl Palette {
    /// Green CRT. Deliberately 256-colour rather than truecolor: it survives
    /// tmux, ssh and terminals that lie about their capabilities.
    fn phosphor() -> Self {
        let green = |n: u8| Style::new().fg_color(Some(Color::Ansi256(Ansi256Color(n))));
        Self {
            frame: green(28),
            title: green(46).bold(),
            label: green(245),
            value: green(194),
            accent: green(84).bold(),
            muted: green(240),
            ok: green(46),
            warn: green(214),
            danger: green(203).bold(),
        }
    }

    fn none() -> Self {
        let plain = Style::new();
        Self {
            frame: plain,
            title: plain,
            label: plain,
            value: plain,
            accent: plain,
            muted: plain,
            ok: plain,
            warn: plain,
            danger: plain,
        }
    }
}

/// Characters that differ between skins.
#[derive(Debug, Clone, Copy)]
pub struct Glyphs {
    pub top_left: char,
    pub top_right: char,
    pub bottom_left: char,
    pub bottom_right: char,
    pub horizontal: char,
    pub vertical: char,
    pub tee_left: char,
    pub tee_right: char,
    pub bullet: &'static str,
    pub arrow: &'static str,
    pub ok: &'static str,
    pub warn: &'static str,
    pub danger: &'static str,
    pub ellipsis: &'static str,
}

impl Glyphs {
    fn unicode() -> Self {
        Self {
            top_left: '┌',
            top_right: '┐',
            bottom_left: '└',
            bottom_right: '┘',
            horizontal: '─',
            vertical: '│',
            tee_left: '├',
            tee_right: '┤',
            bullet: "▸",
            arrow: "→",
            ok: "✓",
            warn: "▲",
            danger: "✗",
            ellipsis: "…",
        }
    }

    fn ascii() -> Self {
        Self {
            top_left: '+',
            top_right: '+',
            bottom_left: '+',
            bottom_right: '+',
            horizontal: '-',
            vertical: '|',
            tee_left: '+',
            tee_right: '+',
            bullet: "-",
            arrow: "->",
            ok: "ok",
            warn: "!",
            danger: "x",
            ellipsis: "...",
        }
    }
}
