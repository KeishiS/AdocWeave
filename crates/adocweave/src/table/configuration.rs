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

impl ResolvedTableConfiguration {
    pub(crate) fn resolve(
        delimiter: &str,
        delimiter_range: TextRange,
        metadata: &BlockMetadata,
        maximum_columns: usize,
    ) -> Result<Self, usize> {
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

    pub(crate) fn configure(self, scanned: ScannedTable) -> ConfiguredTable {
        let columns = self
            .columns
            .unwrap_or_else(|| default_columns(scanned.inferred_columns));
        let mut problems = self.input_problems;
        problems.extend(scanned.problems);
        problems.extend(self.presentation_problems);
        let cells = scanned
            .cells
            .into_iter()
            .flat_map(|cell| {
                (0..cell.duplication).map(move |_| ConfiguredCell {
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
                })
            })
            .collect();
        ConfiguredTable {
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
        }
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
) -> Result<Option<Vec<TableColumn>>, usize> {
    let Some(value) = metadata
        .attributes
        .iter()
        .rev()
        .find(|attribute| attribute.name.as_deref() == Some("cols"))
        .map(|attribute| attribute.value.trim_matches('"'))
    else {
        return Ok(None);
    };
    let mut columns = Vec::new();
    let mut actual = 0_usize;
    for value in value
        .split(',')
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        let (count, spec) = value
            .split_once('*')
            .and_then(|(count, spec)| count.parse::<usize>().ok().map(|count| (count, spec)))
            .unwrap_or((1, value));
        let count = count.max(1);
        actual = actual.saturating_add(count);
        if actual > maximum_columns {
            return Err(actual);
        }
        let column = parse_column(spec);
        columns.extend(std::iter::repeat_n(column, count));
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
            .and_then(super::scan::style)
            .unwrap_or(TableCellStyle::Default),
    }
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
            Err(1_000_000_000)
        );
    }
}
