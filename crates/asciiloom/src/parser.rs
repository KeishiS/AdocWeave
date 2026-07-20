//! Lossless concrete syntax and HTML-independent semantic syntax.

use std::fmt::Write as _;

use crate::inline::{Inline, InlineParseConfig, parse_text};
use crate::source::{PositionError, TextRange};
use crate::source_lines::{LosslessToken, SourceLine, SourceLines};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CstBlockKind {
    DocumentTitle,
    Heading,
    MalformedHeading,
    Paragraph,
    BlankLine,
    Unsupported,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CstBlock {
    pub kind: CstBlockKind,
    pub range: TextRange,
}

#[derive(Debug)]
pub struct CstDocument<'source> {
    source_lines: SourceLines<'source>,
    blocks: Vec<CstBlock>,
}

impl<'source> CstDocument<'source> {
    pub fn source(&self) -> &'source str {
        self.source_lines.source()
    }

    pub fn blocks(&self) -> &[CstBlock] {
        &self.blocks
    }

    pub fn tokens(&self) -> &[LosslessToken] {
        self.source_lines.tokens()
    }

    pub fn reconstruct(&self) -> String {
        self.source_lines.reconstruct()
    }

    pub fn snapshot(&self) -> String {
        let mut output = String::new();
        writeln!(output, "Document@0..{}", self.source().len())
            .expect("writing to a String cannot fail");
        for block in &self.blocks {
            writeln!(
                output,
                "  {:?}@{}..{}",
                block.kind,
                block.range.start().to_u32(),
                block.range.end().to_u32()
            )
            .expect("writing to a String cannot fail");
        }
        output
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TextNode {
    pub range: TextRange,
    pub value: String,
    pub inlines: Vec<Inline>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Paragraph {
    pub range: TextRange,
    pub lines: Vec<TextNode>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Unsupported {
    pub range: TextRange,
    pub raw: String,
    pub reason: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HeadingKind {
    DocumentTitle,
    Section { level: u8 },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HeadingProblem {
    MissingSpace,
    EmptyText,
    LevelTooDeep,
    MisplacedDocumentTitle,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Heading {
    pub range: TextRange,
    pub marker_range: TextRange,
    pub separator_range: TextRange,
    pub text_range: TextRange,
    pub kind: HeadingKind,
    pub text: String,
    pub inlines: Vec<Inline>,
    pub problems: Vec<HeadingProblem>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AstBlock {
    Heading(Heading),
    Paragraph(Paragraph),
    Unsupported(Unsupported),
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct AstDocument {
    pub blocks: Vec<AstBlock>,
}

impl AstDocument {
    pub fn snapshot(&self) -> String {
        let mut output = String::from("Document\n");
        for block in &self.blocks {
            match block {
                AstBlock::Heading(heading) => {
                    writeln!(
                        output,
                        "  {:?}@{}..{} marker={}..{} text={}..{} {:?} problems={:?}",
                        heading.kind,
                        heading.range.start().to_u32(),
                        heading.range.end().to_u32(),
                        heading.marker_range.start().to_u32(),
                        heading.marker_range.end().to_u32(),
                        heading.text_range.start().to_u32(),
                        heading.text_range.end().to_u32(),
                        heading.text,
                        heading.problems
                    )
                    .expect("writing to a String cannot fail");
                }
                AstBlock::Paragraph(paragraph) => {
                    writeln!(
                        output,
                        "  Paragraph@{}..{}",
                        paragraph.range.start().to_u32(),
                        paragraph.range.end().to_u32()
                    )
                    .expect("writing to a String cannot fail");
                    for line in &paragraph.lines {
                        writeln!(
                            output,
                            "    Text@{}..{} {:?}",
                            line.range.start().to_u32(),
                            line.range.end().to_u32(),
                            line.value
                        )
                        .expect("writing to a String cannot fail");
                    }
                }
                AstBlock::Unsupported(unsupported) => {
                    writeln!(
                        output,
                        "  Unsupported@{}..{} {:?} ({})",
                        unsupported.range.start().to_u32(),
                        unsupported.range.end().to_u32(),
                        unsupported.raw,
                        unsupported.reason
                    )
                    .expect("writing to a String cannot fail");
                }
            }
        }
        output
    }
}

#[derive(Debug)]
pub struct ParsedDocument<'source> {
    pub cst: CstDocument<'source>,
    pub ast: AstDocument,
}

pub fn parse(source: &str) -> Result<ParsedDocument<'_>, PositionError> {
    let source_lines = SourceLines::new(source)?;
    let mut blocks = Vec::new();
    let mut ast_blocks = Vec::new();
    let mut paragraph_lines = Vec::new();
    let mut saw_content = false;

    for line in source_lines.lines() {
        let content = source_lines
            .text(line.content_range())
            .expect("line content has valid UTF-8 boundaries");

        if content.trim_matches([' ', '\t']).is_empty() {
            flush_paragraph(&mut blocks, &mut ast_blocks, &mut paragraph_lines);
            blocks.push(CstBlock {
                kind: CstBlockKind::BlankLine,
                range: line.full_range(),
            });
        } else if content.starts_with('=') {
            flush_paragraph(&mut blocks, &mut ast_blocks, &mut paragraph_lines);
            let heading = parse_heading(content, *line, !saw_content)?;
            blocks.push(CstBlock {
                kind: if heading.problems.is_empty() {
                    match heading.kind {
                        HeadingKind::DocumentTitle => CstBlockKind::DocumentTitle,
                        HeadingKind::Section { .. } => CstBlockKind::Heading,
                    }
                } else {
                    CstBlockKind::MalformedHeading
                },
                range: line.full_range(),
            });
            ast_blocks.push(AstBlock::Heading(heading));
            saw_content = true;
        } else if unsupported_reason(content).is_some() {
            flush_paragraph(&mut blocks, &mut ast_blocks, &mut paragraph_lines);
            let reason = unsupported_reason(content).expect("checked above");
            blocks.push(CstBlock {
                kind: CstBlockKind::Unsupported,
                range: line.full_range(),
            });
            ast_blocks.push(AstBlock::Unsupported(Unsupported {
                range: line.full_range(),
                raw: content.to_owned(),
                reason: reason.to_owned(),
            }));
            saw_content = true;
        } else {
            paragraph_lines.push((*line, content.trim_end_matches([' ', '\t']).to_owned()));
            saw_content = true;
        }
    }
    flush_paragraph(&mut blocks, &mut ast_blocks, &mut paragraph_lines);

    Ok(ParsedDocument {
        cst: CstDocument {
            source_lines,
            blocks,
        },
        ast: AstDocument { blocks: ast_blocks },
    })
}

fn parse_heading(
    content: &str,
    line: SourceLine,
    document_title_position: bool,
) -> Result<Heading, PositionError> {
    let marker_len = content.bytes().take_while(|byte| *byte == b'=').count();
    let content_start = line.content_range().start().to_usize();
    let marker_range = text_range(content_start, content_start + marker_len)?;
    let has_space = content.as_bytes().get(marker_len) == Some(&b' ');
    let text_start_relative = marker_len + usize::from(has_space);
    let text = content
        .get(text_start_relative..)
        .unwrap_or_default()
        .trim_end_matches([' ', '\t']);
    let text_start = content_start + text_start_relative.min(content.len());
    let separator_range = if has_space {
        text_range(content_start + marker_len, content_start + marker_len + 1)?
    } else {
        text_range(content_start + marker_len, content_start + marker_len)?
    };
    let text_range = text_range(text_start, text_start + text.len())?;
    let mut problems = Vec::new();
    if !has_space {
        problems.push(HeadingProblem::MissingSpace);
    }
    if text.is_empty() {
        problems.push(HeadingProblem::EmptyText);
    }
    let kind = match marker_len {
        1 if document_title_position => HeadingKind::DocumentTitle,
        1 => {
            problems.push(HeadingProblem::MisplacedDocumentTitle);
            HeadingKind::DocumentTitle
        }
        2..=6 => HeadingKind::Section {
            level: (marker_len - 1) as u8,
        },
        _ => {
            problems.push(HeadingProblem::LevelTooDeep);
            HeadingKind::Section { level: 6 }
        }
    };

    Ok(Heading {
        range: line.full_range(),
        marker_range,
        separator_range,
        text_range,
        kind,
        text: text.to_owned(),
        inlines: parse_text(text, text_range, InlineParseConfig::default()),
        problems,
    })
}

fn flush_paragraph(
    cst_blocks: &mut Vec<CstBlock>,
    ast_blocks: &mut Vec<AstBlock>,
    lines: &mut Vec<(SourceLine, String)>,
) {
    let (Some((first, _)), Some((last, _))) = (lines.first(), lines.last()) else {
        return;
    };
    let range = TextRange::new(first.full_range().start(), last.full_range().end())
        .expect("ordered source lines form an ordered paragraph");
    cst_blocks.push(CstBlock {
        kind: CstBlockKind::Paragraph,
        range,
    });
    ast_blocks.push(AstBlock::Paragraph(Paragraph {
        range,
        lines: lines
            .drain(..)
            .map(|(line, value)| {
                let value_range = TextRange::new(
                    line.content_range().start(),
                    crate::source::TextSize::new(
                        line.content_range().start().to_usize() + value.len(),
                    )
                    .expect("source offset fits"),
                )
                .expect("trimmed text range is ordered");
                TextNode {
                    range: value_range,
                    inlines: parse_text(&value, value_range, InlineParseConfig::default()),
                    value,
                }
            })
            .collect(),
    }));
}

fn unsupported_reason(content: &str) -> Option<&'static str> {
    let trimmed = content.trim_start_matches([' ', '\t']);
    if trimmed.starts_with('[') {
        Some("block attributes are not implemented")
    } else if is_delimiter(trimmed) {
        Some("delimited blocks are not implemented")
    } else if trimmed.starts_with("* ") || trimmed.starts_with(". ") {
        Some("list syntax is not implemented")
    } else {
        None
    }
}

fn text_range(start: usize, end: usize) -> Result<TextRange, PositionError> {
    TextRange::new(
        crate::source::TextSize::new(start)?,
        crate::source::TextSize::new(end)?,
    )
}

fn is_delimiter(text: &str) -> bool {
    let mut characters = text.chars();
    let Some(first) = characters.next() else {
        return false;
    };
    matches!(first, '-' | '.' | '=' | '_')
        && text.chars().count() >= 4
        && characters.all(|character| character == first)
}

#[cfg(test)]
mod tests {
    use super::{AstBlock, CstBlockKind, HeadingKind, HeadingProblem, parse};

    #[test]
    fn paragraph_parser_handles_empty_input() {
        let parsed = parse("").expect("valid source");

        assert!(parsed.ast.blocks.is_empty());
        assert_eq!(parsed.cst.blocks().len(), 1);
        assert_eq!(parsed.cst.blocks()[0].kind, CstBlockKind::BlankLine);
        assert_eq!(parsed.cst.reconstruct(), "");
    }

    #[test]
    fn paragraph_parser_groups_lines_and_splits_on_blank_lines() {
        let source = "\nfirst line\nsecond line\n \t\nlast";
        let parsed = parse(source).expect("valid source");

        assert_eq!(parsed.ast.blocks.len(), 2);
        let AstBlock::Paragraph(first) = &parsed.ast.blocks[0] else {
            panic!("expected paragraph");
        };
        assert_eq!(
            first
                .lines
                .iter()
                .map(|line| line.value.as_str())
                .collect::<Vec<_>>(),
            ["first line", "second line"]
        );
        assert_eq!(parsed.cst.reconstruct().as_bytes(), source.as_bytes());
    }

    #[test]
    fn paragraph_parser_keeps_unsupported_syntax_explicit() {
        let source = "before\n\n[role=test]\n\nafter";
        let parsed = parse(source).expect("valid source");

        assert_eq!(parsed.ast.blocks.len(), 3);
        let AstBlock::Unsupported(unsupported) = &parsed.ast.blocks[1] else {
            panic!("expected unsupported node");
        };
        assert_eq!(unsupported.raw, "[role=test]");
        assert_eq!(unsupported.reason, "block attributes are not implemented");
        assert_eq!(parsed.cst.reconstruct(), source);
    }

    #[test]
    fn heading_parser_distinguishes_title_and_levels_one_to_five() {
        let source = "= Title\n\n== One\n=== Two\n==== Three\n===== Four\n====== Five\n";
        let parsed = parse(source).expect("valid source");
        let headings = parsed
            .ast
            .blocks
            .iter()
            .filter_map(|block| match block {
                AstBlock::Heading(heading) => Some(heading),
                _ => None,
            })
            .collect::<Vec<_>>();

        assert_eq!(headings.len(), 6);
        assert_eq!(headings[0].kind, HeadingKind::DocumentTitle);
        for (index, heading) in headings[1..].iter().enumerate() {
            assert_eq!(
                heading.kind,
                HeadingKind::Section {
                    level: (index + 1) as u8,
                }
            );
            assert!(heading.problems.is_empty());
        }
    }

    #[test]
    fn heading_parser_keeps_marker_separator_and_text_ranges() {
        let parsed = parse("== 日本語").expect("valid source");
        let AstBlock::Heading(heading) = &parsed.ast.blocks[0] else {
            panic!("expected heading");
        };

        assert_eq!(heading.marker_range.start().to_u32(), 0);
        assert_eq!(heading.marker_range.end().to_u32(), 2);
        assert_eq!(heading.separator_range.start().to_u32(), 2);
        assert_eq!(heading.separator_range.end().to_u32(), 3);
        assert_eq!(heading.text_range.start().to_u32(), 3);
        assert_eq!(heading.text_range.end().to_u32(), 12);
    }

    #[test]
    fn heading_parser_preserves_malformed_headings_and_recovers() {
        let parsed = parse("==Missing\n\n======= Too deep\n\nafter").expect("valid source");
        let AstBlock::Heading(first) = &parsed.ast.blocks[0] else {
            panic!("expected malformed heading");
        };
        assert!(first.problems.contains(&HeadingProblem::MissingSpace));
        let AstBlock::Heading(second) = &parsed.ast.blocks[1] else {
            panic!("expected malformed heading");
        };
        assert!(second.problems.contains(&HeadingProblem::LevelTooDeep));
        assert!(matches!(parsed.ast.blocks[2], AstBlock::Paragraph(_)));
    }

    #[test]
    fn paragraph_parser_matches_cst_and_ast_fixtures() {
        let source = include_str!("../../../fixtures/paragraph/basic.adoc");
        let parsed = parse(source).expect("valid source");

        assert_eq!(
            parsed.cst.snapshot(),
            include_str!("../../../fixtures/paragraph/basic.cst")
        );
        assert_eq!(
            parsed.ast.snapshot(),
            include_str!("../../../fixtures/paragraph/basic.ast")
        );
    }
}
