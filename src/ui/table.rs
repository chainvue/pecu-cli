//! Column-aligned rows, for the places where a key/value pair is not enough:
//! token balances, unspent outputs, decoded transaction outputs.
//!
//! Headers are optional. A headerless table is how the aligned money block
//! inside a panel is drawn — the columns line up, nothing announces itself.

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
}

impl Table {
    pub fn new(columns: Vec<Column>) -> Self {
        Self {
            columns,
            rows: Vec::new(),
        }
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
                    column.header.chars().count()
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
    pub fn lines(&self, theme: &Theme) -> Vec<Text> {
        let widths = self.widths();
        let mut lines = Vec::with_capacity(self.rows.len() + 1);

        if self.has_headers() {
            let header_cells: Vec<Text> = self
                .columns
                .iter()
                .map(|column| Text::of(column.header.to_uppercase(), theme.palette.label))
                .collect();
            lines.push(self.lay_out(&header_cells, &widths));
        }

        for row in &self.rows {
            lines.push(self.lay_out(row, &widths));
        }
        lines
    }

    fn lay_out(&self, cells: &[Text], widths: &[usize]) -> Text {
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
            let cell = cells.get(index).unwrap_or(&empty);
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
