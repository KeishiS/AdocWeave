//! Metadata, input, column, and presentation resolution for tables.

use crate::parser::{BlockMetadata, ElementAttribute};
use crate::source::TextRange;

use super::model::{
    ConfiguredCell, ConfiguredTable, HorizontalAlignment, ScannedTable, TableCellStyle,
    TableColumn, TableFormat, TableFrame, TableGrid, TablePresentation, TableProblem,
    TableProblemKind, TableStripes, VerticalAlignment,
};
use super::scan::{delimiter_separator, valid_custom_separator};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct TableInputSpec {
    pub format: TableFormat,
    pub separator: char,
}

#[cfg(test)]
impl TableInputSpec {
    pub(crate) fn resolve(
        delimiter: &str,
        delimiter_range: TextRange,
        metadata: &BlockMetadata,
    ) -> (Self, Vec<TableProblem>) {
        resolve_input(delimiter, delimiter_range, metadata)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ResolvedTableConfiguration {
    input: TableInputSpec,
    input_problems: Vec<TableProblem>,
    columns: Option<Vec<TableColumn>>,
    presentation: TablePresentation,
    presentation_problems: Vec<TableProblem>,
    explicit_header: bool,
    explicit_noheader: bool,
    footer: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum TableConfigurationError {
    ColumnCount(u64),
    ColumnWidth(u64),
}

impl ResolvedTableConfiguration {
    pub(crate) fn resolve(
        delimiter: &str,
        delimiter_range: TextRange,
        metadata: &BlockMetadata,
        maximum_columns: usize,
    ) -> Result<Self, TableConfigurationError> {
        let (input, input_problems) = resolve_input(delimiter, delimiter_range, metadata);
        let columns = resolve_columns(metadata, maximum_columns)?;
        let (presentation, presentation_problems) = resolve_presentation(metadata);
        Ok(Self {
            input,
            input_problems,
            columns,
            presentation,
            presentation_problems,
            explicit_header: has_option(metadata, "header"),
            explicit_noheader: has_option(metadata, "noheader"),
            footer: has_option(metadata, "footer"),
        })
    }

    pub(crate) const fn input(&self) -> TableInputSpec {
        self.input
    }

    pub(crate) fn column_styles(&self) -> impl Iterator<Item = TableCellStyle> + '_ {
        self.columns.iter().flatten().map(|column| column.style)
    }

    pub(crate) fn configure<E>(
        self,
        scanned: ScannedTable,
        mut checkpoint: impl FnMut() -> Result<(), E>,
    ) -> Result<ConfiguredTable, E> {
        let columns = self
            .columns
            .unwrap_or_else(|| default_columns(scanned.inferred_columns));
        let mut problems = self.input_problems;
        problems.extend(scanned.problems);
        problems.extend(self.presentation_problems);
        let mut cells = Vec::with_capacity(scanned.cells.len());
        for cell in scanned.cells {
            for _ in 0..cell.duplication {
                checkpoint()?;
                cells.push(ConfiguredCell {
                    range: cell.range,
                    marker_range: cell.marker_range,
                    content_range: cell.content_range,
                    raw: cell.raw.clone(),
                    column_span: cell.column_span,
                    row_span: cell.row_span,
                    horizontal_alignment: cell.horizontal_alignment,
                    vertical_alignment: cell.vertical_alignment,
                    style: cell.style,
                    style_is_explicit: cell.style_is_explicit,
                });
            }
        }
        Ok(ConfiguredTable {
            format: scanned.format,
            separator: scanned.separator,
            content_range: scanned.content_range,
            columns,
            cells,
            presentation: self.presentation,
            problems,
            header: self.explicit_header
                || (!self.explicit_noheader && scanned.implicit_header_candidate),
            footer: self.footer,
        })
    }
}

fn resolve_input(
    delimiter: &str,
    delimiter_range: TextRange,
    metadata: &BlockMetadata,
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

fn resolve_columns(
    metadata: &BlockMetadata,
    maximum_columns: usize,
) -> Result<Option<Vec<TableColumn>>, TableConfigurationError> {
    let Some(value) = metadata
        .attributes
        .iter()
        .rev()
        .find(|attribute| attribute.name.as_deref() == Some("cols"))
        .map(|attribute| attribute.value.trim_matches('"'))
    else {
        return Ok(None);
    };
    if value.trim().is_empty() {
        return Ok(None);
    }
    let mut columns = Vec::new();
    let mut actual = 0_u64;
    let maximum_columns = maximum_columns as u64;
    for value in value.split(',').map(str::trim) {
        let (count, spec) = match value.split_once('*') {
            Some((count, spec)) => match parse_unsigned(count) {
                Ok(count) => (count, spec),
                Err(UnsignedError::Invalid) => (1, value),
                Err(UnsignedError::Overflow) => {
                    return Err(TableConfigurationError::ColumnCount(u64::MAX));
                }
            },
            None => (1, value),
        };
        let count = count.max(1);
        actual = actual.saturating_add(count);
        if actual > maximum_columns {
            return Err(TableConfigurationError::ColumnCount(actual));
        }
        let column = parse_column(spec)?;
        columns.extend(std::iter::repeat_n(column, count as usize));
    }
    for (index, column) in columns.iter_mut().enumerate() {
        column.index = index as u32;
    }
    Ok((!columns.is_empty()).then_some(columns))
}

fn default_columns(count: usize) -> Vec<TableColumn> {
    (0..count)
        .map(|index| TableColumn {
            index: index as u32,
            width: None,
            horizontal_alignment: HorizontalAlignment::Left,
            vertical_alignment: VerticalAlignment::Top,
            style: TableCellStyle::Default,
        })
        .collect()
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum UnsignedError {
    Invalid,
    Overflow,
}

fn parse_unsigned(value: &str) -> Result<u64, UnsignedError> {
    if value.is_empty() || !value.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(UnsignedError::Invalid);
    }
    value.bytes().try_fold(0_u64, |current, byte| {
        current
            .checked_mul(10)
            .and_then(|current| current.checked_add(u64::from(byte - b'0')))
            .ok_or(UnsignedError::Overflow)
    })
}

fn parse_column(value: &str) -> Result<TableColumn, TableConfigurationError> {
    let width = value
        .bytes()
        .filter(u8::is_ascii_digit)
        .try_fold(None, |current, byte| {
            let width = current
                .unwrap_or(0_u64)
                .checked_mul(10)
                .and_then(|current| current.checked_add(u64::from(byte - b'0')))
                .ok_or(TableConfigurationError::ColumnWidth(u64::MAX))?;
            Ok::<_, TableConfigurationError>(Some(width))
        })?
        .map(|width| u32::try_from(width).map_err(|_| TableConfigurationError::ColumnWidth(width)))
        .transpose()?;
    let horizontal_alignment = value.chars().find_map(|character| match character {
        '<' => Some(HorizontalAlignment::Left),
        '^' => Some(HorizontalAlignment::Center),
        '>' => Some(HorizontalAlignment::Right),
        _ => None,
    });
    Ok(TableColumn {
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
            .and_then(super::scan::style)
            .unwrap_or(TableCellStyle::Default),
    })
}

fn has_option(metadata: &BlockMetadata, name: &str) -> bool {
    metadata.options.iter().any(|option| option.value == name)
        || metadata.attributes.iter().any(|attribute| {
            attribute.name.as_deref() == Some("options")
                && attribute
                    .value
                    .trim_matches('"')
                    .split(',')
                    .any(|option| option.trim() == name)
        })
}

fn resolve_presentation(metadata: &BlockMetadata) -> (TablePresentation, Vec<TableProblem>) {
    let mut presentation = TablePresentation::default();
    let mut problems = Vec::new();
    let attribute = |name| {
        metadata
            .attributes
            .iter()
            .find(|attribute| attribute.name.as_deref() == Some(name))
    };
    for name in ["frame", "grid", "stripes", "width"] {
        let mut attributes = metadata
            .attributes
            .iter()
            .filter(|attribute| attribute.name.as_deref() == Some(name));
        if attributes.next().is_none() {
            continue;
        }
        for duplicate in attributes {
            invalid_presentation(duplicate, &mut problems);
        }
    }
    if let Some(attribute) = attribute("frame") {
        presentation.frame = match attribute.value.as_str() {
            "all" => TableFrame::All,
            "ends" => TableFrame::Ends,
            "none" => TableFrame::None,
            "sides" => TableFrame::Sides,
            _ => {
                invalid_presentation(attribute, &mut problems);
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
                invalid_presentation(attribute, &mut problems);
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
                invalid_presentation(attribute, &mut problems);
                TableStripes::None
            }
        };
    }
    if let Some(attribute) = attribute("width") {
        presentation.width = percentage_width(&attribute.value);
        if presentation.width.is_none() {
            invalid_presentation(attribute, &mut problems);
        }
    }
    presentation.autowidth = has_option(metadata, "autowidth");
    if presentation.autowidth && presentation.width.is_some() {
        if let Some(attribute) = attribute("width") {
            invalid_presentation(attribute, &mut problems);
        }
        presentation.width = None;
    }
    (presentation, problems)
}

fn invalid_presentation(attribute: &ElementAttribute, problems: &mut Vec<TableProblem>) {
    problems.push(TableProblem {
        kind: TableProblemKind::InvalidPresentation,
        range: attribute.range,
    });
}

fn percentage_width(value: &str) -> Option<u8> {
    let value = value.strip_suffix('%').unwrap_or(value);
    (!value.is_empty() && value.bytes().all(|byte| byte.is_ascii_digit()))
        .then(|| value.parse::<u8>().ok())
        .flatten()
        .filter(|value| (1..=100).contains(value))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::source::TextSize;

    fn range(value: &str) -> TextRange {
        TextRange::new(
            TextSize::new(0).expect("start"),
            TextSize::new(value.len()).expect("end"),
        )
        .expect("range")
    }

    #[test]
    fn repeated_columns_are_rejected_before_allocation_exceeds_the_limit() {
        let source = "[cols=\"1000000000*a\"]";
        let metadata = BlockMetadata {
            attributes: vec![ElementAttribute {
                name: Some("cols".to_owned()),
                value: "1000000000*a".to_owned(),
                range: range(source),
            }],
            ..Default::default()
        };
        assert_eq!(
            ResolvedTableConfiguration::resolve("|===", range(source), &metadata, 4),
            Err(TableConfigurationError::ColumnCount(1_000_000_000))
        );
    }

    #[test]
    fn empty_column_specs_resolve_to_default_columns_without_losing_position() {
        for (value, expected_widths) in [
            (",,", vec![None, None, None]),
            (",2", vec![None, Some(2)]),
            ("2,", vec![Some(2), None]),
            ("2,,3", vec![Some(2), None, Some(3)]),
        ] {
            let source = format!("[cols=\"{value}\"]");
            let metadata = BlockMetadata {
                attributes: vec![ElementAttribute {
                    name: Some("cols".to_owned()),
                    value: value.to_owned(),
                    range: range(&source),
                }],
                ..Default::default()
            };
            let configuration =
                ResolvedTableConfiguration::resolve("|===", range(&source), &metadata, 8)
                    .expect("empty column specs are defaults");
            let columns = configuration.columns.expect("explicit columns");
            assert_eq!(
                columns
                    .iter()
                    .map(|column| column.width)
                    .collect::<Vec<_>>(),
                expected_widths,
                "{value:?}"
            );
            assert!(columns.iter().enumerate().all(|(index, column)| {
                column.index == index as u32
                    && column.horizontal_alignment == HorizontalAlignment::Left
                    && column.vertical_alignment == VerticalAlignment::Top
                    && column.style == TableCellStyle::Default
            }));
        }
    }

    #[test]
    fn entirely_empty_cols_attribute_keeps_column_inference() {
        for value in ["", "   "] {
            let source = format!("[cols=\"{value}\"]");
            let metadata = BlockMetadata {
                attributes: vec![ElementAttribute {
                    name: Some("cols".to_owned()),
                    value: value.to_owned(),
                    range: range(&source),
                }],
                ..Default::default()
            };
            let configuration =
                ResolvedTableConfiguration::resolve("|===", range(&source), &metadata, 8)
                    .expect("empty cols keeps inference");
            assert_eq!(configuration.columns, None);
        }
    }

    #[test]
    fn empty_column_specs_count_toward_the_column_limit() {
        let source = "[cols=\",,,,\"]";
        let metadata = BlockMetadata {
            attributes: vec![ElementAttribute {
                name: Some("cols".to_owned()),
                value: ",,,,".to_owned(),
                range: range(source),
            }],
            ..Default::default()
        };
        assert_eq!(
            ResolvedTableConfiguration::resolve("|===", range(source), &metadata, 4),
            Err(TableConfigurationError::ColumnCount(5))
        );
    }

    #[test]
    fn unrepresentable_column_numbers_are_rejected() {
        for (value, expected) in [
            (
                "18446744073709551616*a",
                TableConfigurationError::ColumnCount(u64::MAX),
            ),
            (
                "4294967296",
                TableConfigurationError::ColumnWidth(4_294_967_296),
            ),
        ] {
            let source = format!("[cols=\"{value}\"]");
            let metadata = BlockMetadata {
                attributes: vec![ElementAttribute {
                    name: Some("cols".to_owned()),
                    value: value.to_owned(),
                    range: range(&source),
                }],
                ..Default::default()
            };
            assert_eq!(
                ResolvedTableConfiguration::resolve("|===", range(&source), &metadata, 4),
                Err(expected)
            );
        }
    }

    #[test]
    fn malformed_column_repetition_counts_keep_the_legacy_single_column_shape() {
        for value in ["x*a", "*a"] {
            let source = format!("[cols=\"{value}\"]");
            let metadata = BlockMetadata {
                attributes: vec![ElementAttribute {
                    name: Some("cols".to_owned()),
                    value: value.to_owned(),
                    range: range(&source),
                }],
                ..Default::default()
            };
            let configuration =
                ResolvedTableConfiguration::resolve("|===", range(&source), &metadata, 4)
                    .expect("malformed repetition count recovers");
            assert_eq!(configuration.columns.as_ref().map(Vec::len), Some(1));
            assert_eq!(
                configuration
                    .columns
                    .as_ref()
                    .and_then(|columns| columns.first())
                    .map(|column| column.style),
                Some(TableCellStyle::AsciiDoc)
            );
        }
    }

    #[test]
    fn cell_materialization_stops_at_the_cancellation_checkpoint() {
        let configuration = ResolvedTableConfiguration::resolve(
            "|===",
            range("|==="),
            &BlockMetadata::default(),
            4,
        )
        .expect("configuration");
        let cell_range = range("value");
        let scanned = ScannedTable {
            format: TableFormat::Psv,
            separator: '|',
            content_range: cell_range,
            inferred_columns: 1,
            implicit_header_candidate: false,
            cells: vec![super::super::model::ScannedCell {
                range: cell_range,
                marker_range: TextRange::new(cell_range.start(), cell_range.start())
                    .expect("marker range"),
                content_range: cell_range,
                raw: "value".to_owned(),
                column_span: 1,
                row_span: 1,
                horizontal_alignment: None,
                vertical_alignment: None,
                style: TableCellStyle::Default,
                style_is_explicit: false,
                duplication: 100,
            }],
            problems: Vec::new(),
        };
        let mut checks = 0;
        let result = configuration.configure(scanned, || {
            checks += 1;
            if checks == 4 { Err(()) } else { Ok(()) }
        });
        assert_eq!(result, Err(()));
        assert_eq!(checks, 4);
    }
}
