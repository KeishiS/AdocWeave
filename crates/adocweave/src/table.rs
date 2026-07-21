//! Typed PSV table model and a lossless, I/O-free cell scanner.

use crate::inline::Inline;
use crate::source::{TextRange, TextSize};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TableFormat {
    Psv,
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

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TableCellContent {
    Inlines(Vec<Inline>),
    Verbatim(String),
    AsciiDoc(Vec<Inline>),
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
    pub content_range: TextRange,
    pub columns: Vec<TableColumn>,
    pub rows: Vec<TableRow>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct RawCell {
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
pub(crate) struct RawTable {
    pub content_range: TextRange,
    pub inferred_columns: usize,
    pub cells: Vec<RawCell>,
}

pub(crate) fn scan_psv(value: &str, range: TextRange) -> RawTable {
    let mut cells = Vec::<RawCell>::new();
    let mut offset = 0;
    let mut inferred_columns = 0;
    for line_with_ending in value.split_inclusive('\n') {
        let line = line_with_ending
            .strip_suffix('\n')
            .unwrap_or(line_with_ending);
        let line = line.strip_suffix('\r').unwrap_or(line);
        let markers = marker_positions(line);
        inferred_columns = inferred_columns.max(markers.len());
        if markers.is_empty() {
            if let Some(previous) = cells.last_mut() {
                previous.raw.push('\n');
                previous.raw.push_str(line);
                previous.content_range = absolute_range(
                    range,
                    previous.content_range.start().to_usize() - range.start().to_usize(),
                    offset + line.len(),
                );
                previous.range =
                    TextRange::new(previous.range.start(), previous.content_range.end())
                        .expect("continued cell range is ordered");
            }
            offset += line_with_ending.len();
            continue;
        }
        for (index, (marker_start, pipe)) in markers.iter().copied().enumerate() {
            let end = markers.get(index + 1).map_or(line.len(), |next| next.0);
            let content_start = pipe + 1;
            let raw_end = line[..end].trim_end_matches([' ', '\t']).len();
            let raw = &line[content_start.min(raw_end)..raw_end];
            let spec = parse_cell_spec(&line[marker_start..pipe]);
            let marker_range = absolute_range(range, offset + marker_start, offset + pipe + 1);
            let content_range = absolute_range(range, offset + content_start, offset + raw_end);
            cells.push(RawCell {
                range: TextRange::new(marker_range.start(), content_range.end())
                    .expect("cell range is ordered"),
                marker_range,
                content_range,
                raw: raw.to_owned(),
                column_span: spec.column_span,
                row_span: spec.row_span,
                horizontal_alignment: spec.horizontal_alignment,
                vertical_alignment: spec.vertical_alignment,
                style: spec.style,
                style_is_explicit: spec.style_is_explicit,
            });
        }
        offset += line_with_ending.len();
    }
    for cell in &mut cells {
        let leading = cell.raw.len() - cell.raw.trim_start_matches([' ', '\t', '\r', '\n']).len();
        let trailing = cell.raw.len() - cell.raw.trim_end_matches([' ', '\t', '\r', '\n']).len();
        let end = cell.content_range.end().to_usize().saturating_sub(trailing);
        let start = cell.content_range.start().to_usize() + leading;
        cell.content_range = TextRange::new(
            TextSize::new(start).expect("trimmed table offset is bounded"),
            TextSize::new(end.max(start)).expect("trimmed table offset is bounded"),
        )
        .expect("trimmed table range is ordered");
        cell.range = TextRange::new(cell.marker_range.start(), cell.content_range.end())
            .expect("trimmed cell range is ordered");
        cell.raw = cell.raw.trim_matches([' ', '\t', '\r', '\n']).to_owned();
    }
    RawTable {
        content_range: range,
        inferred_columns: inferred_columns.max(1),
        cells,
    }
}

fn marker_positions(line: &str) -> Vec<(usize, usize)> {
    line.char_indices()
        .filter_map(|(pipe, character)| {
            if character != '|' {
                return None;
            }
            if line[..pipe]
                .bytes()
                .rev()
                .take_while(|byte| *byte == b'\\')
                .count()
                % 2
                == 1
            {
                return None;
            }
            let prefix_start = cell_spec_start(&line[..pipe]);
            let boundary =
                prefix_start == 0 || line.as_bytes()[prefix_start - 1].is_ascii_whitespace();
            boundary.then_some((prefix_start, pipe))
        })
        .collect()
}

fn cell_spec_start(prefix: &str) -> usize {
    let mut start = prefix.len();
    for (offset, character) in prefix.char_indices().rev() {
        if character.is_ascii_digit()
            || matches!(
                character,
                '.' | '+' | '<' | '>' | '^' | 'a' | 'd' | 'e' | 'h' | 'l' | 'm' | 's' | 'v'
            )
        {
            start = offset;
        } else {
            break;
        }
    }
    start
}

#[derive(Clone, Copy)]
struct CellSpec {
    column_span: u32,
    row_span: u32,
    horizontal_alignment: Option<HorizontalAlignment>,
    vertical_alignment: Option<VerticalAlignment>,
    style: TableCellStyle,
    style_is_explicit: bool,
}

fn parse_cell_spec(value: &str) -> CellSpec {
    let explicit_style = value.chars().next_back().and_then(style);
    let style = explicit_style.unwrap_or(TableCellStyle::Default);
    let horizontal_alignment = value.chars().find_map(|character| match character {
        '<' => Some(HorizontalAlignment::Left),
        '^' => Some(HorizontalAlignment::Center),
        '>' => Some(HorizontalAlignment::Right),
        _ => None,
    });
    let vertical_alignment = value.rsplit_once('.').and_then(|(_, right)| {
        right.chars().find_map(|character| match character {
            '<' => Some(VerticalAlignment::Top),
            '^' => Some(VerticalAlignment::Middle),
            '>' => Some(VerticalAlignment::Bottom),
            _ => None,
        })
    });
    let span = value.split_once('+').map_or("", |(span, _)| span);
    let (column_span, row_span) = span.split_once('.').map_or_else(
        || (span.parse().unwrap_or(1), 1),
        |(columns, rows)| (columns.parse().unwrap_or(1), rows.parse().unwrap_or(1)),
    );
    CellSpec {
        column_span: column_span.max(1),
        row_span: row_span.max(1),
        horizontal_alignment,
        vertical_alignment,
        style,
        style_is_explicit: explicit_style.is_some(),
    }
}

pub(crate) fn style(character: char) -> Option<TableCellStyle> {
    match character {
        'a' => Some(TableCellStyle::AsciiDoc),
        'd' => Some(TableCellStyle::Default),
        'e' => Some(TableCellStyle::Emphasis),
        'h' => Some(TableCellStyle::Header),
        'l' => Some(TableCellStyle::Literal),
        'm' => Some(TableCellStyle::Monospace),
        's' => Some(TableCellStyle::Strong),
        'v' => Some(TableCellStyle::Verse),
        _ => None,
    }
}

fn absolute_range(parent: TextRange, start: usize, end: usize) -> TextRange {
    TextRange::new(
        TextSize::new(parent.start().to_usize() + start).expect("table offset is bounded"),
        TextSize::new(parent.start().to_usize() + end).expect("table offset is bounded"),
    )
    .expect("table range is ordered")
}

pub(crate) fn configure(table: &mut Table, metadata: &crate::parser::BlockMetadata) {
    let cols = metadata
        .attributes
        .iter()
        .rev()
        .find(|attribute| attribute.name.as_deref() == Some("cols"))
        .map(|attribute| attribute.value.trim_matches('"'));
    if let Some(cols) = cols {
        let parsed = cols
            .split(',')
            .filter(|value| !value.trim().is_empty())
            .flat_map(|value| expand_column(value.trim()))
            .enumerate()
            .map(|(index, mut column)| {
                column.index = index as u32;
                column
            })
            .collect::<Vec<_>>();
        if !parsed.is_empty() {
            table.columns = parsed;
            layout_rows(table);
        }
    }
    apply_column_defaults(table);
    let has_option = |name: &str| {
        metadata.options.iter().any(|option| option.value == name)
            || metadata.attributes.iter().any(|attribute| {
                attribute.name.as_deref() == Some("options")
                    && attribute
                        .value
                        .trim_matches('"')
                        .split(',')
                        .any(|option| option.trim() == name)
            })
    };
    if has_option("header") {
        if let Some(row) = table.rows.first_mut() {
            row.section = TableSection::Header;
        }
    }
    if has_option("footer") {
        if let Some(row) = table.rows.last_mut() {
            row.section = TableSection::Footer;
        }
    }
}

fn expand_column(value: &str) -> Vec<TableColumn> {
    let (count, spec) = value
        .split_once('*')
        .and_then(|(count, spec)| count.parse::<usize>().ok().map(|count| (count, spec)))
        .unwrap_or((1, value));
    let column = parse_column(spec);
    vec![column; count.max(1)]
}

fn parse_column(value: &str) -> TableColumn {
    let width = value
        .bytes()
        .filter(u8::is_ascii_digit)
        .fold(None, |current, byte| {
            Some(current.unwrap_or(0_u32) * 10 + u32::from(byte - b'0'))
        });
    let horizontal_alignment = value.chars().find_map(|character| match character {
        '<' => Some(HorizontalAlignment::Left),
        '^' => Some(HorizontalAlignment::Center),
        '>' => Some(HorizontalAlignment::Right),
        _ => None,
    });
    TableColumn {
        index: 0,
        width,
        horizontal_alignment: horizontal_alignment.unwrap_or(HorizontalAlignment::Left),
        vertical_alignment: value
            .rsplit_once('.')
            .and_then(|(_, suffix)| {
                suffix.chars().find_map(|character| match character {
                    '<' => Some(VerticalAlignment::Top),
                    '^' => Some(VerticalAlignment::Middle),
                    '>' => Some(VerticalAlignment::Bottom),
                    _ => None,
                })
            })
            .unwrap_or(VerticalAlignment::Top),
        style: value
            .chars()
            .next_back()
            .and_then(style)
            .unwrap_or(TableCellStyle::Default),
    }
}

fn apply_column_defaults(table: &mut Table) {
    for row in &mut table.rows {
        for cell in &mut row.cells {
            let Some(column) = table.columns.get(cell.column_index as usize) else {
                continue;
            };
            if !cell.style_is_explicit {
                cell.style = column.style;
                cell.content = match column.style {
                    TableCellStyle::Literal | TableCellStyle::Verse => {
                        TableCellContent::Verbatim(cell.raw.clone())
                    }
                    TableCellStyle::AsciiDoc => match std::mem::replace(
                        &mut cell.content,
                        TableCellContent::Inlines(Vec::new()),
                    ) {
                        TableCellContent::Inlines(inlines)
                        | TableCellContent::AsciiDoc(inlines) => {
                            TableCellContent::AsciiDoc(inlines)
                        }
                        content @ TableCellContent::Verbatim(_) => content,
                    },
                    _ => {
                        std::mem::replace(&mut cell.content, TableCellContent::Inlines(Vec::new()))
                    }
                };
            }
        }
    }
}

pub(crate) fn layout_rows(table: &mut Table) {
    let column_count = table.columns.len();
    if column_count == 0 {
        return;
    }
    let cells = table
        .rows
        .drain(..)
        .flat_map(|row| row.cells)
        .collect::<Vec<_>>();
    let mut pending = vec![0_u32; column_count];
    let mut rows = Vec::new();
    let mut input = cells.into_iter().peekable();
    while input.peek().is_some() {
        for remaining in &mut pending {
            *remaining = remaining.saturating_sub(1);
        }
        let mut row = Vec::new();
        let mut column = 0;
        while column < column_count {
            while column < column_count && pending[column] > 0 {
                column += 1;
            }
            let Some(next) = input.peek() else { break };
            let span = next.column_span as usize;
            if column + span > column_count
                || pending[column..column + span]
                    .iter()
                    .any(|remaining| *remaining > 0)
            {
                break;
            }
            let mut cell = input.next().expect("peeked table cell exists");
            cell.column_index = column as u32;
            if cell.row_span > 1 {
                for remaining in &mut pending[column..column + span] {
                    *remaining = (*remaining).max(cell.row_span);
                }
            }
            column += span;
            row.push(cell);
        }
        if row.is_empty() {
            pending.fill(0);
            continue;
        }
        rows.push(TableRow {
            range: TextRange::new(
                row.first().expect("non-empty row").range.start(),
                row.last().expect("non-empty row").range.end(),
            )
            .expect("table row range is ordered"),
            section: TableSection::Body,
            cells: row,
        });
    }
    table.rows = rows;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn range(value: &str) -> TextRange {
        TextRange::new(
            TextSize::new(0).expect("start"),
            TextSize::new(value.len()).expect("end"),
        )
        .expect("range")
    }

    #[test]
    fn psv_scanner_distinguishes_escaped_separators_and_cell_specifiers() {
        let source = "2+^s|wide \\| literal |next\n.2+^.>a|nested";
        let table = scan_psv(source, range(source));
        assert_eq!(table.cells.len(), 3);
        assert_eq!(table.cells[0].column_span, 2);
        assert_eq!(
            table.cells[0].horizontal_alignment,
            Some(HorizontalAlignment::Center)
        );
        assert_eq!(table.cells[0].style, TableCellStyle::Strong);
        assert_eq!(table.cells[0].raw, "wide \\| literal");
        assert_eq!(table.cells[2].row_span, 2);
        assert_eq!(
            table.cells[2].vertical_alignment,
            Some(VerticalAlignment::Bottom)
        );
        assert_eq!(table.cells[2].style, TableCellStyle::AsciiDoc);
    }

    #[test]
    fn row_layout_accounts_for_column_and_row_spans() {
        let source = "|a .2+|b\n|c\n|d |e";
        let raw = scan_psv(source, range(source));
        assert_eq!(raw.cells.len(), 5);
        assert_eq!(raw.cells[1].row_span, 2);
    }
}
