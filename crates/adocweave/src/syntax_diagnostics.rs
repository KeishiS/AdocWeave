//! Projects parser facts into syntax diagnostics.

use crate::attributes::{AttributeProblem, AttributeProblemKind};
use crate::inline::{InlineProblem, InlineProblemKind};
use crate::parser::{
    AstBlock, BlockProblem, BlockProblemKind, DelimitedBlockKind, DelimitedContent, HeadingProblem,
    ListBlock, ListProblemKind, MathProblemKind,
};
use crate::source::TextRange;
use crate::syntax::{SyntaxFix, SyntaxIssue, SyntaxIssueClass};

pub(crate) fn collect_and_clear(
    blocks: &mut [AstBlock],
    attribute_problems: &[AttributeProblem],
) -> Vec<SyntaxIssue> {
    let mut output = Vec::new();
    for problem in attribute_problems {
        let message = match problem.kind {
            AttributeProblemKind::InvalidName => "invalid document attribute name",
            AttributeProblemKind::InvalidValue => "invalid document attribute value",
        };
        output.push(issue(
            SyntaxIssueClass::InvalidAttribute,
            problem.range,
            message,
        ));
    }
    for block in blocks {
        block_issues(block, &mut output);
    }
    output
}

fn issue(class: SyntaxIssueClass, range: TextRange, message: &'static str) -> SyntaxIssue {
    SyntaxIssue {
        class,
        range,
        message,
        fix: None,
    }
}

fn inline_issues(problems: &mut Vec<InlineProblem>, output: &mut Vec<SyntaxIssue>) {
    for problem in std::mem::take(problems) {
        let (class, message) = match problem.kind {
            InlineProblemKind::UnclosedMonospace => {
                (SyntaxIssueClass::UnclosedInline, "unclosed monospace span")
            }
            InlineProblemKind::UnclosedStrong => {
                (SyntaxIssueClass::UnclosedInline, "unclosed strong span")
            }
            InlineProblemKind::UnclosedEmphasis => {
                (SyntaxIssueClass::UnclosedInline, "unclosed emphasis span")
            }
            InlineProblemKind::UnclosedHighlight => {
                (SyntaxIssueClass::UnclosedInline, "unclosed highlight span")
            }
            InlineProblemKind::UnclosedSubscript => {
                (SyntaxIssueClass::UnclosedInline, "unclosed subscript span")
            }
            InlineProblemKind::UnclosedSuperscript => (
                SyntaxIssueClass::UnclosedInline,
                "unclosed superscript span",
            ),
            InlineProblemKind::NestingLimitExceeded => (
                SyntaxIssueClass::NestingLimitExceeded,
                "inline nesting limit exceeded",
            ),
            InlineProblemKind::UnclosedAttributeReference => (
                SyntaxIssueClass::UnclosedInline,
                "unclosed attribute reference",
            ),
            InlineProblemKind::IncompleteLink => {
                (SyntaxIssueClass::InvalidUrl, "incomplete link macro")
            }
            InlineProblemKind::UnclosedPassthrough => (
                SyntaxIssueClass::UnclosedInline,
                "unclosed inline passthrough",
            ),
            InlineProblemKind::IncompleteCrossReference
            | InlineProblemKind::InvalidCrossReference => (
                SyntaxIssueClass::InvalidCrossReference,
                "incomplete or invalid cross reference",
            ),
            InlineProblemKind::UnclosedStem => {
                (SyntaxIssueClass::InvalidStem, "unclosed inline STEM")
            }
            InlineProblemKind::EmptyStem => (SyntaxIssueClass::InvalidStem, "inline STEM is empty"),
            InlineProblemKind::StemSizeLimitExceeded => (
                SyntaxIssueClass::InvalidStem,
                "inline STEM exceeds the size limit",
            ),
        };
        output.push(issue(class, problem.range, message));
    }
}

fn block_issues(block: &mut AstBlock, output: &mut Vec<SyntaxIssue>) {
    match block {
        AstBlock::Heading(heading) => {
            inline_issues(&mut heading.inline_problems, output);
            for problem in std::mem::take(&mut heading.problems) {
                match problem {
                    HeadingProblem::MissingSpace => {
                        let range =
                            TextRange::new(heading.marker_range.end(), heading.marker_range.end())
                                .expect("empty insertion range is ordered");
                        output.push(SyntaxIssue {
                            class: SyntaxIssueClass::HeadingMarkerSpace,
                            range,
                            message: "heading marker must be followed by a space",
                            fix: Some(SyntaxFix {
                                label: "insert a space after heading marker",
                                range,
                                replacement: " ",
                            }),
                        });
                    }
                    HeadingProblem::LevelTooDeep | HeadingProblem::MisplacedDocumentTitle => {
                        output.push(issue(
                            SyntaxIssueClass::InvalidHeadingLevel,
                            heading.marker_range,
                            "invalid heading level or document title position",
                        ));
                    }
                    HeadingProblem::EmptyText => {}
                }
            }
        }
        AstBlock::Paragraph(paragraph) => inline_issues(&mut paragraph.inline_problems, output),
        AstBlock::LiteralParagraph(_) | AstBlock::Break(_) => {}
        AstBlock::Literal(block) => block_problem_issues(&mut block.problems, "literal", output),
        AstBlock::Source(block) => block_problem_issues(&mut block.problems, "source", output),
        AstBlock::List(list) => list_issues(list, output),
        AstBlock::Math(math) => {
            for problem in std::mem::take(&mut math.problems) {
                let message = match problem.kind {
                    MathProblemKind::Unclosed => "unclosed STEM block",
                    MathProblemKind::Empty => "STEM block is empty",
                    MathProblemKind::SizeLimitExceeded => "STEM block exceeds the size limit",
                };
                output.push(issue(SyntaxIssueClass::InvalidStem, problem.range, message));
            }
        }
        AstBlock::Delimited(block) => {
            let block_name = if block.kind == DelimitedBlockKind::Literal {
                "literal"
            } else {
                "delimited"
            };
            block_problem_issues(&mut block.problems, block_name, output);
            match &mut block.content {
                DelimitedContent::Compound(children) => {
                    for child in children {
                        block_issues(child, output);
                    }
                }
                DelimitedContent::Table(table) => {
                    for row in &mut table.rows {
                        for cell in &mut row.cells {
                            if let crate::table::TableCellContent::AsciiDoc(children) =
                                &mut cell.content
                            {
                                for child in children {
                                    block_issues(child, output);
                                }
                            }
                        }
                    }
                }
                DelimitedContent::Verbatim(_) | DelimitedContent::Passthrough(_) => {}
            }
        }
        AstBlock::Unsupported(_) => {}
    }
}

fn block_problem_issues(
    problems: &mut Vec<BlockProblem>,
    block_name: &'static str,
    output: &mut Vec<SyntaxIssue>,
) {
    for problem in std::mem::take(problems) {
        let (class, message) = match (problem.kind, block_name) {
            (BlockProblemKind::UnclosedBlock, "literal") => {
                (SyntaxIssueClass::UnclosedBlock, "unclosed literal block")
            }
            (BlockProblemKind::UnclosedBlock, "source") => {
                (SyntaxIssueClass::UnclosedBlock, "unclosed source block")
            }
            (BlockProblemKind::UnclosedBlock, _) => {
                (SyntaxIssueClass::UnclosedBlock, "unclosed delimited block")
            }
            (BlockProblemKind::MissingSourceLanguage, _) => (
                SyntaxIssueClass::MissingSourceLanguage,
                "source block requires a language",
            ),
        };
        output.push(issue(class, problem.range, message));
    }
}

fn list_issues(list: &mut ListBlock, output: &mut Vec<SyntaxIssue>) {
    for item in &mut list.items {
        for term in &mut item.terms {
            inline_issues(&mut term.inline_problems, output);
        }
        inline_issues(&mut item.inline_problems, output);
        for problem in std::mem::take(&mut item.problems) {
            let (message, fix) = match problem.kind {
                ListProblemKind::EmptyItem => ("list item is empty", None),
                ListProblemKind::InconsistentMarker => {
                    ("list marker kind changes at the same depth", None)
                }
                ListProblemKind::InvalidNesting => ("list nesting skips a depth", None),
                ListProblemKind::DepthLimitExceeded => {
                    ("list nesting exceeds the configured limit", None)
                }
                ListProblemKind::NonCanonicalSeparator => (
                    "list marker must be followed by one space",
                    Some(SyntaxFix {
                        label: "replace the separator with a space",
                        range: problem.range,
                        replacement: " ",
                    }),
                ),
            };
            output.push(SyntaxIssue {
                class: SyntaxIssueClass::InconsistentList,
                range: problem.range,
                message,
                fix,
            });
        }
        for child in &mut item.children {
            list_issues(child, output);
        }
        for continuation in &mut item.continuations {
            block_issues(continuation, output);
        }
    }
}
