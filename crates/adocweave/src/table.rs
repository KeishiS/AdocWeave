//! Typed table model and lossless, I/O-free format scanners.

use crate::inline::Inline;
use crate::source::{TextRange, TextSize};

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
    pub caption: Option<String>,
    pub frame: TableFrame,
    pub grid: TableGrid,
    pub stripes: TableStripes,
    pub width: Option<u8>,
    pub autowidth: bool,
}

impl Default for TablePresentation {
    fn default() -> Self {
        Self {
            caption: None,
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
    pub duplication: u32,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct RawTable {
    pub format: TableFormat,
    pub separator: char,
    pub content_range: TextRange,
    pub inferred_columns: usize,
    pub cells: Vec<RawCell>,
    pub problems: Vec<TableProblem>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct TableInputSpec {
    pub format: TableFormat,
    pub separator: char,
}

impl TableInputSpec {
    pub(crate) fn resolve(
        delimiter: &str,
        delimiter_range: TextRange,
        metadata: &crate::parser::BlockMetadata,
    ) -> (Self, Vec<TableProblem>) {
        resolve_input(delimiter, delimiter_range, metadata)
    }
}

pub(crate) fn delimiter_separator(value: &str) -> Option<char> {
    raw_delimiter_separator(value).filter(|separator| valid_custom_separator(*separator))
}

pub(crate) fn is_table_delimiter(value: &str) -> bool {
    raw_delimiter_separator(value).is_some()
}

fn raw_delimiter_separator(value: &str) -> Option<char> {
    let prefix = value.strip_suffix("===")?;
    let mut characters = prefix.chars();
    let separator = characters.next()?;
    (separator != '=' && characters.next().is_none()).then_some(separator)
}

fn valid_custom_separator(separator: char) -> bool {
    separator != '=' && !separator.is_control() && !separator.is_whitespace()
}

fn resolve_input(
    delimiter: &str,
    delimiter_range: TextRange,
    metadata: &crate::parser::BlockMetadata,
) -> (TableInputSpec, Vec<TableProblem>) {
    let format_attribute = metadata
        .attributes
        .iter()
        .rev()
        .find(|attribute| attribute.name.as_deref() == Some("format"));
    let parsed_format = format_attribute.and_then(|attribute| {
        match attribute
            .value
            .trim_matches('"')
            .to_ascii_lowercase()
            .as_str()
        {
            "psv" => Some(TableFormat::Psv),
            "csv" => Some(TableFormat::Csv),
            "dsv" => Some(TableFormat::Dsv),
            "tsv" => Some(TableFormat::Tsv),
            _ => None,
        }
    });
    let delimiter_separator = (delimiter != "|===")
        .then(|| delimiter_separator(delimiter))
        .flatten();
    let inferred_format = match delimiter_separator {
        Some(',') => TableFormat::Csv,
        Some(':') => TableFormat::Dsv,
        _ => TableFormat::Psv,
    };
    let format = if format_attribute.is_some() {
        parsed_format.unwrap_or(TableFormat::Psv)
    } else {
        inferred_format
    };
    let separator_attribute = metadata
        .attributes
        .iter()
        .rev()
        .find(|attribute| attribute.name.as_deref() == Some("separator"));
    let separator_value = separator_attribute.map(|attribute| attribute.value.trim_matches('"'));
    let attribute_separator = separator_value.and_then(|value| {
        let mut characters = value.chars();
        let separator = characters.next()?;
        (characters.next().is_none() && valid_custom_separator(separator)).then_some(separator)
    });
    let separator = delimiter_separator
        .or(attribute_separator)
        .unwrap_or_else(|| format.default_separator());
    let mut problems = Vec::new();
    if delimiter != "|===" && delimiter_separator.is_none() {
        problems.push(TableProblem {
            kind: TableProblemKind::InvalidSeparator,
            range: delimiter_range,
        });
    }
    if let Some(attribute) = format_attribute.filter(|_| parsed_format.is_none()) {
        problems.push(TableProblem {
            kind: TableProblemKind::InvalidFormat,
            range: attribute.range,
        });
    }
    if let Some(attribute) = separator_attribute.filter(|_| {
        !separator_value.is_some_and(|value| {
            let mut characters = value.chars();
            characters.next().is_some_and(valid_custom_separator) && characters.next().is_none()
        })
    }) {
        problems.push(TableProblem {
            kind: TableProblemKind::InvalidSeparator,
            range: attribute.range,
        });
    }
    if let (Some(delimiter_separator), Some(attribute_separator), Some(attribute)) = (
        delimiter_separator,
        attribute_separator,
        separator_attribute,
    ) && delimiter_separator != attribute_separator
    {
        problems.push(TableProblem {
            kind: TableProblemKind::InvalidSeparator,
            range: attribute.range,
        });
    }
    (TableInputSpec { format, separator }, problems)
}

pub(crate) fn scan(value: &str, range: TextRange, input: TableInputSpec) -> RawTable {
    match input.format {
        TableFormat::Psv => scan_psv_with_separator(value, range, input.separator),
        TableFormat::Csv | TableFormat::Dsv | TableFormat::Tsv => {
            scan_delimited(value, range, input)
        }
    }
}

#[cfg(test)]
pub(crate) fn scan_psv(value: &str, range: TextRange) -> RawTable {
    scan_psv_with_separator(value, range, '|')
}

fn scan_psv_with_separator(value: &str, range: TextRange, separator: char) -> RawTable {
    let mut cells = Vec::<RawCell>::new();
    let mut offset = 0;
    let mut inferred_columns = 0;
    for line_with_ending in value.split_inclusive('\n') {
        let line = line_with_ending
            .strip_suffix('\n')
            .unwrap_or(line_with_ending);
        let line = line.strip_suffix('\r').unwrap_or(line);
        let markers = marker_positions(line, separator);
        inferred_columns = inferred_columns.max(
            markers
                .iter()
                .map(|(start, separator)| parse_cell_spec(&line[*start..*separator]).duplication)
                .sum::<u32>() as usize,
        );
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
                duplication: spec.duplication,
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
        format: TableFormat::Psv,
        separator,
        content_range: range,
        inferred_columns: inferred_columns.max(1),
        cells,
        problems: Vec::new(),
    }
}

fn marker_positions(line: &str, separator: char) -> Vec<(usize, usize)> {
    line.char_indices()
        .filter_map(|(pipe, character)| {
            if character != separator {
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
                '.' | '+' | '*' | '<' | '>' | '^' | 'a' | 'd' | 'e' | 'h' | 'l' | 'm' | 's' | 'v'
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
    duplication: u32,
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
        duplication: value
            .split_once('*')
            .and_then(|(count, _)| count.parse().ok())
            .unwrap_or(1)
            .max(1),
    }
}

fn scan_delimited(value: &str, range: TextRange, input: TableInputSpec) -> RawTable {
    let mut cells = Vec::new();
    let mut problems = Vec::new();
    let mut field_start = 0;
    let mut content_start = 0;
    let mut raw = String::new();
    let mut quoted = false;
    let mut quote_closed = false;
    let mut columns = 0_usize;
    let mut row_columns = 0_usize;
    let mut chars = value.char_indices().peekable();
    while let Some((offset, character)) = chars.next() {
        if quoted {
            if character == '"' {
                if chars.peek().is_some_and(|(_, next)| *next == '"') {
                    raw.push('"');
                    chars.next();
                } else {
                    quoted = false;
                    quote_closed = true;
                }
            } else {
                raw.push(character);
            }
            continue;
        }
        if character == '"' && raw.is_empty() && !quote_closed {
            quoted = true;
            content_start = offset + character.len_utf8();
            continue;
        }
        let row_end = character == '\n';
        if character == input.separator || row_end {
            push_delimited_cell(
                &mut cells,
                range,
                field_start,
                content_start,
                offset,
                std::mem::take(&mut raw),
            );
            row_columns += 1;
            if row_end {
                columns = columns.max(row_columns);
                row_columns = 0;
            }
            field_start = offset + character.len_utf8();
            content_start = field_start;
            quote_closed = false;
        } else if !quote_closed || !character.is_ascii_whitespace() {
            raw.push(character);
        }
    }
    if field_start < value.len() || (!value.is_empty() && value.ends_with(input.separator)) {
        push_delimited_cell(
            &mut cells,
            range,
            field_start,
            content_start,
            value.len(),
            raw,
        );
        row_columns += 1;
    }
    if quoted {
        let start = absolute_range(range, field_start, field_start).start();
        let end = absolute_range(range, value.len(), value.len()).end();
        problems.push(TableProblem {
            kind: TableProblemKind::UnclosedQuotedCell,
            range: TextRange::new(start, end).expect("quoted cell range is ordered"),
        });
    }
    RawTable {
        format: input.format,
        separator: input.separator,
        content_range: range,
        inferred_columns: columns.max(row_columns).max(1),
        cells,
        problems,
    }
}

fn push_delimited_cell(
    cells: &mut Vec<RawCell>,
    parent: TextRange,
    field_start: usize,
    content_start: usize,
    field_end: usize,
    raw: String,
) {
    cells.push(RawCell {
        range: absolute_range(parent, field_start, field_end),
        marker_range: absolute_range(parent, field_start, content_start),
        content_range: absolute_range(parent, content_start, field_end),
        raw: raw.trim_end_matches('\r').to_owned(),
        column_span: 1,
        row_span: 1,
        horizontal_alignment: None,
        vertical_alignment: None,
        style: TableCellStyle::Default,
        style_is_explicit: false,
        duplication: 1,
    });
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
    // Tables are configured during parsing and again while lowering. Keep the
    // metadata-derived diagnostics idempotent across those two passes.
    table
        .problems
        .retain(|problem| problem.kind != TableProblemKind::InvalidPresentation);
    table.presentation = resolve_presentation(metadata, &mut table.problems);
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
    if has_option("header")
        && let Some(row) = table.rows.first_mut()
    {
        row.section = TableSection::Header;
    }
    if has_option("footer")
        && let Some(row) = table.rows.last_mut()
    {
        row.section = TableSection::Footer;
    }
}

fn resolve_presentation(
    metadata: &crate::parser::BlockMetadata,
    problems: &mut Vec<TableProblem>,
) -> TablePresentation {
    let mut presentation = TablePresentation {
        caption: metadata.title.as_ref().map(|title| title.value.clone()),
        ..TablePresentation::default()
    };
    // The first occurrence is authoritative. Later occurrences are diagnosed
    // as duplicates and never influence the effective presentation.
    let attribute = |name| {
        metadata
            .attributes
            .iter()
            .find(|attribute| attribute.name.as_deref() == Some(name))
    };
    let invalid = |attribute: &crate::parser::ElementAttribute,
                   problems: &mut Vec<TableProblem>| {
        problems.push(TableProblem {
            kind: TableProblemKind::InvalidPresentation,
            range: attribute.range,
        });
    };
    for name in ["frame", "grid", "stripes", "width"] {
        let mut attributes = metadata
            .attributes
            .iter()
            .filter(|attribute| attribute.name.as_deref() == Some(name));
        let Some(_) = attributes.next() else {
            continue;
        };
        for duplicate in attributes {
            invalid(duplicate, problems);
        }
    }
    if let Some(attribute) = attribute("frame") {
        presentation.frame = match attribute.value.as_str() {
            "all" => TableFrame::All,
            "ends" => TableFrame::Ends,
            "none" => TableFrame::None,
            "sides" => TableFrame::Sides,
            _ => {
                invalid(attribute, problems);
                TableFrame::All
            }
        };
    }
    if let Some(attribute) = attribute("grid") {
        presentation.grid = match attribute.value.as_str() {
            "all" => TableGrid::All,
            "cols" => TableGrid::Columns,
            "none" => TableGrid::None,
            "rows" => TableGrid::Rows,
            _ => {
                invalid(attribute, problems);
                TableGrid::All
            }
        };
    }
    if let Some(attribute) = attribute("stripes") {
        presentation.stripes = match attribute.value.as_str() {
            "all" => TableStripes::All,
            "even" => TableStripes::Even,
            "hover" => TableStripes::Hover,
            "none" => TableStripes::None,
            "odd" => TableStripes::Odd,
            _ => {
                invalid(attribute, problems);
                TableStripes::None
            }
        };
    }
    if let Some(attribute) = attribute("width") {
        presentation.width = percentage_width(&attribute.value);
        if presentation.width.is_none() {
            invalid(attribute, problems);
        }
    }
    presentation.autowidth = metadata
        .options
        .iter()
        .any(|option| option.value == "autowidth")
        || metadata.attributes.iter().any(|attribute| {
            attribute.name.as_deref() == Some("options")
                && attribute
                    .value
                    .split(',')
                    .any(|option| option.trim() == "autowidth")
        });
    if presentation.autowidth && presentation.width.is_some() {
        if let Some(attribute) = attribute("width") {
            invalid(attribute, problems);
        }
        presentation.width = None;
    }
    presentation
}

fn percentage_width(value: &str) -> Option<u8> {
    let value = value.strip_suffix('%').unwrap_or(value);
    (!value.is_empty() && value.bytes().all(|byte| byte.is_ascii_digit()))
        .then(|| value.parse::<u8>().ok())
        .flatten()
        .filter(|value| (1..=100).contains(value))
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
                    TableCellStyle::AsciiDoc => {
                        std::mem::replace(&mut cell.content, TableCellContent::Inlines(Vec::new()))
                    }
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

    #[test]
    fn separated_scanner_handles_quotes_escaped_quotes_and_multiline_cells() {
        let source = "name,description\nalpha,\"one, two\"\nbeta,\"line one\nline \"\"two\"\"\"";
        let table = scan(
            source,
            range(source),
            TableInputSpec {
                format: TableFormat::Csv,
                separator: ',',
            },
        );
        assert_eq!(table.format, TableFormat::Csv);
        assert_eq!(table.inferred_columns, 2);
        assert_eq!(table.cells.len(), 6);
        assert_eq!(table.cells[3].raw, "one, two");
        assert_eq!(table.cells[5].raw, "line one\nline \"two\"");
    }

    #[test]
    fn table_input_spec_resolves_delimiter_format_and_separator_precedence() {
        let range = range("[format=tsv,separator=;]");
        let metadata = crate::parser::BlockMetadata {
            attributes: vec![
                crate::parser::ElementAttribute {
                    name: Some("format".to_owned()),
                    value: "tsv".to_owned(),
                    range,
                },
                crate::parser::ElementAttribute {
                    name: Some("separator".to_owned()),
                    value: ";".to_owned(),
                    range,
                },
            ],
            ..Default::default()
        };
        assert_eq!(
            TableInputSpec::resolve("|===", range, &metadata),
            (
                TableInputSpec {
                    format: TableFormat::Tsv,
                    separator: ';'
                },
                Vec::new()
            )
        );

        for (delimiter, metadata, expected, problem_count) in [
            (
                ",===",
                crate::parser::BlockMetadata::default(),
                TableInputSpec {
                    format: TableFormat::Csv,
                    separator: ',',
                },
                0,
            ),
            (
                ":===",
                crate::parser::BlockMetadata::default(),
                TableInputSpec {
                    format: TableFormat::Dsv,
                    separator: ':',
                },
                0,
            ),
            (
                "!===",
                crate::parser::BlockMetadata::default(),
                TableInputSpec {
                    format: TableFormat::Psv,
                    separator: '!',
                },
                0,
            ),
            (
                ",===",
                metadata.clone(),
                TableInputSpec {
                    format: TableFormat::Tsv,
                    separator: ',',
                },
                1,
            ),
        ] {
            let (actual, problems) = TableInputSpec::resolve(delimiter, range, &metadata);
            assert_eq!(actual, expected, "{delimiter}");
            assert_eq!(problems.len(), problem_count, "{delimiter}");
        }

        assert_eq!(delimiter_separator("!==="), Some('!'));
        assert_eq!(delimiter_separator(" ==="), None);
        assert_eq!(delimiter_separator("\0==="), None);
        assert_eq!(delimiter_separator("===="), None);
        assert!(is_table_delimiter("\0==="));
    }

    #[test]
    fn psv_scanner_records_cell_duplication() {
        let source = "3*|same |last";
        let table = scan_psv(source, range(source));
        assert_eq!(table.cells[0].duplication, 3);
        assert_eq!(table.cells[1].duplication, 1);
    }
}
