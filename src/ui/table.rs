//! Column-aligned rows, for the places where a key/value pair is not enough:
//! token balances, unspent outputs, decoded transaction outputs.
//!
//! Headers are optional. A headerless table is how the aligned money block
//! inside a panel is drawn — the columns line up, nothing announces itself.

use unicode_width::UnicodeWidthStr;

use crate::ui::text::Text;
use crate::ui::theme::Theme;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Align {
    Left,
    Right,
}

#[derive(Debug, Clone)]
pub struct Column {
    pub header: String,
    pub align: Align,
}

impl Column {
    pub fn left(header: impl Into<String>) -> Self {
        Self {
            header: header.into(),
            align: Align::Left,
        }
    }

    pub fn right(header: impl Into<String>) -> Self {
        Self {
            header: header.into(),
            align: Align::Right,
        }
    }
}

/// Two spaces between columns. Enough to read as a gap, tight enough that a
/// wide table still fits.
const GUTTER: usize = 2;

#[derive(Debug, Clone)]
pub struct Table {
    columns: Vec<Column>,
    rows: Vec<Vec<Text>>,
    /// Columns whose cells may be shortened to keep the table inside its frame,
    /// the one that pays first at the front, each with the floor the caller
    /// named for it. See [`Table::elidable`].
    elidable: Vec<(usize, Option<usize>)>,
}

impl Table {
    pub fn new(columns: Vec<Column>) -> Self {
        Self {
            columns,
            rows: Vec::new(),
            elidable: Vec::new(),
        }
    }

    /// Name a column whose cells may be shortened when the table will not fit
    /// its frame, and say where in the queue it is: the first call names the
    /// column that pays first.
    ///
    /// Nothing is elided unless the table is over budget, so a column marked
    /// here keeps whole, copyable ids at every width that has room for them.
    /// Only mark a column whose cells survive being cut from the middle — an
    /// id or a hash, which stays recognisable and stays visibly incomplete.
    /// A count, an amount or a timestamp does not: `(215 outputs)` cut to
    /// `(2…ts)` has lost the number and kept the noise, and a column of those
    /// is better left pushing the frame out of square, where it reads as the
    /// bug it is.
    #[must_use]
    pub fn elidable(mut self, column: usize) -> Self {
        self.debug_assert_exists(column);
        self.elidable.push((column, None));
        self
    }

    /// The same, but the column stops at `floor` cells rather than at the
    /// default — its own header, or where [`Text::fit`] itself refuses.
    ///
    /// For a column whose cells stop being worth anything some way *above* that
    /// default. A currency id is the case this exists for: `fmt::address`
    /// already states the shortest form that can still be copied, pasted or
    /// looked up, so a table has no business cutting one below it. Under a floor
    /// it cannot meet, [`Table::fitted_lines`]' all-or-nothing rule takes over
    /// and the table comes back whole and ragged — which is the trade this is
    /// asking for. Removing data from a wallet table is worse than a cosmetic
    /// ragged frame.
    ///
    /// A floor below the default is ignored: the header still may not be cut.
    #[must_use]
    pub fn elidable_to(mut self, column: usize, floor: usize) -> Self {
        self.debug_assert_exists(column);
        self.elidable.push((column, Some(floor)));
        self
    }

    fn debug_assert_exists(&self, column: usize) {
        debug_assert!(
            column < self.columns.len(),
            "column {column} does not exist in a table of {}",
            self.columns.len()
        );
    }

    fn is_elidable(&self, column: usize) -> bool {
        self.elidable.iter().any(|(index, _)| *index == column)
    }

    /// A table whose columns are only there to align things.
    pub fn headerless(aligns: impl IntoIterator<Item = Align>) -> Self {
        Self::new(
            aligns
                .into_iter()
                .map(|align| Column {
                    header: String::new(),
                    align,
                })
                .collect(),
        )
    }

    /// Add a row. Short rows are padded with empty cells; long rows would
    /// misalign the table, so they are an assertion rather than a silent trim.
    pub fn push(&mut self, cells: Vec<Text>) {
        debug_assert!(
            cells.len() <= self.columns.len(),
            "row has {} cells but the table has {} columns",
            cells.len(),
            self.columns.len()
        );
        self.rows.push(cells);
    }

    fn has_headers(&self) -> bool {
        self.columns.iter().any(|column| !column.header.is_empty())
    }

    fn widths(&self) -> Vec<usize> {
        self.columns
            .iter()
            .enumerate()
            .map(|(index, column)| {
                let header = if self.has_headers() {
                    header_width(column)
                } else {
                    0
                };
                self.rows
                    .iter()
                    .filter_map(|row| row.get(index))
                    .map(Text::width)
                    .chain(std::iter::once(header))
                    .max()
                    .unwrap_or(0)
            })
            .collect()
    }

    /// Render to one [`Text`] per line, header included when there is one.
    ///
    /// Unbudgeted: every column is as wide as its widest cell. This is what the
    /// plain skin prints — there is no frame to break there, and piped output is
    /// likelier to be fed to something than read — and what a caller measures
    /// when it wants to know how wide the table wants to be.
    pub fn lines(&self, theme: &Theme) -> Vec<Text> {
        self.render(theme, &self.widths())
    }

    /// The same, shortened to `budget` cells if that is what it takes.
    ///
    /// The frame pads a content line without cutting it, so a table wider than
    /// the panel runs out through the right-hand border and the box comes out
    /// ragged. Only the caller knows which columns may be shortened, so it says
    /// so with [`Table::elidable`]; a table that has named none is returned
    /// whole and ragged, deliberately.
    ///
    /// How much a narrower column actually saves is measured, not predicted.
    /// This is the lesson a width constant here learned the hard way: a column
    /// is as wide as the widest cell *across* rows, not all rows reach the last
    /// column, and the last column is not padded — so the only honest test is to
    /// render the whole table and look at what came out. Hence the loop: shrink
    /// by the shortfall, re-render, measure again, and stop when it fits or when
    /// there is nothing left that may be shortened.
    ///
    /// All or nothing. If even the narrowest the elidable columns will go is
    /// still too wide — a four-column table in a fifty-column split, where the
    /// three columns that may not be touched have already spent the budget —
    /// the table comes back whole. A shortening that does not square the frame
    /// has cut an id for nothing: the frame is ragged either way, and the
    /// ragged frame with the id still on it is the better bug report.
    pub fn fitted_lines(&self, theme: &Theme, budget: usize) -> Vec<Text> {
        let natural = self.widths();
        let lines = self.render(theme, &natural);
        if widest(&lines) <= budget {
            return lines;
        }

        let mut widths = natural;
        let mut shortened = lines.clone();
        while let Some(over) = widest(&shortened)
            .checked_sub(budget)
            .filter(|over| *over > 0)
        {
            if !self.shrink(&mut widths, over, theme) {
                return lines;
            }
            shortened = self.render(theme, &widths);
        }
        shortened
    }

    /// Whether fitting this table into `budget` would take anything off a cell.
    ///
    /// For the caller whose table holds something a reader is meant to copy. A
    /// shortened cell is no longer the string it names, and a panel that prints
    /// one without saying so has handed over a value that looks whole and is
    /// not — which on a column of names is the difference between a list you
    /// can act on and one you cannot.
    ///
    /// Measured by rendering both ways rather than predicted from the widths,
    /// for the same reason [`Table::fitted_lines`] measures: the saving a
    /// narrower column makes is not arithmetic on the column, and the
    /// all-or-nothing rule means a table over budget is not always a table that
    /// gets cut. Only a difference in what comes out counts.
    #[must_use]
    pub fn shortens_at(&self, theme: &Theme, budget: usize) -> bool {
        let natural = self.lines(theme);
        let fitted = self.fitted_lines(theme, budget);
        natural.len() != fitted.len()
            || natural
                .iter()
                .zip(&fitted)
                .any(|(whole, cut)| whole.render() != cut.render())
    }

    fn render(&self, theme: &Theme, widths: &[usize]) -> Vec<Text> {
        let mut lines = Vec::with_capacity(self.rows.len() + 1);

        if self.has_headers() {
            let header_cells: Vec<Text> = self
                .columns
                .iter()
                .map(|column| Text::of(column.header.to_uppercase(), theme.palette.label))
                .collect();
            lines.push(self.lay_out(&header_cells, widths, theme));
        }

        for row in &self.rows {
            lines.push(self.lay_out(row, widths, theme));
        }
        lines
    }

    /// Take `deficit` cells off the elidable columns, in the order the caller
    /// queued them. Returns whether anything was given up at all — `false` is
    /// how [`Table::fitted_lines`] learns to stop trying.
    ///
    /// A column stops at its own header. An elided header is not a shorter
    /// table, it is an unreadable one: the header is the only thing that says
    /// what the column beneath it holds, and it is not data anybody came here
    /// to read. A headerless column stops where [`Text::fit`] itself refuses,
    /// which is where the result would be more ellipsis than text — or higher
    /// still, where the caller named a floor with [`Table::elidable_to`].
    fn shrink(&self, widths: &mut [usize], deficit: usize, theme: &Theme) -> bool {
        let marker = UnicodeWidthStr::width(theme.glyphs.ellipsis);
        let mut left = deficit;
        let mut gave = false;

        for &(column, named) in &self.elidable {
            if left == 0 {
                break;
            }
            let default = if self.has_headers() {
                header_width(&self.columns[column])
            } else {
                marker + 3
            };
            // The named floor may only be more conservative than the default.
            // Below it the header goes, and an elided header is not a shorter
            // table but an unreadable one.
            let floor = named.unwrap_or(0).max(default);
            let give = widths[column].saturating_sub(floor).min(left);
            if give > 0 {
                widths[column] -= give;
                left -= give;
                gave = true;
            }
        }
        gave
    }

    fn lay_out(&self, cells: &[Text], widths: &[usize], theme: &Theme) -> Text {
        let empty = Text::new();
        // The last column that has anything in it, rather than the last column
        // that exists. A row which leaves the final cells blank — the totals
        // rows in `wallet balance`, where only some lines carry a note — would
        // otherwise end in the gutter leading up to them, and pay for a column
        // it does not use with trailing whitespace.
        let Some(last) = self
            .columns
            .iter()
            .enumerate()
            .rposition(|(index, _)| cells.get(index).is_some_and(|cell| cell.width() > 0))
        else {
            // A blank row is a blank line, not a line of padding. Falling
            // through would emit the right-aligned columns' padding.
            return Text::preformatted(String::new(), 0);
        };
        let mut line = String::new();
        let mut width = 0;

        for (index, column) in self.columns.iter().enumerate().take(last + 1) {
            if index > 0 {
                line.push_str(&" ".repeat(GUTTER));
                width += GUTTER;
            }
            // A column that was never narrowed is at least as wide as every
            // cell in it, so this is a no-op until `shrink` has taken something
            // off — which is what keeps an id whole wherever there is room.
            let source = cells.get(index).unwrap_or(&empty);
            let shortened;
            let cell = if self.is_elidable(index) {
                shortened = source.fit(widths[index], theme.glyphs.ellipsis);
                &shortened
            } else {
                source
            };
            // Trailing spaces on the last column would only push the panel's
            // right-hand frame around, so it is left ragged.
            let padded = column.align == Align::Right || index != last;
            line.push_str(&match (padded, column.align) {
                (false, _) => cell.render(),
                (true, Align::Left) => cell.render_padded(widths[index]),
                (true, Align::Right) => cell.render_right(widths[index]),
            });
            width += if padded {
                widths[index].max(cell.width())
            } else {
                cell.width()
            };
        }

        // The cells already carry their escapes; re-wrapping the assembled line
        // as a plain span would measure those escapes as visible characters.
        Text::preformatted(line, width)
    }
}

/// The widest line in a rendered table, in display cells.
fn widest(lines: &[Text]) -> usize {
    lines.iter().map(Text::width).max().unwrap_or(0)
}

/// The header as [`Table::render`] actually draws it: upper-cased, and measured
/// in display cells rather than `char`s.
///
/// Both readings agree for the ASCII headers in the tree today. They stop
/// agreeing the moment one does not, and the two places that ask — the width a
/// column is sized to, and the floor [`Table::shrink`] will not cut past — are
/// both promises about what reaches the terminal, so both have to be counted
/// the way the terminal counts.
fn header_width(column: &Column) -> usize {
    UnicodeWidthStr::width(column.header.to_uppercase().as_str())
}

#[cfg(test)]
mod tests {
    use anstyle::Style;

    use super::*;
    use crate::ui::text::strip_ansi;
    use crate::ui::theme::{Skin, Theme};

    const ADDRESS: &str = "RQC1EG3GhZ9pvT9YgCp3YvxyYBsdb4FYfH";

    fn phosphor(terminal: usize) -> Theme {
        Theme::with_skin(Skin::Phosphor, terminal)
    }

    /// A `key list` table: a user-chosen label, a whole address, an age.
    ///
    /// Queued the way `key_table` queues it — the display text first, the
    /// identifier second — so what these tests pin is the shape that ships.
    fn key_list(label: &str) -> Table {
        let mut table = Table::new(vec![
            Column::left("label"),
            Column::left("address"),
            Column::right("created"),
        ])
        .elidable(0)
        .elidable(1);
        table.push(vec![
            Text::of(label, Style::new().bold()),
            Text::raw(ADDRESS),
            Text::raw("3h 04m ago"),
        ]);
        table
    }

    fn rendered(table: &Table, theme: &Theme, budget: usize) -> Vec<String> {
        table
            .fitted_lines(theme, budget)
            .iter()
            .map(|line| strip_ansi(&line.render()))
            .collect()
    }

    #[test]
    fn a_table_the_frame_has_room_for_is_not_shortened_at_all() {
        // A table that fits is returned untouched — the branch where nothing is
        // over budget in the first place.
        let theme = phosphor(120);
        let lines = rendered(&key_list("demo"), &theme, theme.width);
        assert!(
            lines.iter().any(|line| line.contains(ADDRESS)),
            "the address was shortened with room to spare: {lines:?}"
        );
    }

    #[test]
    fn an_id_stays_whole_while_the_text_beside_it_still_has_room_to_give() {
        // The rule the fix must not break, on the branch that actually elides.
        // A 35-character label puts this table five cells over the widest frame
        // the theme can reach, so something must give — and the something is
        // the label, which is user-chosen and readable back from `--json`, not
        // the address, which is the only identifier on the line.
        let theme = phosphor(120);
        let label = "l".repeat(35);
        let lines = rendered(&key_list(&label), &theme, theme.width);
        assert!(
            !lines[1].contains(&label),
            "the label should have paid: {lines:?}"
        );
        assert!(
            lines[1].contains(ADDRESS),
            "the address was cut while the label still had cells to spare: {lines:?}"
        );
    }

    #[test]
    fn one_long_label_does_not_cost_the_other_rows_their_addresses() {
        // Column widths are the maximum across rows, so a badly-behaved row can
        // reach the well-behaved ones. Queued id-first it did: one 64-character
        // label used to cut `alice`'s address to the width of the word ADDRESS,
        // at every width the theme can reach.
        let theme = phosphor(120);
        let mut table = Table::new(vec![
            Column::left("label"),
            Column::left("address"),
            Column::right("created"),
        ])
        .elidable(0)
        .elidable(1);
        table.push(vec![
            Text::raw("alice"),
            Text::raw(ADDRESS),
            Text::raw("3h 04m ago"),
        ]);
        table.push(vec![
            Text::raw("l".repeat(64)),
            Text::raw("RWpmUu8uEcbgyrgqVHqXMbckR5g11HsvaD"),
            Text::raw("9d 11h ago"),
        ]);
        let lines = rendered(&table, &theme, theme.width);
        assert!(
            lines[1].contains(ADDRESS),
            "a neighbour's long label cut alice's address: {lines:?}"
        );
    }

    #[test]
    fn every_line_fits_at_every_width_the_theme_can_reach() {
        // `Theme::with_skin` clamps to 48..=78, so these are all of them, and
        // the label is the longest the keystore will accept.
        let long = "l".repeat(64);
        for label in ["a", "demo", "a-considerably-longer-label", &long] {
            for width in 48..=78 {
                let theme = phosphor(width + 4);
                assert_eq!(theme.width, width, "the clamp moved");
                for line in key_list(label).fitted_lines(&theme, width) {
                    assert!(
                        line.width() <= width,
                        "{} cells against a budget of {width}, label {} chars: {:?}",
                        line.width(),
                        label.chars().count(),
                        strip_ansi(&line.render())
                    );
                }
            }
        }
    }

    #[test]
    fn the_column_that_was_queued_first_is_the_one_that_pays() {
        // Nine cells short. The label gives them up and the address — the one
        // thing on the line a reader is here to copy — is untouched.
        let theme = phosphor(80);
        let label = "a-considerably-longer-label";
        let lines = rendered(&key_list(label), &theme, 66);
        assert!(
            !lines[1].contains(label) && lines[1].contains('\u{2026}'),
            "the label should have been cut: {lines:?}"
        );
        assert!(
            lines[1].contains(ADDRESS),
            "the address paid first: {lines:?}"
        );
    }

    #[test]
    fn a_column_stops_at_its_own_header_before_anything_else_is_touched() {
        // Sixty-four cells short of a fifty-column split: more than the label
        // alone can find. It goes down to exactly `LABEL` and stops — below
        // that an elided header would be the only thing saying what the column
        // holds — and the address, queued second, pays the small remainder
        // rather than the frame.
        let theme = phosphor(80);
        let lines = rendered(&key_list(&"l".repeat(64)), &theme, 48);
        assert!(lines[0].contains("ADDRESS"), "header was cut: {lines:?}");
        assert!(lines[0].contains("LABEL"), "header was cut: {lines:?}");
        assert!(!lines[1].contains(ADDRESS), "{lines:?}");
        assert!(lines[1].contains('\u{2026}'), "{lines:?}");
    }

    #[test]
    fn the_id_gives_up_no_more_than_the_shortfall_the_text_could_not_cover() {
        // The queue is a priority order, not a sacrifice: once the label is at
        // its floor the address does pay, but only the remainder. Twenty-nine
        // of its thirty-four characters survive the narrowest frame the theme
        // can reach — where queued the other way round it was seven.
        let theme = phosphor(52);
        assert_eq!(theme.width, 48, "the clamp moved");
        let lines = rendered(&key_list(&"l".repeat(64)), &theme, theme.width);
        let shown: usize = lines[1]
            .split_whitespace()
            .find(|word| word.starts_with('R'))
            .expect("an address cell")
            .chars()
            .count();
        assert!(
            shown >= 20,
            "the address was cut to {shown} characters: {lines:?}"
        );
    }

    #[test]
    fn no_column_is_ever_dropped_to_make_room() {
        // Removing data from a wallet table is worse than a ragged frame, so
        // the last column is still there at the narrowest width there is.
        let theme = phosphor(52);
        let lines = rendered(&key_list(&"l".repeat(64)), &theme, theme.width);
        assert!(lines[0].contains("CREATED"), "{lines:?}");
        assert!(lines[1].contains("3h 04m ago"), "{lines:?}");
    }

    #[test]
    fn a_table_that_named_no_elidable_column_is_left_whole_rather_than_cut() {
        // The boundary, stated as a test. Nothing here survives being cut from
        // the middle — an amount cut in half is a different number — so the
        // table comes out over budget and the frame comes out ragged, which is
        // a legible bug rather than a quiet lie.
        let theme = phosphor(52);
        let mut table = Table::headerless([Align::Left, Align::Right, Align::Left]);
        table.push(vec![
            Text::raw("IN CONDITIONS"),
            Text::raw("12,345,678.00000000"),
            Text::raw("VRSCTEST  (215 outputs)"),
        ]);
        let lines = rendered(&table, &theme, theme.width);
        assert!(lines[0].contains("12,345,678.00000000"), "{lines:?}");
        assert!(lines[0].contains("(215 outputs)"), "{lines:?}");
    }

    #[test]
    fn a_headerless_column_shortens_down_to_where_fitting_itself_refuses() {
        // No header to stop at, so the floor is the point below which a cut is
        // more ellipsis than text.
        let theme = phosphor(80);
        let mut table = Table::headerless([Align::Right, Align::Left, Align::Left]).elidable(2);
        table.push(vec![
            Text::raw("9,272.49511041"),
            Text::raw("a-name-of-twentyfour-chr@"),
            Text::raw("iHBwQo7LUmrTFsc9Kz7RTs2WYNXR44dK9f"),
        ]);
        let lines = rendered(&table, &theme, 48);
        assert!(lines[0].contains("9,272.49511041"), "{lines:?}");
        assert!(lines[0].contains("a-name-of-twentyfour-chr@"), "{lines:?}");
        assert!(lines[0].contains('\u{2026}'), "{lines:?}");
    }

    #[test]
    fn a_named_floor_stops_a_column_above_where_fitting_would_have() {
        // The same table as above, whose id column `Text::fit` would take down
        // to `i…dK9f`. Told where the id stops being a handle, the column
        // refuses — and since it was the only one that could give, the
        // all-or-nothing rule returns the table whole and the frame goes
        // ragged, which is the trade `elidable_to` is asking for.
        let theme = phosphor(80);
        let mut table = Table::headerless([Align::Right, Align::Left, Align::Left])
            .elidable_to(2, "iHBwQo7LU…dK9f".chars().count());
        table.push(vec![
            Text::raw("9,272.49511041"),
            Text::raw("a-name-of-twentyfour-chr@"),
            Text::raw("iHBwQo7LUmrTFsc9Kz7RTs2WYNXR44dK9f"),
        ]);
        let lines = rendered(&table, &theme, 48);
        assert!(
            lines[0].contains("iHBwQo7LUmrTFsc9Kz7RTs2WYNXR44dK9f"),
            "the id was cut past its floor: {lines:?}"
        );
    }

    #[test]
    fn a_named_floor_below_the_header_does_not_get_to_cut_the_header() {
        // The floor may only be more conservative than the default. A caller
        // naming one under the header does not thereby earn the right to elide
        // it — the header is the only thing saying what the column holds.
        let theme = phosphor(80);
        let mut table =
            Table::new(vec![Column::left("outpoint"), Column::left("status")]).elidable_to(0, 1);
        table.push(vec![
            Text::raw("9f2c1ab4de77605318bbcafe0021d4e9c7b3:0"),
            Text::raw("spendable"),
        ]);
        let lines = rendered(&table, &theme, 24);
        assert!(lines[0].contains("OUTPOINT"), "header was cut: {lines:?}");
    }

    #[test]
    fn a_row_that_stops_short_of_the_last_column_is_measured_as_it_prints() {
        // The trap a width constant walks into: rows do not all reach the last
        // column and the last column is not padded, so what a narrower column
        // saves is what came out, not what was predicted.
        let theme = phosphor(80);
        let mut table = Table::headerless([Align::Left, Align::Right, Align::Left]).elidable(2);
        table.push(vec![Text::raw("NET"), Text::raw("-0.75010000")]);
        table.push(vec![
            Text::raw(""),
            Text::raw("+9,272.49511041"),
            Text::raw("iHBwQo7LUmrTFsc9Kz7RTs2WYNXR44dK9f"),
        ]);
        for line in table.fitted_lines(&theme, 40) {
            assert!(line.width() <= 40, "{} cells", line.width());
        }
    }
}
