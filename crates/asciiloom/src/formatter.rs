//! Conservative, CST-aware source formatting.

use crate::diagnostic::{Applicability, Fix, TextEdit};
use crate::parser::{CstBlockKind, parse};
use crate::source::{PositionError, TextRange, TextSize};
use crate::source_lines::{LineEnding, SourceLines};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NewlineStyle {
    Lf,
    CrLf,
}

impl NewlineStyle {
    const fn text(self) -> &'static str {
        match self {
            Self::Lf => "\n",
            Self::CrLf => "\r\n",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FormatConfig {
    pub newline: NewlineStyle,
    pub final_newline: bool,
    pub max_consecutive_blank_lines: usize,
}

impl Default for FormatConfig {
    fn default() -> Self {
        Self {
            newline: NewlineStyle::Lf,
            final_newline: true,
            max_consecutive_blank_lines: 1,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FormatOutput {
    pub formatted: String,
    pub edits: Vec<TextEdit>,
}

impl FormatOutput {
    pub fn changed(&self) -> bool {
        !self.edits.is_empty()
    }
}

pub fn format(source: &str, config: &FormatConfig) -> Result<FormatOutput, PositionError> {
    let parsed = parse(source)?;
    let source_lines = SourceLines::new(source)?;
    let protected = parsed
        .cst
        .blocks()
        .iter()
        .filter(|block| {
            !matches!(
                block.kind,
                CstBlockKind::Paragraph | CstBlockKind::BlankLine
            )
        })
        .map(|block| block.range)
        .collect::<Vec<_>>();
    let last_real_line = source_lines
        .lines()
        .iter()
        .rposition(|line| !line.full_range().is_empty());
    let mut edits = Vec::new();
    let mut blank_count = 0;

    for (index, line) in source_lines.lines().iter().enumerate() {
        if protected
            .iter()
            .any(|range| ranges_overlap(*range, line.full_range()))
        {
            blank_count = 0;
            continue;
        }

        let content = source_lines
            .text(line.content_range())
            .expect("line ranges are valid");
        let virtual_final = line.full_range().is_empty() && line.ending() == LineEnding::None;
        let blank = content.trim_matches([' ', '\t']).is_empty();

        if blank && !virtual_final {
            blank_count += 1;
            if blank_count > config.max_consecutive_blank_lines {
                edits.push(TextEdit {
                    range: line.full_range(),
                    replacement: String::new(),
                });
                continue;
            }
        } else {
            blank_count = 0;
        }

        let trimmed = content.trim_end_matches([' ', '\t']);
        if trimmed.len() != content.len() {
            edits.push(TextEdit {
                range: text_range(
                    line.content_range().start().to_usize() + trimmed.len(),
                    line.content_range().end().to_usize(),
                )?,
                replacement: String::new(),
            });
        }

        if line.ending() != LineEnding::None {
            let replacement = if Some(index) == last_real_line && !config.final_newline {
                ""
            } else {
                config.newline.text()
            };
            let current = source_lines
                .text(line.ending_range())
                .expect("line ending range is valid");
            if current != replacement {
                edits.push(TextEdit {
                    range: line.ending_range(),
                    replacement: replacement.to_owned(),
                });
            }
        }
    }

    if config.final_newline
        && !source.is_empty()
        && !source.ends_with('\n')
        && last_real_line.is_some_and(|index| {
            let line = source_lines.lines()[index];
            !protected
                .iter()
                .any(|range| ranges_overlap(*range, line.full_range()))
        })
    {
        edits.push(TextEdit {
            range: text_range(source.len(), source.len())?,
            replacement: config.newline.text().to_owned(),
        });
    }

    let fix = Fix::new("format document", Applicability::Always, edits)
        .expect("formatter emits non-overlapping edits");
    let edits = fix.edits().to_vec();
    let formatted = apply_edits(source, &edits);
    Ok(FormatOutput { formatted, edits })
}

fn apply_edits(source: &str, edits: &[TextEdit]) -> String {
    let mut output = source.to_owned();
    for edit in edits.iter().rev() {
        output.replace_range(
            edit.range.start().to_usize()..edit.range.end().to_usize(),
            &edit.replacement,
        );
    }
    output
}

fn ranges_overlap(left: TextRange, right: TextRange) -> bool {
    left.start() < right.end() && right.start() < left.end()
}

fn text_range(start: usize, end: usize) -> Result<TextRange, PositionError> {
    TextRange::new(TextSize::new(start)?, TextSize::new(end)?)
}

#[cfg(test)]
mod tests {
    use super::{FormatConfig, NewlineStyle, format};
    use crate::parser::{AstBlock, parse};

    fn semantic_text(source: &str) -> Vec<Vec<String>> {
        parse(source)
            .expect("valid source")
            .ast
            .blocks
            .into_iter()
            .filter_map(|block| match block {
                AstBlock::Paragraph(paragraph) => {
                    Some(paragraph.lines.into_iter().map(|line| line.value).collect())
                }
                AstBlock::Heading(_) => None,
                AstBlock::Unsupported(_) => None,
            })
            .collect()
    }

    #[test]
    fn formatter_normalizes_plain_text() {
        let source = include_str!("../../../fixtures/format/basic.adoc");
        let output = format(source, &FormatConfig::default()).expect("valid source");

        assert_eq!(
            output.formatted,
            include_str!("../../../fixtures/format/basic.formatted.adoc")
        );
        assert!(output.changed());
    }

    #[test]
    fn formatter_is_idempotent_and_preserves_semantics() {
        let source = "first  \r\nsecond\r\n\r\n\r\nlast";
        let first = format(source, &FormatConfig::default()).expect("valid source");
        let second = format(&first.formatted, &FormatConfig::default()).expect("valid source");

        assert_eq!(second.formatted, first.formatted);
        assert!(!second.changed());
        assert_eq!(semantic_text(source), semantic_text(&first.formatted));
    }

    #[test]
    fn formatter_preserves_unsupported_regions_byte_for_byte() {
        let source = "before  \r\n\n== Unsupported  \r\n\nafter  ";
        let output = format(source, &FormatConfig::default()).expect("valid source");

        assert!(output.formatted.contains("== Unsupported  \r\n"));
        assert!(output.formatted.starts_with("before\n"));
        assert!(output.formatted.ends_with("after\n"));
    }

    #[test]
    fn formatter_supports_crlf_and_no_final_newline() {
        let config = FormatConfig {
            newline: NewlineStyle::CrLf,
            final_newline: false,
            max_consecutive_blank_lines: 1,
        };
        let output = format("one\n\ntwo\n", &config).expect("valid source");

        assert_eq!(output.formatted, "one\r\n\r\ntwo");
    }
}
