//! One-shot row and span layout for configured tables.

use crate::source::TextRange;

use super::model::{
    ConfiguredCell, ConfiguredTable, LaidOutCell, LaidOutRow, LaidOutTable, TableSection,
};

impl ConfiguredTable {
    pub(crate) fn layout(self) -> LaidOutTable {
        let ConfiguredTable {
            format,
            separator,
            content_range,
            columns,
            cells,
            presentation,
            problems,
            header,
            footer,
        } = self;
        let column_count = columns.len();
        let mut rows = if column_count == 0 {
            Vec::new()
        } else {
            layout_rows(cells, &columns)
        };
        if header && let Some(row) = rows.first_mut() {
            row.section = TableSection::Header;
        }
        if footer && let Some(row) = rows.last_mut() {
            row.section = TableSection::Footer;
        }
        LaidOutTable {
            format,
            separator,
            content_range,
            columns,
            rows,
            presentation,
            problems,
        }
    }
}

fn layout_rows(
    cells: Vec<ConfiguredCell>,
    columns: &[super::model::TableColumn],
) -> Vec<LaidOutRow> {
    let column_count = columns.len();
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
            let occupied_span = (next.column_span as usize).min(column_count);
            if column + occupied_span > column_count
                || pending[column..column + occupied_span]
                    .iter()
                    .any(|remaining| *remaining > 0)
            {
                break;
            }
            let mut cell = input.next().expect("peeked table cell exists");
            apply_column_style(&mut cell, &columns[column]);
            if cell.row_span > 1 {
                for remaining in &mut pending[column..column + occupied_span] {
                    *remaining = (*remaining).max(cell.row_span);
                }
            }
            row.push(LaidOutCell {
                cell,
                column_index: column as u32,
            });
            column += occupied_span;
        }
        if row.is_empty() {
            // An explicit column definition may be narrower than a cell span.
            // Consume that cell once at the first column rather than retrying
            // it indefinitely. Its semantic span remains lossless.
            pending.fill(0);
            let mut cell = input
                .next()
                .expect("non-empty input has a cell after clearing pending spans");
            apply_column_style(&mut cell, &columns[0]);
            let occupied_span = (cell.column_span as usize).min(column_count);
            if cell.row_span > 1 {
                for remaining in &mut pending[..occupied_span] {
                    *remaining = cell.row_span;
                }
            }
            row.push(LaidOutCell {
                cell,
                column_index: 0,
            });
        }
        rows.push(LaidOutRow {
            range: TextRange::new(
                row.first().expect("non-empty row").cell.range.start(),
                row.last().expect("non-empty row").cell.range.end(),
            )
            .expect("table row range is ordered"),
            section: TableSection::Body,
            cells: row,
        });
    }
    rows
}

fn apply_column_style(cell: &mut ConfiguredCell, column: &super::model::TableColumn) {
    if !cell.style_is_explicit {
        cell.style = column.style;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::source::TextSize;
    use crate::table::model::{
        HorizontalAlignment, TableCellStyle, TableColumn, TableFormat, TablePresentation,
        VerticalAlignment,
    };

    fn range(start: usize, end: usize) -> TextRange {
        TextRange::new(
            TextSize::new(start).expect("start"),
            TextSize::new(end).expect("end"),
        )
        .expect("range")
    }

    fn cell(start: usize, span: u32, row_span: u32) -> ConfiguredCell {
        ConfiguredCell {
            range: range(start, start + 1),
            marker_range: range(start, start),
            content_range: range(start, start + 1),
            raw: start.to_string(),
            column_span: span,
            row_span,
            horizontal_alignment: None,
            vertical_alignment: None,
            style: TableCellStyle::Default,
            style_is_explicit: false,
        }
    }

    #[test]
    fn layout_consumes_cells_once_and_applies_column_style_before_lowering() {
        let configured = ConfiguredTable {
            format: TableFormat::Psv,
            separator: '|',
            content_range: range(0, 5),
            columns: vec![
                TableColumn {
                    index: 0,
                    width: None,
                    horizontal_alignment: HorizontalAlignment::Left,
                    vertical_alignment: VerticalAlignment::Top,
                    style: TableCellStyle::AsciiDoc,
                },
                TableColumn {
                    index: 1,
                    width: None,
                    horizontal_alignment: HorizontalAlignment::Left,
                    vertical_alignment: VerticalAlignment::Top,
                    style: TableCellStyle::Strong,
                },
            ],
            cells: vec![cell(0, 1, 2), cell(1, 1, 1), cell(2, 1, 1)],
            presentation: TablePresentation::default(),
            problems: Vec::new(),
            header: true,
            footer: false,
        };
        let laid_out = configured.layout();
        assert_eq!(laid_out.rows.len(), 2);
        assert_eq!(laid_out.rows[0].section, TableSection::Header);
        assert_eq!(
            laid_out.rows[0].cells[0].cell.style,
            TableCellStyle::AsciiDoc
        );
        assert_eq!(laid_out.rows[0].cells[1].cell.style, TableCellStyle::Strong);
        assert_eq!(laid_out.rows[1].cells[0].column_index, 1);
    }

    #[test]
    fn layout_terminates_when_row_spans_cover_every_column() {
        let columns = vec![
            TableColumn {
                index: 0,
                width: None,
                horizontal_alignment: HorizontalAlignment::Left,
                vertical_alignment: VerticalAlignment::Top,
                style: TableCellStyle::Default,
            },
            TableColumn {
                index: 1,
                width: None,
                horizontal_alignment: HorizontalAlignment::Left,
                vertical_alignment: VerticalAlignment::Top,
                style: TableCellStyle::Default,
            },
        ];
        let rows = layout_rows(vec![cell(0, 2, u32::MAX), cell(1, 1, 1)], &columns);
        assert_eq!(rows.iter().map(|row| row.cells.len()).sum::<usize>(), 2);
        assert_eq!(rows[0].cells[0].column_index, 0);
        assert_eq!(rows[1].cells[0].column_index, 0);
    }

    #[test]
    fn layout_consumes_oversized_spans_and_duplicated_row_spans_once() {
        let columns = vec![TableColumn {
            index: 0,
            width: None,
            horizontal_alignment: HorizontalAlignment::Left,
            vertical_alignment: VerticalAlignment::Top,
            style: TableCellStyle::Default,
        }];
        let rows = layout_rows(
            vec![cell(0, u32::MAX, 2), cell(1, u32::MAX, 2), cell(2, 1, 1)],
            &columns,
        );
        assert_eq!(
            rows.iter()
                .flat_map(|row| &row.cells)
                .map(|cell| cell.cell.raw.as_str())
                .collect::<Vec<_>>(),
            ["0", "1", "2"]
        );
        assert!(
            rows.iter()
                .flat_map(|row| &row.cells)
                .all(|cell| cell.column_index == 0)
        );
    }
}
