//! Projects parser facts into syntax diagnostics.

use crate::attributes::{AttributeProblem, AttributeProblemKind};
use crate::inline::{InlineProblem, InlineProblemKind};
use crate::parser::{
    AstBlock, BlockProblem, BlockProblemKind, DelimitedBlockKind, HeadingProblem, ListBlock,
    ListProblemKind, MathProblemKind,
};
use crate::source::TextRange;
use crate::syntax::{SyntaxFix, SyntaxIssue, SyntaxIssueClass};

pub(crate) fn collect_and_clear_cancellable(
    blocks: &mut [AstBlock],
    attribute_problems: &[AttributeProblem],
    checkpoint: &mut crate::cancellation::CancellationCheckpoint<'_>,
) -> Result<Vec<SyntaxIssue>, ()> {
    let mut output = Vec::new();
    for problem in attribute_problems {
        if checkpoint.is_cancelled() {
            return Err(());
        }
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
    struct IssueCollector<'output, 'cancellation> {
        output: &'output mut Vec<SyntaxIssue>,
        checkpoint: crate::cancellation::CancellationCheckpoint<'cancellation>,
        cancelled: bool,
    }
    impl crate::walker::BlockVisitorMut for IssueCollector<'_, '_> {
        fn visit_block(&mut self, block: &mut AstBlock) {
            if self.cancelled {
                return;
            }
            self.cancelled = block_issues(block, self.output, &mut self.checkpoint).is_err();
        }

        fn visit_list(&mut self, list: &mut ListBlock) {
            if self.cancelled {
                return;
            }
            self.cancelled = list_issues(list, self.output, &mut self.checkpoint).is_err();
        }
    }
    let cancellation = checkpoint.cancellation();
    let mut collector = IssueCollector {
        output: &mut output,
        checkpoint: crate::cancellation::CancellationCheckpoint::new(cancellation),
        cancelled: false,
    };
    crate::walker::walk_blocks_mut_cancellable(blocks, &mut collector, checkpoint)?;
    if collector.cancelled {
        return Err(());
    }
    Ok(output)
}

fn issue(class: SyntaxIssueClass, range: TextRange, message: &'static str) -> SyntaxIssue {
    SyntaxIssue {
        class,
        range,
        message,
        detail: crate::syntax::SyntaxIssueDetail::None,
        fix: None,
    }
}

fn inline_issues(
    problems: &mut Vec<InlineProblem>,
    output: &mut Vec<SyntaxIssue>,
    checkpoint: &mut crate::cancellation::CancellationCheckpoint<'_>,
) -> Result<(), ()> {
    for problem in std::mem::take(problems) {
        if checkpoint.is_cancelled() {
            return Err(());
        }
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
            InlineProblemKind::MacroBoundary { name } => {
                output.push(SyntaxIssue {
                    class: SyntaxIssueClass::MacroBoundary,
                    range: problem.range,
                    message: "inline macro must start at a token boundary",
                    detail: crate::syntax::SyntaxIssueDetail::MacroBoundary { name },
                    fix: None,
                });
                continue;
            }
        };
        output.push(issue(class, problem.range, message));
    }
    Ok(())
}

fn block_issues(
    block: &mut AstBlock,
    output: &mut Vec<SyntaxIssue>,
    checkpoint: &mut crate::cancellation::CancellationCheckpoint<'_>,
) -> Result<(), ()> {
    if let Some(title) = &mut block.metadata_mut().title {
        inline_issues(&mut title.inline_problems, output, checkpoint)?;
    }
    match block {
        AstBlock::Heading(heading) => {
            inline_issues(&mut heading.inline_problems, output, checkpoint)?;
            for problem in std::mem::take(&mut heading.problems) {
                if checkpoint.is_cancelled() {
                    return Err(());
                }
                match problem {
                    HeadingProblem::MissingSpace => {
                        let range =
                            TextRange::new(heading.marker_range.end(), heading.marker_range.end())
                                .expect("empty insertion range is ordered");
                        output.push(SyntaxIssue {
                            class: SyntaxIssueClass::HeadingMarkerSpace,
                            range,
                            message: "heading marker must be followed by a space",
                            detail: crate::syntax::SyntaxIssueDetail::None,
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
        AstBlock::Paragraph(paragraph) => {
            inline_issues(&mut paragraph.inline_problems, output, checkpoint)?;
        }
        AstBlock::LiteralParagraph(_) | AstBlock::Break(_) => {}
        AstBlock::Source(block) => {
            block_problem_issues(&mut block.problems, "source", output, checkpoint)?;
        }
        AstBlock::Verbatim(block) => {
            let name = match block.kind {
                crate::parser::VerbatimKind::Literal => "literal",
                crate::parser::VerbatimKind::Listing => "listing",
                crate::parser::VerbatimKind::Source(_) => "source",
            };
            block_problem_issues(&mut block.problems, name, output, checkpoint)?;
        }
        AstBlock::List(_) => {}
        AstBlock::Math(math) => {
            for problem in std::mem::take(&mut math.problems) {
                if checkpoint.is_cancelled() {
                    return Err(());
                }
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
            block_problem_issues(&mut block.problems, block_name, output, checkpoint)?;
        }
        AstBlock::Unsupported(block) => {
            if block.reason == "invalid source block attribute" {
                output.push(issue(
                    SyntaxIssueClass::InvalidAttribute,
                    block.range,
                    "unsupported source block option",
                ));
            }
        }
    }
    Ok(())
}

fn block_problem_issues(
    problems: &mut Vec<BlockProblem>,
    block_name: &'static str,
    output: &mut Vec<SyntaxIssue>,
    checkpoint: &mut crate::cancellation::CancellationCheckpoint<'_>,
) -> Result<(), ()> {
    for problem in std::mem::take(problems) {
        if checkpoint.is_cancelled() {
            return Err(());
        }
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
            (BlockProblemKind::InvalidSourceOption, _) => (
                SyntaxIssueClass::InvalidAttribute,
                "unsupported source block option",
            ),
            (BlockProblemKind::InvalidSourceStart, _) => (
                SyntaxIssueClass::InvalidAttribute,
                "source block start must be a positive integer with linenums",
            ),
        };
        output.push(issue(class, problem.range, message));
    }
    Ok(())
}

fn list_issues(
    list: &mut ListBlock,
    output: &mut Vec<SyntaxIssue>,
    checkpoint: &mut crate::cancellation::CancellationCheckpoint<'_>,
) -> Result<(), ()> {
    for item in &mut list.items {
        if checkpoint.is_cancelled() {
            return Err(());
        }
        for term in &mut item.terms {
            inline_issues(&mut term.inline_problems, output, checkpoint)?;
        }
        inline_issues(&mut item.inline_problems, output, checkpoint)?;
        for problem in std::mem::take(&mut item.problems) {
            if checkpoint.is_cancelled() {
                return Err(());
            }
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
                detail: crate::syntax::SyntaxIssueDetail::None,
                fix,
            });
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::inline::InlineProblemKind as Inline;
    use crate::parser::BlockProblemKind as Block;

    fn checkpoint() -> crate::cancellation::CancellationCheckpoint<'static> {
        crate::cancellation::CancellationCheckpoint::new(&crate::core::NeverCancel)
    }

    fn at(start: u32) -> TextRange {
        TextRange::new(
            crate::source::TextSize::new(start as usize).expect("start"),
            crate::source::TextSize::new(start as usize + 1).expect("end"),
        )
        .expect("range")
    }

    fn inline(kind: Inline) -> Vec<InlineProblem> {
        vec![InlineProblem { kind, range: at(0) }]
    }

    fn empty_list() -> ListBlock {
        ListBlock {
            metadata: crate::block_model::BlockMetadata::default(),
            kind: crate::block_model::ListKind::Unordered,
            presentation: crate::block_model::OrderedListPresentation::default(),
            presentation_problems: Vec::new(),
            range: at(0),
            items: Vec::new(),
        }
    }

    fn list_item() -> crate::block_model::ListItem {
        crate::block_model::ListItem {
            range: at(0),
            marker_range: at(0),
            explicit_number: None,
            invalid_explicit_number: false,
            separator_range: at(0),
            text_range: at(0),
            text: String::new(),
            inlines: Vec::new(),
            terms: Vec::new(),
            checklist: None,
            callout_id: None,
            inline_problems: Vec::new(),
            children: Vec::new(),
            continuations: Vec::new(),
            continuation_ranges: Vec::new(),
            problems: Vec::new(),
        }
    }

    fn project_inline(kind: Inline) -> SyntaxIssue {
        let mut problems = inline(kind);
        let mut output = Vec::new();
        inline_issues(&mut problems, &mut output, &mut checkpoint()).expect("not cancelled");
        assert!(problems.is_empty(), "projected problems are consumed");
        assert_eq!(output.len(), 1);
        output.pop().expect("issue")
    }

    /// Every unclosed inline span reports one class and its own wording.
    ///
    /// The class is what a caller filters on, so the whole family sharing
    /// `UnclosedInline` is the property under test; the message names which
    /// span so a reader is not left guessing.
    #[test]
    fn unclosed_inline_spans_share_one_class_and_keep_distinct_messages() {
        let mut messages = std::collections::BTreeSet::new();
        for kind in [
            Inline::UnclosedMonospace,
            Inline::UnclosedStrong,
            Inline::UnclosedEmphasis,
            Inline::UnclosedHighlight,
            Inline::UnclosedSubscript,
            Inline::UnclosedSuperscript,
            Inline::UnclosedAttributeReference,
            Inline::UnclosedPassthrough,
        ] {
            let issue = project_inline(kind);
            assert_eq!(issue.class, SyntaxIssueClass::UnclosedInline, "{kind:?}");
            assert!(
                messages.insert(issue.message),
                "duplicate message: {}",
                issue.message
            );
        }
    }

    /// Inline problems that are not unclosed spans keep their own class.
    #[test]
    fn other_inline_problems_project_onto_their_own_class() {
        for (kind, class) in [
            (
                Inline::NestingLimitExceeded,
                SyntaxIssueClass::NestingLimitExceeded,
            ),
            (Inline::IncompleteLink, SyntaxIssueClass::InvalidUrl),
            (
                Inline::IncompleteCrossReference,
                SyntaxIssueClass::InvalidCrossReference,
            ),
            (
                Inline::InvalidCrossReference,
                SyntaxIssueClass::InvalidCrossReference,
            ),
            (Inline::UnclosedStem, SyntaxIssueClass::InvalidStem),
            (Inline::EmptyStem, SyntaxIssueClass::InvalidStem),
            (Inline::StemSizeLimitExceeded, SyntaxIssueClass::InvalidStem),
        ] {
            assert_eq!(project_inline(kind).class, class, "{kind:?}");
        }

        // The two cross-reference problems deliberately share one message.
        assert_eq!(
            project_inline(Inline::IncompleteCrossReference).message,
            project_inline(Inline::InvalidCrossReference).message,
        );
    }

    /// A macro boundary problem carries the macro name to the caller.
    ///
    /// It is the one inline problem that does not go through the shared
    /// constructor, because the name has to survive into the detail.
    #[test]
    fn a_macro_boundary_problem_carries_the_macro_name() {
        let issue = project_inline(Inline::MacroBoundary { name: "xref" });
        assert_eq!(issue.class, SyntaxIssueClass::MacroBoundary);
        assert_eq!(
            issue.detail,
            crate::syntax::SyntaxIssueDetail::MacroBoundary { name: "xref" }
        );
        assert!(issue.fix.is_none());
    }

    /// An unclosed block names the kind it came from.
    ///
    /// The parser reports one problem for every delimited block, so the block
    /// name is what turns it into wording a reader can act on.
    #[test]
    fn an_unclosed_block_is_named_after_the_block_it_came_from() {
        let message = |block_name| {
            let mut problems = vec![BlockProblem {
                kind: Block::UnclosedBlock,
                range: at(0),
            }];
            let mut output = Vec::new();
            block_problem_issues(&mut problems, block_name, &mut output, &mut checkpoint())
                .expect("not cancelled");
            assert_eq!(output.len(), 1);
            assert_eq!(output[0].class, SyntaxIssueClass::UnclosedBlock);
            output[0].message
        };

        assert_eq!(message("literal"), "unclosed literal block");
        assert_eq!(message("source"), "unclosed source block");
        // Any other delimited block falls back to the general wording.
        assert_eq!(message("example"), "unclosed delimited block");
        assert_eq!(message("quote"), "unclosed delimited block");
    }

    /// Source block problems do not depend on the block name.
    #[test]
    fn source_block_problems_keep_one_class_regardless_of_the_block_name() {
        for (kind, class) in [
            (
                Block::MissingSourceLanguage,
                SyntaxIssueClass::MissingSourceLanguage,
            ),
            (
                Block::InvalidSourceOption,
                SyntaxIssueClass::InvalidAttribute,
            ),
            (
                Block::InvalidSourceStart,
                SyntaxIssueClass::InvalidAttribute,
            ),
        ] {
            for block_name in ["source", "literal", "example"] {
                let mut problems = vec![BlockProblem { kind, range: at(0) }];
                let mut output = Vec::new();
                block_problem_issues(&mut problems, block_name, &mut output, &mut checkpoint())
                    .expect("not cancelled");
                assert_eq!(output[0].class, class, "{kind:?} in {block_name}");
            }
        }
    }

    /// A non-canonical list separator is the only list problem with a fix.
    ///
    /// The others describe structure the author has to resolve, so proposing a
    /// replacement would be guessing.
    #[test]
    fn only_a_non_canonical_list_separator_proposes_a_replacement() {
        use crate::parser::ListProblemKind as List;

        let issues = |kind| {
            let mut list = ListBlock {
                items: vec![crate::block_model::ListItem {
                    problems: vec![crate::block_model::ListProblem { kind, range: at(3) }],
                    ..list_item()
                }],
                ..empty_list()
            };
            let mut output = Vec::new();
            list_issues(&mut list, &mut output, &mut checkpoint()).expect("not cancelled");
            output
        };

        for kind in [
            List::EmptyItem,
            List::InconsistentMarker,
            List::InvalidNesting,
            List::DepthLimitExceeded,
        ] {
            let output = issues(kind);
            assert_eq!(output.len(), 1, "{kind:?}");
            assert_eq!(output[0].class, SyntaxIssueClass::InconsistentList);
            assert!(output[0].fix.is_none(), "{kind:?}");
        }

        let output = issues(List::NonCanonicalSeparator);
        let fix = output[0].fix.as_ref().expect("replacement");
        assert_eq!(fix.replacement, " ");
        assert_eq!(fix.range, at(3));
    }

    /// Cancellation stops projection and reports nothing partial.
    #[test]
    fn cancellation_stops_projection_before_the_next_problem() {
        struct AlwaysCancel;
        impl crate::core::CancellationCheck for AlwaysCancel {
            fn is_cancelled(&self) -> bool {
                true
            }
        }

        let mut problems = inline(Inline::UnclosedStrong);
        let mut output = Vec::new();
        let mut checkpoint = crate::cancellation::CancellationCheckpoint::new(&AlwaysCancel);
        assert!(inline_issues(&mut problems, &mut output, &mut checkpoint).is_err());
        assert!(output.is_empty());
    }
}
