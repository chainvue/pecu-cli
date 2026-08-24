//! The house style.
//!
//! Three skins. [`Skin::Phosphor`] is the one this tool is dressed in: green on
//! black, box-drawing frames, dim labels beside bright values. [`Skin::Light`]
//! is the same layout re-inked for a terminal with a light background, which
//! phosphor is unreadable on and which no terminal profile can fix — see
//! [`Palette::light`] for why. [`Skin::Plain`] is the same information with the
//! frames and colour taken out — what you get when the output is piped
//! somewhere, and what the snapshot tests read.
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
    Light,
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
    ///
    /// The only thing asked of the terminal is whether it is one. `Auto` never
    /// picks [`Skin::Light`]: the two ways to detect a light background are
    /// `$COLORFGBG`, which most terminals do not set, and an OSC 11 query,
    /// which means putting the tty into raw mode and waiting on a reply — a new
    /// way for a spend confirmation to fail. A light terminal is a fact about
    /// the reader, so the reader states it: `--theme light`, or `PECU_THEME`
    /// once.
    pub fn resolve(flag: ThemeFlag, is_terminal: bool) -> Self {
        let skin = match flag {
            ThemeFlag::Phosphor => Skin::Phosphor,
            ThemeFlag::Light => Skin::Light,
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
                Skin::Light => Palette::light(),
                Skin::Plain => Palette::none(),
            },
            glyphs: match skin {
                Skin::Phosphor | Skin::Light => Glyphs::unicode(),
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
    ///
    /// Every index here is ≥ 16, so it is outside the sixteen slots a terminal
    /// profile can remap. That is the cost of the choice above: on a light
    /// background these colours arrive exactly as written, and `value` at
    /// `#d7ffd7` is 1.10:1 on white. [`Palette::light`] is the answer, not a
    /// profile.
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

    /// Dark ink on paper: the same nine roles for a light terminal.
    ///
    /// 256-colour for the same reason [`Palette::phosphor`] is, and every
    /// colour measured against `#ffffff` rather than asserted — WCAG 2.x, sRGB,
    /// pinned by `light_palette_clears_wcag_aa` below:
    ///
    /// | role | index | hex | contrast |
    /// |---|---|---|---|
    /// | `frame` | 28 | `#008700` | 4.70:1 |
    /// | `title` | 22 | `#005f00` | 7.96:1 |
    /// | `label` | 238 | `#444444` | 9.74:1 |
    /// | `value` | 235 | `#262626` | 15.13:1 |
    /// | `accent` | 25 | `#005faf` | 6.45:1 |
    /// | `muted` | 242 | `#6c6c6c` | 5.25:1 |
    /// | `ok` | 22 | `#005f00` | 7.96:1 |
    /// | `warn` | 130 | `#af5f00` | 4.71:1 |
    /// | `danger` | 124 | `#af0000` | 7.44:1 |
    ///
    /// It is not phosphor with the greens darkened, because it cannot be: of
    /// the 240 fixed cube colours exactly three greens clear 4.5:1 on white
    /// (22, 28 and 29, and 29 is `#00875f` at 4.53:1, indistinguishable from
    /// 28). Two of them go to the frame and the title, and the rest of the
    /// panel is ink — value darkest, label mid, muted lightest, keeping the
    /// order phosphor has on black — with one blue for `accent`, the figure
    /// that matters most on the line. `ok` shares `title`'s green exactly as it
    /// shares index 46 in phosphor, so the two skins have the same number of
    /// distinct colours and a capture can be re-inked from one to the other.
    fn light() -> Self {
        let ink = |n: u8| Style::new().fg_color(Some(Color::Ansi256(Ansi256Color(n))));
        Self {
            frame: ink(28),
            title: ink(22).bold(),
            label: ink(238),
            value: ink(235),
            accent: ink(25).bold(),
            muted: ink(242),
            ok: ink(22),
            warn: ink(130),
            danger: ink(124).bold(),
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

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::*;

    /// Text has to clear this against the surface it is drawn on. WCAG 2.x AA
    /// for body text; the panels are 12–14px in the captures, so the 3:1 large-
    /// text allowance does not apply to any of it.
    const AA: f64 = 4.5;

    const PAPER: &str = "#ffffff";
    /// `docs/media/dark.xresources`, and what a phosphor terminal is assumed to be.
    const PHOSPHOR_SURFACE: &str = "#0d1117";

    /// Destructured rather than read field by field, so that adding a tenth
    /// role to [`Palette`] is a compile error here instead of a colour that
    /// quietly ships without ever being measured.
    fn roles(palette: &Palette) -> [(&'static str, Style); 9] {
        let Palette {
            frame,
            title,
            label,
            value,
            accent,
            muted,
            ok,
            warn,
            danger,
        } = *palette;
        [
            ("frame", frame),
            ("title", title),
            ("label", label),
            ("value", value),
            ("accent", accent),
            ("muted", muted),
            ("ok", ok),
            ("warn", warn),
            ("danger", danger),
        ]
    }

    /// The 256-colour index a role asks for.
    ///
    /// Panics on 0–15 rather than resolving them, and that is the point: those
    /// sixteen are whatever the reader's terminal profile says they are, so a
    /// palette that used them could not promise a contrast ratio at all. Both
    /// real palettes stay above 15, which is why the numbers below are ours to
    /// assert.
    fn index(role: &str, style: Style) -> u8 {
        match style.get_fg_color() {
            Some(Color::Ansi256(Ansi256Color(n))) if n >= 16 => n,
            Some(Color::Ansi256(Ansi256Color(n))) => {
                panic!("{role} uses profile-defined colour {n}; its contrast would be the terminal's to decide")
            }
            other => panic!("{role} is not a 256-colour index: {other:?}"),
        }
    }

    /// xterm's 256-colour table: a 6×6×6 cube from index 16, then a 24-step grey ramp.
    fn hex(index: u8) -> String {
        const LEVEL: [u32; 6] = [0, 95, 135, 175, 215, 255];
        let n = u32::from(index);
        let (r, g, b) = if n >= 232 {
            let grey = 8 + 10 * (n - 232);
            (grey, grey, grey)
        } else {
            let c = n - 16;
            (
                LEVEL[(c / 36) as usize],
                LEVEL[((c / 6) % 6) as usize],
                LEVEL[(c % 6) as usize],
            )
        };
        format!("#{r:02x}{g:02x}{b:02x}")
    }

    fn channel(value: u32) -> f64 {
        let value = f64::from(value) / 255.0;
        if value <= 0.03928 {
            value / 12.92
        } else {
            ((value + 0.055) / 1.055).powf(2.4)
        }
    }

    /// WCAG 2.x relative luminance, sRGB.
    fn luminance(colour: &str) -> f64 {
        let digits = colour.trim_start_matches('#');
        let digits = if digits.len() == 3 {
            digits.chars().flat_map(|c| [c, c]).collect()
        } else {
            digits.to_string()
        };
        let byte = |at: usize| u32::from_str_radix(&digits[at..at + 2], 16).expect("hex colour");
        0.2126 * channel(byte(0)) + 0.7152 * channel(byte(2)) + 0.0722 * channel(byte(4))
    }

    fn contrast(a: &str, b: &str) -> f64 {
        let (a, b) = (luminance(a), luminance(b));
        (a.max(b) + 0.05) / (a.min(b) + 0.05)
    }

    #[test]
    fn the_contrast_arithmetic_agrees_with_the_published_examples() {
        // Two ratios anyone can check by hand: black on white is 21:1, and a
        // colour on itself is 1:1.
        assert!((contrast("#000000", "#ffffff") - 21.0).abs() < 0.01);
        assert!((contrast("#8a8a8a", "#8a8a8a") - 1.0).abs() < 0.001);
    }

    /// The invariant this skin exists for. Nine roles, measured, not asserted.
    #[test]
    fn light_palette_clears_wcag_aa() {
        for (role, style) in roles(&Palette::light()) {
            let colour = hex(index(role, style));
            let ratio = contrast(&colour, PAPER);
            assert!(
                ratio >= AA,
                "light `{role}` is {colour} on {PAPER}: {ratio:.2}:1, below {AA}:1"
            );
        }
    }

    /// Phosphor is pinned rather than measured, because two of its colours are
    /// below 4.5:1 on `#0d1117` and always have been: `muted` (240) at 2.66:1
    /// and `frame` (28) at 4.02:1. That is a separate defect from the light one
    /// and fixing it here would change what every existing user sees, so this
    /// test's job is to prove the dark side did *not* move.
    #[test]
    fn the_phosphor_palette_is_unchanged() {
        let expected = [
            ("frame", 28u8),
            ("title", 46),
            ("label", 245),
            ("value", 194),
            ("accent", 84),
            ("muted", 240),
            ("ok", 46),
            ("warn", 214),
            ("danger", 203),
        ];
        for ((role, style), (named, want)) in roles(&Palette::phosphor()).into_iter().zip(expected)
        {
            assert_eq!(role, named);
            assert_eq!(index(role, style), want, "phosphor `{role}` moved");
        }
        // And the two known-thin ones are still exactly as thin, so a change
        // there has to be deliberate.
        assert!((contrast(&hex(240), PHOSPHOR_SURFACE) - 2.66).abs() < 0.01);
        assert!((contrast(&hex(28), PHOSPHOR_SURFACE) - 4.02).abs() < 0.01);
    }

    /// `Skin::Light` is only reachable by asking for it.
    #[test]
    fn auto_never_picks_the_light_skin() {
        assert_eq!(Theme::resolve(ThemeFlag::Auto, true).skin, Skin::Phosphor);
        assert_eq!(Theme::resolve(ThemeFlag::Auto, false).skin, Skin::Plain);
        assert_eq!(Theme::resolve(ThemeFlag::Light, true).skin, Skin::Light);
        assert_eq!(Theme::resolve(ThemeFlag::Light, false).skin, Skin::Light);
    }

    /// The light skin is a re-inking, not a redesign: same glyphs, same width,
    /// so a capture of one is a capture of the other with the colours swapped.
    #[test]
    fn the_light_skin_keeps_the_phosphor_geometry() {
        let light = Theme::with_skin(Skin::Light, 84);
        let phosphor = Theme::with_skin(Skin::Phosphor, 84);
        assert_eq!(light.width, phosphor.width);
        assert_eq!(light.glyphs.vertical, phosphor.glyphs.vertical);
        assert_eq!(light.glyphs.ok, phosphor.glyphs.ok);
        assert!(!light.is_plain());
    }

    // --- the published captures -------------------------------------------
    //
    // `docs/media/*.svg` are recordings of this palette, and a recording cannot
    // be regenerated cheaply: `record.sh` keeps no `.cast`, and re-recording
    // `register` spends real VRSCTEST. So the captures are checked here, where
    // the palette they are supposed to show actually lives.

    fn captures(variant: &str) -> Vec<(String, String)> {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("docs/media");
        let mut found: Vec<_> = std::fs::read_dir(&dir)
            .expect("docs/media should be readable")
            .filter_map(|entry| {
                let path = entry.expect("a readable directory entry").path();
                let name = path.file_name()?.to_str()?.to_string();
                name.ends_with(&format!("-{variant}.svg")).then(|| {
                    (
                        name,
                        std::fs::read_to_string(&path).expect("a readable svg"),
                    )
                })
            })
            .collect();
        found.sort();
        assert_eq!(found.len(), 7, "expected seven {variant} captures");
        found
    }

    /// Every colour an SVG paints with, normalised to six digits. Reads the
    /// declarations rather than the cascade on purpose: a rule that is
    /// overridden later still has to be a colour we chose, and checking all of
    /// them is strictly stronger than checking the ones that win.
    fn fills(svg: &str) -> BTreeSet<String> {
        let mut found = BTreeSet::new();
        for (at, _) in svg.match_indices("fill") {
            let rest = &svg[at + "fill".len()..];
            let rest = match rest.strip_prefix(':').or_else(|| rest.strip_prefix("=\"")) {
                Some(rest) => rest,
                None => continue,
            };
            let Some(rest) = rest.strip_prefix('#') else {
                continue;
            };
            let digits: String = rest
                .chars()
                .take(6)
                .take_while(|c| c.is_ascii_hexdigit())
                .collect();
            let digits = match digits.len() {
                3 => digits.chars().flat_map(|c| [c, c]).collect(),
                6 => digits,
                _ => continue,
            };
            found.insert(format!("#{}", digits.to_lowercase()));
        }
        found
    }

    /// Window chrome drawn by `svg-term`, not by `pecu`: three title-bar
    /// buttons and the cursor block. Shapes, not text.
    const CHROME: [&str; 5] = ["#ff5f58", "#ffbd2e", "#18c132", "#0969da", "#58a6ff"];

    /// The bug in the README, pinned: every colour in a light capture is one
    /// this program would emit under `--theme light`, and all of them are
    /// legible on the paper they are drawn on. Before the fix the worst was
    /// `#d7ffd7` at 1.10:1 — the amount, the fee and the txid, invisible beside
    /// labels that were not.
    #[test]
    fn the_published_light_captures_are_the_light_palette() {
        let palette: BTreeSet<String> = roles(&Palette::light())
            .into_iter()
            .map(|(role, style)| hex(index(role, style)))
            .collect();
        for (name, svg) in captures("light") {
            let background = "#ffffff";
            for colour in fills(&svg) {
                if CHROME.contains(&colour.as_str()) || colour == background {
                    continue;
                }
                // `#1f2328` is the xresources foreground: the shell's own echo
                // of the demo command, which `pecu` never colours.
                let ours = palette.contains(&colour) || colour == "#1f2328";
                assert!(
                    ours,
                    "{name} paints {colour}, which no `--theme light` role emits — \
                     re-ink it with docs/media/relight.py"
                );
                // Not redundant with the line above, though it looks it: a
                // palette colour reaches this already knowing it clears AA,
                // because `light_palette_clears_wcag_aa` measured all nine. The
                // exceptions are what this catches — `#1f2328` here, and
                // anything a later reader adds to that `||`. An exemption from
                // "is it ours" is not an exemption from "can it be read".
                let ratio = contrast(&colour, background);
                assert!(
                    ratio >= AA,
                    "{name}: {colour} on {background} is {ratio:.2}:1, below {AA}:1"
                );
            }
        }
    }

    /// The other half of the promise: fixing the light captures did not touch
    /// the dark ones, which are still the phosphor palette exactly.
    #[test]
    fn the_published_dark_captures_are_still_phosphor() {
        let mut allowed: BTreeSet<String> = roles(&Palette::phosphor())
            .into_iter()
            .map(|(role, style)| hex(index(role, style)))
            .collect();
        allowed.extend(CHROME.iter().map(|c| (*c).to_string()));
        // Background, foreground, and the `$` sigil `record.sh` prints (index 120).
        allowed.extend(["#0d1117", "#c9d1d9", "#87ff87"].map(String::from));
        for (name, svg) in captures("dark") {
            for colour in fills(&svg) {
                assert!(
                    allowed.contains(&colour),
                    "{name} paints {colour}, which phosphor does not — the dark \
                     captures are not supposed to change"
                );
            }
        }
    }
}
