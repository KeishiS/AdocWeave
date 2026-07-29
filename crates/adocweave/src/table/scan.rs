//! Lossless, I/O-free table format scanners.

use super::configuration::TableInputSpec;
use super::model::{
    HorizontalAlignment, ScannedCell, ScannedTable, TableCellStyle, TableFormat, TableProblem,
    TableProblemKind, VerticalAlignment,
};
use crate::source::{TextRange, TextSize};

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

pub(super) fn valid_custom_separator(separator: char) -> bool {
    separator != '=' && !separator.is_control() && !separator.is_whitespace()
}

#[cfg(test)]
pub(crate) fn scan(value: &str, range: TextRange, input: TableInputSpec) -> ScannedTable {
    scan_with_psv_context(value, range, input, &[])
}

pub(crate) fn scan_with_configuration(
    value: &str,
    range: TextRange,
    input: TableInputSpec,
    column_styles: &[TableCellStyle],
) -> ScannedTable {
    scan_with_psv_context(value, range, input, column_styles)
}

fn scan_with_psv_context(
    value: &str,
    range: TextRange,
    input: TableInputSpec,
    column_styles: &[TableCellStyle],
) -> ScannedTable {
    let implicit_header_layout = implicit_header_layout(value);
    match input.format {
        TableFormat::Psv => {
            let mut table = scan_psv_with_separator(
                value,
                range,
                input.separator,
                &implicit_header_layout,
                column_styles,
            );
            table.implicit_header_candidate =
                table.implicit_header_candidate && first_psv_row_is_single_line(&table);
            table
        }
        TableFormat::Csv | TableFormat::Dsv | TableFormat::Tsv => {
            scan_delimited(value, range, input, implicit_header_layout)
        }
    }
}

#[cfg(test)]
pub(crate) fn scan_psv(value: &str, range: TextRange) -> ScannedTable {
    scan_psv_with_separator(value, range, '|', &ImplicitHeaderLayout::default(), &[])
}

fn scan_psv_with_separator(
    value: &str,
    range: TextRange,
    separator: char,
    header_layout: &ImplicitHeaderLayout,
    column_styles: &[TableCellStyle],
) -> ScannedTable {
    let mut cells = Vec::<ScannedCell>::new();
    let mut offset = 0;
    let mut maximum_columns = 0;
    let mut first_record_columns = 0;
    let mut last_cell_start_column = 0_usize;
    let mut next_cell_start_column = 0_usize;
    let mut ignored_comment_lines = header_layout.ignored_comment_lines.iter().peekable();
    for line_with_ending in value.split_inclusive('\n') {
        let line = line_with_ending
            .strip_suffix('\n')
            .unwrap_or(line_with_ending);
        let line = line.strip_suffix('\r').unwrap_or(line);
        if ignored_comment_lines.next_if_eq(&&offset).is_some() {
            let continues_asciidoc_cell = cells.last().is_some_and(|cell| {
                cell_uses_asciidoc_style(cell, last_cell_start_column, column_styles)
            });
            if !continues_asciidoc_cell {
                offset += line_with_ending.len();
                continue;
            }
        }
        let markers = marker_positions(line, separator);
        let line_columns = markers
            .iter()
            .map(|(start, separator)| {
                parse_cell_spec(&line[*start..*separator]).duplication as usize
            })
            .fold(0_usize, usize::saturating_add);
        maximum_columns = maximum_columns.max(line_columns);
        if offset == header_layout.first_record_start.unwrap_or(usize::MAX) {
            first_record_columns = line_columns;
        }
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
            last_cell_start_column = next_cell_start_column;
            if !column_styles.is_empty() {
                let contribution = (spec.column_span as usize % column_styles.len())
                    .saturating_mul(spec.duplication as usize % column_styles.len())
                    % column_styles.len();
                next_cell_start_column =
                    (next_cell_start_column + contribution) % column_styles.len();
            }
            let marker_range = absolute_range(range, offset + marker_start, offset + pipe + 1);
            let content_range = absolute_range(range, offset + content_start, offset + raw_end);
            cells.push(ScannedCell {
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
    ScannedTable {
        format: TableFormat::Psv,
        separator,
        content_range: range,
        inferred_columns: if header_layout.candidate && first_record_columns != 0 {
            first_record_columns
        } else {
            maximum_columns
        }
        .max(1),
        implicit_header_candidate: header_layout.candidate && first_record_columns != 0,
        cells,
        problems: Vec::new(),
    }
}

#[derive(Default)]
struct ImplicitHeaderLayout {
    candidate: bool,
    first_record_start: Option<usize>,
    ignored_comment_lines: Vec<usize>,
}

fn implicit_header_layout(value: &str) -> ImplicitHeaderLayout {
    let mut layout = ImplicitHeaderLayout::default();
    let mut offset = 0;
    for line_with_ending in value.split_inclusive('\n') {
        let line = line_with_ending
            .trim_end_matches('\n')
            .trim_end_matches('\r');
        if is_line_comment(line) {
            layout.ignored_comment_lines.push(offset);
        } else if layout.first_record_start.is_none() {
            if line.is_empty() {
                return ImplicitHeaderLayout::default();
            }
            layout.first_record_start = Some(offset);
        } else {
            if line.is_empty() {
                layout.candidate = true;
                return layout;
            }
            return ImplicitHeaderLayout::default();
        }
        offset += line_with_ending.len();
    }
    ImplicitHeaderLayout::default()
}

fn is_line_comment(line: &str) -> bool {
    line.trim_start_matches([' ', '\t']).starts_with("//")
}

fn cell_uses_asciidoc_style(
    cell: &ScannedCell,
    start_column: usize,
    column_styles: &[TableCellStyle],
) -> bool {
    if cell.style_is_explicit {
        return cell.style == TableCellStyle::AsciiDoc;
    }
    if column_styles.is_empty() {
        return false;
    }
    let checks = (cell.duplication as usize).min(column_styles.len());
    (0..checks).any(|duplicate| {
        let column = start_column
            .saturating_add(duplicate.saturating_mul(cell.column_span as usize))
            % column_styles.len();
        column_styles[column] == TableCellStyle::AsciiDoc
    })
}

fn first_psv_row_is_single_line(table: &ScannedTable) -> bool {
    let mut columns = 0_usize;
    for cell in &table.cells {
        if cell.raw.contains('\n') {
            return false;
        }
        columns = columns
            .saturating_add((cell.column_span as usize).saturating_mul(cell.duplication as usize));
        if columns >= table.inferred_columns {
            return true;
        }
    }
    false
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
            let prefix_start = cell_spec_start(&line[..pipe], separator);
            let boundary =
                prefix_start == 0 || line.as_bytes()[prefix_start - 1].is_ascii_whitespace();
            boundary.then_some((prefix_start, pipe))
        })
        .collect()
}

fn cell_spec_start(prefix: &str, separator: char) -> usize {
    let mut start = prefix.len();
    for (offset, character) in prefix.char_indices().rev() {
        if character == separator {
            break;
        }
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

fn scan_delimited(
    value: &str,
    range: TextRange,
    input: TableInputSpec,
    header_layout: ImplicitHeaderLayout,
) -> ScannedTable {
    let mut implicit_header_candidate = header_layout.candidate;
    let mut cells = Vec::new();
    let mut problems = Vec::new();
    let mut field_start = 0;
    let mut content_start = 0;
    let mut raw = String::new();
    let mut quoted = false;
    let mut quote_closed = false;
    let mut maximum_columns = 0_usize;
    let mut first_record_columns = 0_usize;
    let mut row_columns = 0_usize;
    let mut completed_rows = 0_usize;
    let mut ignored_comment_lines = header_layout.ignored_comment_lines.iter().peekable();
    let mut chars = value.char_indices().peekable();
    while let Some((offset, character)) = chars.next() {
        if !quoted && ignored_comment_lines.next_if_eq(&&offset).is_some() {
            let mut next_line = value.len();
            for (comment_offset, comment_character) in chars.by_ref() {
                if comment_character == '\n' {
                    next_line = comment_offset + comment_character.len_utf8();
                    break;
                }
            }
            field_start = next_line;
            content_start = next_line;
            raw.clear();
            quote_closed = false;
            continue;
        }
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
                if character == '\n' && completed_rows == 0 {
                    implicit_header_candidate = false;
                }
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
            if row_end
                && row_columns == 0
                && raw.trim_end_matches('\r').is_empty()
                && field_start == content_start
            {
                field_start = offset + character.len_utf8();
                content_start = field_start;
                raw.clear();
                quote_closed = false;
                continue;
            }
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
                if completed_rows == 0 {
                    first_record_columns = row_columns;
                }
                maximum_columns = maximum_columns.max(row_columns);
                row_columns = 0;
                completed_rows += 1;
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
    ScannedTable {
        format: input.format,
        separator: input.separator,
        content_range: range,
        inferred_columns: if implicit_header_candidate && first_record_columns != 0 {
            first_record_columns
        } else {
            maximum_columns.max(row_columns)
        }
        .max(1),
        implicit_header_candidate,
        cells,
        problems,
    }
}

fn push_delimited_cell(
    cells: &mut Vec<ScannedCell>,
    parent: TextRange,
    field_start: usize,
    content_start: usize,
    field_end: usize,
    raw: String,
) {
    cells.push(ScannedCell {
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
    fn separated_scanner_skips_blank_records_without_shifting_later_rows() {
        let source = "name,value\r\n\r\nalpha,one\r\n";
        let table = scan(
            source,
            range(source),
            TableInputSpec {
                format: TableFormat::Csv,
                separator: ',',
            },
        );
        assert_eq!(table.inferred_columns, 2);
        assert_eq!(table.cells.len(), 4);
        assert_eq!(table.cells[2].raw, "alpha");
        assert!(table.implicit_header_candidate);

        let multiline = "name,\"line one\n\nline two\"\n\nalpha,one\n";
        let table = scan(
            multiline,
            range(multiline),
            TableInputSpec {
                format: TableFormat::Csv,
                separator: ',',
            },
        );
        assert!(!table.implicit_header_candidate);
        assert_eq!(table.cells[1].raw, "line one\n\nline two");

        let middle_blank = "name,value\nalpha,one\n\nbeta,two\n";
        let table = scan(
            middle_blank,
            range(middle_blank),
            TableInputSpec {
                format: TableFormat::Csv,
                separator: ',',
            },
        );
        assert!(!table.implicit_header_candidate);
        assert_eq!(table.cells.len(), 6);
        assert_eq!(table.cells[4].raw, "beta");
        assert_eq!(table.cells[5].raw, "two");
    }

    #[test]
    fn scanners_use_the_first_record_for_the_implicit_column_count() {
        let psv = "|h1 |h2\r\n\r\n|a |b |c";
        let table = scan(
            psv,
            range(psv),
            TableInputSpec {
                format: TableFormat::Psv,
                separator: '|',
            },
        );
        assert_eq!(table.inferred_columns, 2);
        assert!(table.implicit_header_candidate);

        let csv = "h1,h2\n\na,b,c\n";
        let table = scan(
            csv,
            range(csv),
            TableInputSpec {
                format: TableFormat::Csv,
                separator: ',',
            },
        );
        assert_eq!(table.inferred_columns, 2);
        assert!(table.implicit_header_candidate);

        let ordinary = "a,b\nc,d,e\n";
        let table = scan(
            ordinary,
            range(ordinary),
            TableInputSpec {
                format: TableFormat::Csv,
                separator: ',',
            },
        );
        assert_eq!(table.inferred_columns, 3);
        assert!(!table.implicit_header_candidate);
    }

    #[test]
    fn header_comments_match_lossless_line_comment_classification() {
        for source in [
            "   // leading\n|h1 |h2\n/// separator comment\n\n|a |b\n",
            "\t// leading\r\n|h1 |h2\r\n   /// separator comment\r\n\r\n|a |b\r\n",
        ] {
            let table = scan(
                source,
                range(source),
                TableInputSpec {
                    format: TableFormat::Psv,
                    separator: '|',
                },
            );
            assert!(table.implicit_header_candidate, "{source:?}");
            assert_eq!(table.cells[0].raw, "h1");
            assert_eq!(table.cells[1].raw, "h2");
        }

        let asciidoc_cell = "a|....\n// literal must remain\n\n....\n";
        let table = scan(
            asciidoc_cell,
            range(asciidoc_cell),
            TableInputSpec {
                format: TableFormat::Psv,
                separator: '|',
            },
        );
        assert!(!table.implicit_header_candidate);
        assert_eq!(table.cells[0].raw, "....\n// literal must remain\n\n....");
    }

    #[test]
    fn unclosed_first_delimited_record_never_becomes_an_implicit_header() {
        let source = "name,\"open\n\ncontinued";
        let table = scan(
            source,
            range(source),
            TableInputSpec {
                format: TableFormat::Csv,
                separator: ',',
            },
        );
        assert!(!table.implicit_header_candidate);
        assert_eq!(
            table.problems,
            [TableProblem {
                kind: TableProblemKind::UnclosedQuotedCell,
                range: absolute_range(range(source), 5, source.len()),
            }]
        );
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

    #[test]
    fn asciidoc_style_lookup_is_bounded_for_large_duplication() {
        let source = "4294967295*|H\n// cell content\n\n|body";
        let table = scan_with_psv_context(
            source,
            range(source),
            TableInputSpec {
                format: TableFormat::Psv,
                separator: '|',
            },
            &[TableCellStyle::AsciiDoc, TableCellStyle::Default],
        );
        assert!(!table.implicit_header_candidate);
        assert_eq!(table.cells[0].duplication, u32::MAX);
        assert_eq!(table.cells[0].raw, "H\n// cell content");
    }

    #[test]
    fn custom_separator_is_not_reparsed_as_a_previous_cell_spec() {
        // `>` is both a permitted custom separator and a cell-spec character.
        // The second `>` must remain content of the cell opened by the first.
        let source = ">e>:Rc{he";
        let table = scan(
            source,
            range(source),
            TableInputSpec {
                format: TableFormat::Psv,
                separator: '>',
            },
        );
        assert_eq!(table.cells.len(), 1);
        assert_eq!(table.cells[0].raw, "e>:Rc{he");
        assert_eq!(table.cells[0].range, range(source));

        // Keep the minimized fuzz input at the parser boundary as well.  This
        // previously constructed an inverted table-cell range and panicked.
        let fuzz_regression = "\
COip&t

=ip&t=
>===
:Rc{;h
>e>:Rc{he}";
        crate::Engine::new(crate::AnalysisOptions::default())
            .analyze(fuzz_regression)
            .expect("custom separator input must analyze without a panic");
    }
}
