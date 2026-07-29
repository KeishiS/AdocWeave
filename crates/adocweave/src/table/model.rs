use crate::inline::Inline;
use crate::source::TextRange;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TableFormat {
    Psv,
    Csv,
    Dsv,
    Tsv,
}

impl TableFormat {
    pub const fn default_separator(self) -> char {
        match self {
            Self::Psv => '|',
            Self::Csv => ',',
            Self::Dsv => ':',
            Self::Tsv => '\t',
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TableSection {
    Header,
    Body,
    Footer,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HorizontalAlignment {
    Left,
    Center,
    Right,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum VerticalAlignment {
    Top,
    Middle,
    Bottom,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TableCellStyle {
    Default,
    AsciiDoc,
    Emphasis,
    Header,
    Literal,
    Monospace,
    Strong,
    Verse,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TableFrame {
    All,
    Ends,
    None,
    Sides,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TableGrid {
    All,
    Columns,
    None,
    Rows,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TableStripes {
    All,
    Even,
    Hover,
    None,
    Odd,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TablePresentation {
    pub frame: TableFrame,
    pub grid: TableGrid,
    pub stripes: TableStripes,
    pub width: Option<u8>,
    pub autowidth: bool,
}

impl Default for TablePresentation {
    fn default() -> Self {
        Self {
            frame: TableFrame::All,
            grid: TableGrid::All,
            stripes: TableStripes::None,
            width: None,
            autowidth: false,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TableCellContent {
    Inlines(Vec<Inline>),
    Verbatim(String),
    AsciiDoc(Vec<crate::parser::AstBlock>),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TableColumn {
    pub index: u32,
    pub width: Option<u32>,
    pub horizontal_alignment: HorizontalAlignment,
    pub vertical_alignment: VerticalAlignment,
    pub style: TableCellStyle,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TableCell {
    pub range: TextRange,
    pub marker_range: TextRange,
    pub content_range: TextRange,
    pub raw: String,
    pub column_index: u32,
    pub column_span: u32,
    pub row_span: u32,
    pub horizontal_alignment: Option<HorizontalAlignment>,
    pub vertical_alignment: Option<VerticalAlignment>,
    pub style: TableCellStyle,
    pub style_is_explicit: bool,
    pub content: TableCellContent,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TableRow {
    pub range: TextRange,
    pub section: TableSection,
    pub cells: Vec<TableCell>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Table {
    pub format: TableFormat,
    pub separator: char,
    pub content_range: TextRange,
    pub columns: Vec<TableColumn>,
    pub rows: Vec<TableRow>,
    pub presentation: TablePresentation,
    pub problems: Vec<TableProblem>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TableProblemKind {
    InvalidFormat,
    InvalidSeparator,
    UnclosedQuotedCell,
    InvalidPresentation,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TableProblem {
    pub kind: TableProblemKind,
    pub range: TextRange,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ScannedCell {
    pub range: TextRange,
    pub marker_range: TextRange,
    pub content_range: TextRange,
    pub raw: String,
    pub column_span: u32,
    pub row_span: u32,
    pub horizontal_alignment: Option<HorizontalAlignment>,
    pub vertical_alignment: Option<VerticalAlignment>,
    pub style: TableCellStyle,
    pub style_is_explicit: bool,
    pub duplication: u32,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ScannedTable {
    pub format: TableFormat,
    pub separator: char,
    pub content_range: TextRange,
    pub inferred_columns: usize,
    pub implicit_header_candidate: bool,
    pub cells: Vec<ScannedCell>,
    pub problems: Vec<TableProblem>,
}

impl ScannedTable {
    pub(crate) fn materialized_cell_count(&self) -> u64 {
        self.cells
            .iter()
            .map(|cell| u64::from(cell.duplication))
            .sum()
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ConfiguredCell {
    pub range: TextRange,
    pub marker_range: TextRange,
    pub content_range: TextRange,
    pub raw: String,
    pub column_span: u32,
    pub row_span: u32,
    pub horizontal_alignment: Option<HorizontalAlignment>,
    pub vertical_alignment: Option<VerticalAlignment>,
    pub style: TableCellStyle,
    pub style_is_explicit: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ConfiguredTable {
    pub format: TableFormat,
    pub separator: char,
    pub content_range: TextRange,
    pub columns: Vec<TableColumn>,
    pub cells: Vec<ConfiguredCell>,
    pub presentation: TablePresentation,
    pub problems: Vec<TableProblem>,
    pub header: bool,
    pub footer: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct LaidOutCell {
    pub cell: ConfiguredCell,
    pub column_index: u32,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct LaidOutRow {
    pub range: TextRange,
    pub section: TableSection,
    pub cells: Vec<LaidOutCell>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct LaidOutTable {
    pub format: TableFormat,
    pub separator: char,
    pub content_range: TextRange,
    pub columns: Vec<TableColumn>,
    pub rows: Vec<LaidOutRow>,
    pub presentation: TablePresentation,
    pub problems: Vec<TableProblem>,
}

impl LaidOutTable {
    pub(crate) fn lower_content<E>(
        self,
        mut lower: impl FnMut(&ConfiguredCell) -> Result<TableCellContent, E>,
    ) -> Result<Table, E> {
        let rows = self
            .rows
            .into_iter()
            .map(|row| {
                let cells = row
                    .cells
                    .into_iter()
                    .map(|laid_out| {
                        let content = lower(&laid_out.cell)?;
                        let cell = laid_out.cell;
                        Ok(TableCell {
                            range: cell.range,
                            marker_range: cell.marker_range,
                            content_range: cell.content_range,
                            raw: cell.raw,
                            column_index: laid_out.column_index,
                            column_span: cell.column_span,
                            row_span: cell.row_span,
                            horizontal_alignment: cell.horizontal_alignment,
                            vertical_alignment: cell.vertical_alignment,
                            style: cell.style,
                            style_is_explicit: cell.style_is_explicit,
                            content,
                        })
                    })
                    .collect::<Result<Vec<_>, E>>()?;
                Ok(TableRow {
                    range: row.range,
                    section: row.section,
                    cells,
                })
            })
            .collect::<Result<Vec<_>, E>>()?;
        Ok(Table {
            format: self.format,
            separator: self.separator,
            content_range: self.content_range,
            columns: self.columns,
            rows,
            presentation: self.presentation,
            problems: self.problems,
        })
    }
}
