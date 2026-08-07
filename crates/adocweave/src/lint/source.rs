use crate::diagnostic::Applicability;
use crate::source::LineEnding;
use crate::source::{PositionError, TextRange, TextSize};

use super::{
    EXCESSIVE_BLANK_LINES, LINE_TOO_LONG, LintContext, LintDiagnosticBody, LintDiagnosticSink,
    TRAILING_WHITESPACE,
};

pub(super) fn lint_source_lines(
    context: &LintContext<'_>,
    sink: &mut LintDiagnosticSink<'_>,
) -> Result<(), PositionError> {
    let source_document = context.source_document();
    let max_consecutive_blank_lines = sink.config().max_consecutive_blank_lines;
    let max_line_length = sink.config().max_line_length;
    let mut blank_count = 0;

    for line in source_document.lines() {
        if sink.should_stop() {
            break;
        }
        let content = source_document
            .text(line.content_range())
            .expect("line ranges are valid");
        let is_virtual_final_line =
            line.full_range().is_empty() && line.ending() == LineEnding::None;
        let is_blank = content.trim_matches([' ', '\t']).is_empty();

        if is_blank && !is_virtual_final_line {
            blank_count += 1;
            if blank_count > max_consecutive_blank_lines {
                sink.emit(EXCESSIVE_BLANK_LINES, line.full_range(), || {
                    LintDiagnosticBody::new("excessive blank line").with_edit_fix(
                        "remove excessive blank line",
                        line.full_range(),
                        "",
                        Applicability::Always,
                    )
                });
            }
        } else {
            blank_count = 0;
        }

        let trimmed_end = content.trim_end_matches([' ', '\t']);
        if trimmed_end.len() != content.len() {
            let range = text_range(
                line.content_range().start().to_usize() + trimmed_end.len(),
                line.content_range().end().to_usize(),
            )?;
            sink.emit(TRAILING_WHITESPACE, range, || {
                LintDiagnosticBody::new("trailing whitespace").with_edit_fix(
                    "remove trailing whitespace",
                    range,
                    "",
                    Applicability::Always,
                )
            });
        }

        let character_count = content.chars().count();
        if character_count > max_line_length {
            let overflow_start = content
                .char_indices()
                .nth(max_line_length)
                .map(|(offset, _)| offset)
                .expect("line is longer than configured maximum");
            let range = text_range(
                line.content_range().start().to_usize() + overflow_start,
                line.content_range().end().to_usize(),
            )?;
            sink.emit(LINE_TOO_LONG, range, || {
                LintDiagnosticBody::new(format!(
                    "line has {character_count} characters; maximum is {max_line_length}"
                ))
            });
        }
    }

    Ok(())
}

fn text_range(start: usize, end: usize) -> Result<TextRange, PositionError> {
    TextRange::new(TextSize::new(start)?, TextSize::new(end)?)
}
