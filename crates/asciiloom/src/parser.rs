//! Lossless concrete syntax and HTML-independent semantic syntax.

use std::fmt::Write as _;

use crate::source::{PositionError, TextRange};
use crate::source_lines::{LosslessToken, SourceLine, SourceLines};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CstBlockKind {
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

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AstBlock {
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
        } else {
            paragraph_lines.push((*line, content.to_owned()));
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
            .map(|(line, value)| TextNode {
                range: line.content_range(),
                value,
            })
            .collect(),
    }));
}

fn unsupported_reason(content: &str) -> Option<&'static str> {
    let trimmed = content.trim_start_matches([' ', '\t']);
    if trimmed.starts_with("= ") || trimmed.starts_with("==") {
        Some("heading syntax is not implemented")
    } else if trimmed.starts_with('[') {
        Some("block attributes are not implemented")
    } else if is_delimiter(trimmed) {
        Some("delimited blocks are not implemented")
    } else if trimmed.starts_with("* ") || trimmed.starts_with(". ") {
        Some("list syntax is not implemented")
    } else {
        None
    }
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
    use super::{AstBlock, CstBlockKind, parse};

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
        let source = "before\n\n== Heading\n\nafter";
        let parsed = parse(source).expect("valid source");

        assert_eq!(parsed.ast.blocks.len(), 3);
        let AstBlock::Unsupported(unsupported) = &parsed.ast.blocks[1] else {
            panic!("expected unsupported node");
        };
        assert_eq!(unsupported.raw, "== Heading");
        assert_eq!(unsupported.reason, "heading syntax is not implemented");
        assert_eq!(parsed.cst.reconstruct(), source);
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
