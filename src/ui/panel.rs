//! The framed block that most of this tool's output lives in.
//!
//! A panel is built once and rendered twice over: with box-drawing and colour
//! under [`Skin::Phosphor`](crate::ui::theme::Skin), and as indented plain text
//! otherwise. Callers never branch on the skin — they describe what the block
//! contains, and the renderer decides how it looks.

use crate::ui::fmt;
use crate::ui::table::Table;
use crate::ui::text::Text;
use crate::ui::theme::Theme;

/// Space between a row's label column and its value column.
const LABEL_GUTTER: usize = 3;

/// A panel narrower than this looks like a mistake rather than a frame.
const MIN_PANEL_WIDTH: usize = 36;

/// One line of a framed panel, after the items have been laid out but before a
/// width has been chosen.
enum Drawn {
    Divider(Option<String>),
    Content(Text),
}

#[derive(Debug, Clone)]
enum Item {
    /// A label/value pair, aligned with every other row in the same panel.
    Row {
        label: String,
        value: Text,
    },
    /// A label/value pair whose value is a filesystem path.
    ///
    /// Kept apart from [`Item::Row`] because a path is the one value that may be
    /// shortened: it can be arbitrarily long, and the renderer is the only thing
    /// that knows both the label column and the width it has to fit into. An
    /// address or a hash is never shortened this way — silently cutting one is
    /// worse than a ragged frame.
    Path {
        label: String,
        path: String,
    },
    /// A line that spans the panel.
    Line(Text),
    /// Prose, wrapped to the panel and indented on every line.
    Wrapped {
        indent: usize,
        text: Text,
    },
    /// A divider carrying a title.
    Section(String),
    /// A plain divider.
    Rule,
    Table(Table),
    Blank,
}

#[derive(Debug, Clone)]
pub struct Panel {
    title: String,
    items: Vec<Item>,
    notes: Vec<Text>,
}

impl Panel {
    pub fn new(title: impl Into<String>) -> Self {
        Self {
            title: title.into(),
            items: Vec::new(),
            notes: Vec::new(),
        }
    }

    #[must_use]
    pub fn row(mut self, label: impl Into<String>, value: Text) -> Self {
        self.items.push(Item::Row {
            label: label.into(),
            value,
        });
        self
    }

    /// A row holding a path, shortened from the middle if it will not fit and
    /// with `$HOME` collapsed to `~`.
    #[must_use]
    pub fn path(mut self, label: impl Into<String>, path: &std::path::Path) -> Self {
        self.items.push(Item::Path {
            label: label.into(),
            path: crate::config::tildify(path),
        });
        self
    }

    #[must_use]
    pub fn line(mut self, text: Text) -> Self {
        self.items.push(Item::Line(text));
        self
    }

    /// A line of prose, wrapped to fit rather than allowed to break the frame,
    /// with `indent` spaces in front of every line it becomes.
    #[must_use]
    pub fn wrapped(mut self, indent: usize, text: Text) -> Self {
        self.items.push(Item::Wrapped { indent, text });
        self
    }

    #[must_use]
    pub fn section(mut self, title: impl Into<String>) -> Self {
        self.items.push(Item::Section(title.into()));
        self
    }

    #[must_use]
    pub fn rule(mut self) -> Self {
        self.items.push(Item::Rule);
        self
    }

    #[must_use]
    pub fn table(mut self, table: Table) -> Self {
        self.items.push(Item::Table(table));
        self
    }

    #[must_use]
    pub fn blank(mut self) -> Self {
        self.items.push(Item::Blank);
        self
    }

    /// A remark that hangs below the frame rather than inside it.
    #[must_use]
    pub fn note(mut self, text: Text) -> Self {
        self.notes.push(text);
        self
    }

    pub fn render(&self, theme: &Theme) -> String {
        if theme.is_plain() {
            self.render_plain(theme)
        } else {
            self.render_framed(theme)
        }
    }

    /// Width of the widest label, so every row's value starts in the same column.
    fn label_width(&self) -> usize {
        self.items
            .iter()
            .filter_map(|item| match item {
                Item::Row { label, .. } | Item::Path { label, .. } => Some(label.chars().count()),
                _ => None,
            })
            .max()
            .unwrap_or(0)
    }

    /// Flatten the items into the two things the frame can draw: a divider, or
    /// a line of content. Done before any width is chosen, because the width is
    /// chosen from these.
    fn flatten(&self, theme: &Theme) -> Vec<Drawn> {
        let palette = &theme.palette;
        let label_width = self.label_width();
        let mut drawn = Vec::with_capacity(self.items.len());

        for item in &self.items {
            match item {
                Item::Section(title) => drawn.push(Drawn::Divider(Some(title.clone()))),
                Item::Rule => drawn.push(Drawn::Divider(None)),
                Item::Blank => drawn.push(Drawn::Content(Text::new())),
                Item::Line(text) => drawn.push(Drawn::Content(text.clone())),
                Item::Wrapped { indent, text } => {
                    // Wrapped against the theme's ceiling, not the final panel
                    // width — that is not decided yet, and cannot exceed this.
                    let budget = theme.width.saturating_sub(*indent);
                    for line in text.wrap(budget) {
                        let width = line.width();
                        let padded = format!("{}{}", " ".repeat(*indent), line.render());
                        drawn.push(Drawn::Content(Text::preformatted(padded, indent + width)));
                    }
                }
                Item::Row { label, value } => {
                    let assembled = Text::of(pad(label, label_width), palette.label)
                        .push(" ".repeat(LABEL_GUTTER), palette.label)
                        .push(value.render(), Default::default());
                    // `value` is already escaped, so its width has to be carried
                    // rather than re-measured off the string.
                    drawn.push(Drawn::Content(Text::preformatted(
                        assembled.render(),
                        label_width + LABEL_GUTTER + value.width(),
                    )));
                }
                Item::Path { label, path } => {
                    // Fitted against the theme's ceiling rather than the final
                    // panel width, which is not known yet — and cannot be
                    // smaller than this, so the row is guaranteed to fit.
                    let budget = theme.width.saturating_sub(label_width + LABEL_GUTTER);
                    let shown = fmt::fit(path, budget, theme.glyphs.ellipsis);
                    let value = Text::of(shown, palette.muted);
                    let assembled = Text::of(pad(label, label_width), palette.label)
                        .push(" ".repeat(LABEL_GUTTER), palette.label)
                        .push(value.render(), Default::default());
                    drawn.push(Drawn::Content(Text::preformatted(
                        assembled.render(),
                        label_width + LABEL_GUTTER + value.width(),
                    )));
                }
                Item::Table(table) => {
                    drawn.extend(table.lines(theme).into_iter().map(Drawn::Content));
                }
            }
        }
        drawn
    }

    fn render_framed(&self, theme: &Theme) -> String {
        let glyphs = &theme.glyphs;
        let palette = &theme.palette;
        let drawn = self.flatten(theme);

        // Shrink to fit. A panel holding one short line should not be as wide
        // as the window; `theme.width` is the ceiling, not the target.
        let widest_content = drawn
            .iter()
            .filter_map(|line| match line {
                Drawn::Content(text) => Some(text.width()),
                Drawn::Divider(_) => None,
            })
            .max()
            .unwrap_or(0);
        // Every divider title has to fit between the corners too: `─ TITLE ─`.
        let widest_title = drawn
            .iter()
            .filter_map(|line| match line {
                Drawn::Divider(title) => title.as_deref(),
                Drawn::Content(_) => None,
            })
            .chain(std::iter::once(self.title.as_str()))
            .map(|title| title.chars().count() + 2)
            .max()
            .unwrap_or(0);
        let inner = widest_content
            .max(widest_title)
            .clamp(MIN_PANEL_WIDTH, theme.width);

        let mut out = titled_border(
            theme,
            inner,
            glyphs.top_left,
            glyphs.top_right,
            Some(&self.title),
        );
        for line in &drawn {
            out.push_str(&match line {
                Drawn::Divider(title) => titled_border(
                    theme,
                    inner,
                    glyphs.tee_left,
                    glyphs.tee_right,
                    title.as_deref(),
                ),
                Drawn::Content(text) => content_line(theme, text, inner),
            });
        }
        out.push_str(&titled_border(
            theme,
            inner,
            glyphs.bottom_left,
            glyphs.bottom_right,
            None,
        ));

        for note in &self.notes {
            out.push_str(&format!(
                "  {} {}\n",
                Text::of(glyphs.bullet, palette.muted).render(),
                note.render()
            ));
        }
        out
    }

    fn render_plain(&self, theme: &Theme) -> String {
        let label_width = self.label_width();
        let mut out = String::new();

        if !self.title.is_empty() {
            out.push_str(&self.title);
            out.push('\n');
        }

        for item in &self.items {
            match item {
                Item::Section(title) => {
                    out.push('\n');
                    out.push_str(title);
                    out.push('\n');
                }
                Item::Rule | Item::Blank => out.push('\n'),
                Item::Line(text) => {
                    out.push_str("  ");
                    out.push_str(&text.render());
                    out.push('\n');
                }
                // Left unwrapped: there is no frame to break, and piped output
                // is likelier to be fed to something than read.
                Item::Wrapped { indent, text } => {
                    out.push_str("  ");
                    out.push_str(&" ".repeat(*indent));
                    out.push_str(&text.render());
                    out.push('\n');
                }
                Item::Row { label, value } => {
                    out.push_str("  ");
                    out.push_str(&pad(label, label_width));
                    out.push_str(&" ".repeat(LABEL_GUTTER));
                    out.push_str(&value.render());
                    out.push('\n');
                }
                // No frame to break, so the path goes out whole. Piped output
                // is likelier to be fed to something than read.
                Item::Path { label, path } => {
                    out.push_str("  ");
                    out.push_str(&pad(label, label_width));
                    out.push_str(&" ".repeat(LABEL_GUTTER));
                    out.push_str(path);
                    out.push('\n');
                }
                Item::Table(table) => {
                    for line in table.lines(theme) {
                        out.push_str("  ");
                        out.push_str(&line.render());
                        out.push('\n');
                    }
                }
            }
        }

        for note in &self.notes {
            out.push_str(&format!("  {} {}\n", theme.glyphs.bullet, note.render()));
        }
        out
    }
}

/// `┌─ TITLE ────────┐`, or the same without a title.
fn titled_border(
    theme: &Theme,
    inner: usize,
    left: char,
    right: char,
    title: Option<&str>,
) -> String {
    let glyphs = &theme.glyphs;
    let palette = &theme.palette;
    // Cells between the two corners: the panel's inner width plus the space
    // that sits either side of the content.
    let span = inner + 2;

    let (label, used) = match title.filter(|title| !title.is_empty()) {
        Some(title) => (Some(title), 1 + 1 + title.chars().count() + 1),
        None => (None, 0),
    };
    let fill = span.saturating_sub(used);

    let mut line = Text::of(left.to_string(), palette.frame);
    if let Some(title) = label {
        line = line
            .push(glyphs.horizontal.to_string(), palette.frame)
            .push(" ", palette.frame)
            .push(title.to_string(), palette.title)
            .push(" ", palette.frame);
    }
    line = line
        .push(glyphs.horizontal.to_string().repeat(fill), palette.frame)
        .push(right.to_string(), palette.frame);
    format!("{}\n", line.render())
}

/// `│ content… │`
fn content_line(theme: &Theme, text: &Text, inner: usize) -> String {
    let bar = Text::of(theme.glyphs.vertical.to_string(), theme.palette.frame).render();
    format!("{bar} {} {bar}\n", text.render_padded(inner))
}

fn pad(text: &str, width: usize) -> String {
    let len = text.chars().count();
    format!("{text}{}", " ".repeat(width.saturating_sub(len)))
}

#[cfg(test)]
mod tests {
    use anstyle::Style;
    use unicode_width::UnicodeWidthStr;

    use super::*;
    use crate::ui::table::{Align, Table};
    use crate::ui::theme::Skin;

    use crate::ui::text::strip_ansi as visible;

    fn frame_widths(rendered: &str) -> Vec<usize> {
        rendered
            .lines()
            .map(visible)
            .filter(|line| line.starts_with(['┌', '│', '├', '└']))
            .map(|line| UnicodeWidthStr::width(line.as_str()))
            .collect()
    }

    /// The frame is only ever as good as this property: whatever goes in, every
    /// line of the box has to come out the same width.
    fn assert_rectangular(panel: &Panel, theme: &Theme) {
        let rendered = panel.render(theme);
        let widths = frame_widths(&rendered);
        assert!(!widths.is_empty(), "nothing was framed:\n{rendered}");
        assert!(
            widths.windows(2).all(|pair| pair[0] == pair[1]),
            "ragged frame, widths {widths:?}:\n{rendered}"
        );
    }

    fn phosphor() -> Theme {
        Theme::with_skin(Skin::Phosphor, 80)
    }

    #[test]
    fn frames_stay_rectangular_with_styled_and_wide_content() {
        let mut table = Table::headerless([Align::Left, Align::Right]);
        table.push(vec![
            Text::of("世界", Style::new().bold()),
            Text::of("1.00000000", Style::new()),
        ]);
        table.push(vec![Text::raw("a"), Text::raw("22.00000000")]);

        let panel = Panel::new("TITLE")
            .row("label", Text::of("value", Style::new().bold()))
            .row("a much longer label", Text::raw("x"))
            .rule()
            .table(table)
            .section("SECTION")
            .line(Text::raw("plain"))
            .blank();
        assert_rectangular(&panel, &phosphor());
    }

    #[test]
    fn a_panel_shrinks_to_its_content_but_not_below_the_minimum() {
        let rendered = Panel::new("T").row("a", Text::raw("b")).render(&phosphor());
        assert_eq!(frame_widths(&rendered)[0], MIN_PANEL_WIDTH + 4);
    }

    #[test]
    fn a_panel_never_grows_past_the_theme_width() {
        let theme = phosphor();
        let rendered = Panel::new("T")
            .line(Text::raw("x".repeat(500)))
            .render(&theme);
        // The over-long line itself is left alone; the frame stops at the cap.
        assert_eq!(frame_widths(&rendered)[0], theme.width + 4);
    }

    #[test]
    fn a_long_title_widens_the_frame_rather_than_overflowing_it() {
        let panel = Panel::new("A TITLE CONSIDERABLY LONGER THAN ITS ONE SHORT ROW")
            .row("a", Text::raw("b"));
        assert_rectangular(&panel, &phosphor());
    }

    #[test]
    fn the_plain_skin_emits_no_escapes_and_no_box_drawing() {
        let theme = Theme::with_skin(Skin::Plain, 80);
        // Callers style from the palette, and the plain palette is empty — that
        // is what keeps piped output free of escapes.
        let rendered = Panel::new("TITLE")
            .row("label", Text::of("value", theme.palette.value))
            .render(&theme);
        assert!(!rendered.contains('\u{1b}'), "escapes leaked: {rendered:?}");
        assert!(!rendered.contains('│'), "box drawing leaked: {rendered:?}");
    }
}
