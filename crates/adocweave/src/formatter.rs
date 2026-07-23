//! Conservative, CST-aware source formatting.

use crate::core::Analysis;
#[cfg(test)]
use crate::core::{ParseError, ParseOptions, analyze};
use crate::diagnostic::{Applicability, Fix, TextEdit};
use crate::source::{PositionError, TextRange, TextSize};
use crate::source_document::LineEnding;
use crate::syntax::SyntaxTree;

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

#[cfg(test)]
fn format(source: &str, config: &FormatConfig) -> Result<FormatOutput, ParseError> {
    let analysis = analyze(source, &ParseOptions::default())?;
    format_analysis(&analysis, config).map_err(ParseError::Position)
}

pub fn format_analysis(
    analysis: &Analysis,
    config: &FormatConfig,
) -> Result<FormatOutput, PositionError> {
    format_syntax(analysis.syntax(), config)
}

fn format_syntax(
    syntax: &SyntaxTree,
    config: &FormatConfig,
) -> Result<FormatOutput, PositionError> {
    let source = syntax.source();
    let source_document = syntax.source_document();
    let protected = syntax.formatting_protected_ranges();
    let last_real_line = source_document
        .lines()
        .iter()
        .rposition(|line| !line.full_range().is_empty());
    let mut edits = Vec::new();
    let mut blank_count = 0;

    for (index, line) in source_document.lines().iter().enumerate() {
        if protected
            .iter()
            .any(|range| ranges_overlap(*range, line.full_range()))
        {
            blank_count = 0;
            continue;
        }

        let content = source_document
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
        if trimmed.len() != content.len()
            && !crate::parser::trailing_whitespace_is_structural(content)
        {
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
            let current = source_document
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
            let line = source_document.lines()[index];
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
    use crate::syntax::SyntaxKind;

    fn semantic_text(source: &str) -> Vec<Vec<String>> {
        parse(source)
            .expect("valid source")
            .ast
            .blocks()
            .iter()
            .filter_map(|block| match block {
                AstBlock::Paragraph(paragraph) => Some(
                    paragraph
                        .value
                        .lines()
                        .map(|line| line.trim_end_matches([' ', '\t']).to_owned())
                        .collect(),
                ),
                AstBlock::Heading(_) => None,
                AstBlock::LiteralParagraph(_) | AstBlock::Break(_) => None,
                AstBlock::Source(_) => None,
                AstBlock::Verbatim(_) => None,
                AstBlock::List(_) => None,
                AstBlock::Math(_) => None,
                AstBlock::Delimited(_) => None,
                AstBlock::Unsupported(_) => None,
            })
            .collect()
    }

    #[test]
    fn syntax_kinds_explicitly_identify_byte_protected_subtrees() {
        assert!(!SyntaxKind::Paragraph.protects_formatting());
        assert!(!SyntaxKind::BlankLine.protects_formatting());
        for kind in [
            SyntaxKind::DocumentTitle,
            SyntaxKind::Heading,
            SyntaxKind::MalformedHeading,
            SyntaxKind::LiteralBlock,
            SyntaxKind::SourceBlock,
            SyntaxKind::Unsupported,
            SyntaxKind::DocumentAttribute,
            SyntaxKind::BlockAnchor,
            SyntaxKind::List,
            SyntaxKind::MathBlock,
        ] {
            assert!(kind.protects_formatting());
        }
        assert!(SyntaxKind::InlineSpan.protects_formatting());
        assert!(SyntaxKind::Macro.protects_formatting());
        assert!(SyntaxKind::Error.protects_formatting());
        assert!(SyntaxKind::Unknown.protects_formatting());
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
    fn formatter_preserves_monospace_content_byte_for_byte() {
        let source = "before `  <tag>  ` after  ";
        let output = format(source, &FormatConfig::default()).expect("valid source");

        assert_eq!(output.formatted, "before `  <tag>  ` after\n");
    }

    #[test]
    fn formatter_preserves_line_boundaries_inside_multiline_spans() {
        let source = "before *strong\r\n日本語* and ``mono\r\ncode``";
        let formatted = format(
            source,
            &FormatConfig {
                newline: NewlineStyle::Lf,
                final_newline: false,
                ..FormatConfig::default()
            },
        )
        .expect("format");

        assert_eq!(formatted.formatted, source);
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
    fn formatter_preserves_invalid_explicit_ordered_number_markers() {
        let source = "4294967296. overflow\n0. zero\n";
        let output = format(source, &FormatConfig::default()).expect("valid source");

        assert_eq!(output.formatted, source);
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

    #[test]
    fn literal_block_formatter_preserves_body_byte_for_byte() {
        let source = "before  \n\n....\r\ncode  \r\n\r\n....\r\n\nafter  ";
        let output = format(source, &FormatConfig::default()).expect("valid source");

        assert!(output.formatted.contains("....\r\ncode  \r\n\r\n....\r\n"));
        assert!(output.formatted.starts_with("before\n"));
        assert!(output.formatted.ends_with("after\n"));
    }

    #[test]
    fn formatter_preserves_links_and_cross_references_without_a_resolver() {
        let source = "[[target]]  \n== Target  \n\nhttps://example.com[label] <<target,Here>>  ";
        let output = format(source, &FormatConfig::default()).expect("format");
        let after = parse(&output.formatted).expect("parse formatted");

        assert!(output.formatted.contains("https://example.com[label]"));
        assert!(output.formatted.contains("<<target,Here>>"));
        assert_eq!(
            after
                .ast
                .blocks()
                .iter()
                .flat_map(block_inlines)
                .filter(|inline| matches!(inline, crate::inline::Inline::Reference(_)))
                .count(),
            1
        );
    }

    #[test]
    fn formatter_preserves_stem_contents_byte_for_byte() {
        let source = "stem:[{x} * y < z]  \n\n[stem]\n++++\n  {x} * y < z  \n++++\n";
        let formatted = format(source, &FormatConfig::default()).expect("format");

        assert!(formatted.formatted.contains("stem:[{x} * y < z]"));
        assert!(formatted.formatted.contains("  {x} * y < z  \n"));
        let reparsed = parse(&formatted.formatted).expect("parse formatted");
        assert!(matches!(reparsed.ast.blocks()[1], AstBlock::Math(_)));
    }

    #[test]
    fn formatter_preserves_quoted_and_asciidoc_table_cells_byte_for_byte() {
        let source = "[format=csv]\n|===\na,\"one,  two\"\n|===\n\n[cols=a]\n|===\n|paragraph  \n\n* item\n|===\n";
        let formatted = format(source, &FormatConfig::default()).expect("format");
        assert_eq!(formatted.formatted, source);
    }

    #[test]
    fn formatter_preserves_block_metadata_byte_for_byte() {
        let source = ".Visible title  \n[#item.custom%collapsible,kind=\"demo\"]\nParagraph  ";
        let formatted = format(source, &FormatConfig::default()).expect("format");

        assert!(
            formatted
                .formatted
                .starts_with(".Visible title  \n[#item.custom%collapsible,kind=\"demo\"]\n")
        );
        assert!(formatted.formatted.ends_with("Paragraph\n"));
    }

    fn block_inlines(block: &AstBlock) -> Vec<&crate::inline::Inline> {
        match block {
            AstBlock::Heading(heading) => heading.inlines.iter().collect(),
            AstBlock::Paragraph(paragraph) => paragraph.inlines.iter().collect(),
            _ => Vec::new(),
        }
    }
}
