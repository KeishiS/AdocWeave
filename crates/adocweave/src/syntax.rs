//! Lossless concrete syntax tree over one [`SourceDocument`].

use std::fmt::Write as _;

use crate::source::{LosslessToken, LosslessTokenKind, SourceDocument};
use crate::source::{TextRange, TextSize};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct SyntaxInvariantError;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SyntaxKind {
    Document,
    DocumentTitle,
    AuthorLine,
    RevisionLine,
    Heading,
    MalformedHeading,
    Paragraph,
    ThematicBreak,
    PageBreak,
    LiteralBlock,
    SourceBlock,
    DelimitedBlock,
    CommentLine,
    BlankLine,
    Unsupported,
    DocumentAttribute,
    BlockAnchor,
    List,
    MathBlock,
    Token(LosslessTokenKind),
    HeadingMarker,
    BlockAttribute,
    BlockTitle,
    BlockDelimiter,
    ListItem,
    ListMarker,
    InlineSpan,
    HardBreak,
    InlineDelimiter,
    Macro,
    Target,
    Label,
    Error,
    Unknown,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SyntaxIssueClass {
    HeadingMarkerSpace,
    InvalidHeadingLevel,
    UnclosedInline,
    NestingLimitExceeded,
    UnclosedBlock,
    MissingSourceLanguage,
    InvalidAttribute,
    InvalidUrl,
    InvalidCrossReference,
    InconsistentList,
    InvalidStem,
    MacroBoundary,
    UnprocessedDirective,
    MonospaceBoundary,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SyntaxFix {
    pub label: &'static str,
    pub range: TextRange,
    pub replacement: &'static str,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SyntaxIssue {
    pub class: SyntaxIssueClass,
    pub range: TextRange,
    pub message: &'static str,
    pub detail: SyntaxIssueDetail,
    pub fix: Option<SyntaxFix>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SyntaxIssueDetail {
    None,
    MacroBoundary { name: &'static str },
}

impl SyntaxKind {
    pub const fn is_block(self) -> bool {
        matches!(
            self,
            Self::DocumentTitle
                | Self::AuthorLine
                | Self::RevisionLine
                | Self::Heading
                | Self::MalformedHeading
                | Self::Paragraph
                | Self::ThematicBreak
                | Self::PageBreak
                | Self::LiteralBlock
                | Self::SourceBlock
                | Self::DelimitedBlock
                | Self::CommentLine
                | Self::BlankLine
                | Self::Unsupported
                | Self::DocumentAttribute
                | Self::BlockAnchor
                | Self::BlockAttribute
                | Self::BlockTitle
                | Self::List
                | Self::MathBlock
        )
    }

    pub const fn protects_formatting(self) -> bool {
        matches!(
            self,
            Self::DocumentTitle
                | Self::Heading
                | Self::MalformedHeading
                | Self::LiteralBlock
                | Self::SourceBlock
                | Self::DelimitedBlock
                | Self::Unsupported
                | Self::DocumentAttribute
                | Self::BlockAnchor
                | Self::BlockAttribute
                | Self::BlockTitle
                | Self::List
                | Self::MathBlock
                | Self::InlineSpan
                | Self::Macro
                | Self::Error
                | Self::Unknown
        )
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SyntaxNode {
    kind: SyntaxKind,
    range: TextRange,
    children: Vec<SyntaxNode>,
}

impl SyntaxNode {
    pub fn new(kind: SyntaxKind, range: TextRange, children: Vec<Self>) -> Self {
        Self {
            kind,
            range,
            children,
        }
    }

    pub fn leaf(kind: SyntaxKind, range: TextRange) -> Self {
        Self::new(kind, range, Vec::new())
    }

    pub const fn kind(&self) -> SyntaxKind {
        self.kind
    }

    pub const fn range(&self) -> TextRange {
        self.range
    }

    pub fn children(&self) -> &[Self] {
        &self.children
    }

    pub(crate) fn prepend_annotations(
        &mut self,
        start: crate::source::TextSize,
        mut annotations: Vec<Self>,
    ) {
        self.range = TextRange::new(start, self.range.end()).expect("metadata precedes block");
        annotations.append(&mut self.children);
        self.children = annotations;
    }

    pub fn descendants(&self) -> SyntaxDescendants<'_> {
        SyntaxDescendants {
            stack: self.children.iter().rev().collect(),
        }
    }
}

pub struct SyntaxDescendants<'a> {
    stack: Vec<&'a SyntaxNode>,
}

impl<'a> Iterator for SyntaxDescendants<'a> {
    type Item = &'a SyntaxNode;

    fn next(&mut self) -> Option<Self::Item> {
        let node = self.stack.pop()?;
        self.stack.extend(node.children.iter().rev());
        Some(node)
    }
}

#[derive(Debug)]
pub struct SyntaxTree {
    source: SourceDocument,
    root: SyntaxNode,
    issues: Vec<SyntaxIssue>,
}

pub(crate) enum SyntaxTreeBuildFailure {
    Invariant(SyntaxInvariantError),
    Cancelled,
}

impl SyntaxTree {
    /// Builds a tree only when top-level blocks and materialized token leaves
    /// form ordered, non-overlapping partitions of the source.
    #[cfg(test)]
    pub(crate) fn from_blocks(
        source: SourceDocument,
        blocks: Vec<SyntaxNode>,
        issues: Vec<SyntaxIssue>,
    ) -> Result<Self, SyntaxInvariantError> {
        match Self::from_blocks_cancellable(
            source,
            blocks,
            issues,
            &mut crate::cancellation::CancellationCheckpoint::new(&crate::core::NeverCancel),
        ) {
            Ok(tree) => Ok(tree),
            Err(SyntaxTreeBuildFailure::Invariant(error)) => Err(error),
            Err(SyntaxTreeBuildFailure::Cancelled) => {
                unreachable!("NeverCancel cannot cancel syntax tree construction")
            }
        }
    }

    pub(crate) fn from_blocks_cancellable(
        source: SourceDocument,
        mut blocks: Vec<SyntaxNode>,
        issues: Vec<SyntaxIssue>,
        checkpoint: &mut crate::cancellation::CancellationCheckpoint<'_>,
    ) -> Result<Self, SyntaxTreeBuildFailure> {
        let end = TextSize::new(source.source().len()).expect("validated source length");
        let mut cursor = TextSize::ZERO;
        for block in &mut blocks {
            if checkpoint.is_cancelled() {
                return Err(SyntaxTreeBuildFailure::Cancelled);
            }
            if !block.kind.is_block()
                || block.range.start() != cursor
                || end < block.range.end()
                || source.text(block.range).is_none()
            {
                return Err(SyntaxTreeBuildFailure::Invariant(SyntaxInvariantError));
            }
            cursor = block.range.end();
            materialize(&source, block, checkpoint)?;
        }
        if cursor != end {
            return Err(SyntaxTreeBuildFailure::Invariant(SyntaxInvariantError));
        }

        let tree = Self {
            source,
            root: SyntaxNode::new(
                SyntaxKind::Document,
                TextRange::new(TextSize::ZERO, end).expect("document range is ordered"),
                blocks,
            ),
            issues,
        };
        if !token_leaves_partition_source(&tree.root, end, checkpoint)? {
            return Err(SyntaxTreeBuildFailure::Invariant(SyntaxInvariantError));
        }
        Ok(tree)
    }

    pub fn source(&self) -> &str {
        self.source.source()
    }

    pub const fn source_document(&self) -> &SourceDocument {
        &self.source
    }

    pub const fn root(&self) -> &SyntaxNode {
        &self.root
    }

    pub fn blocks(&self) -> &[SyntaxNode] {
        self.root.children()
    }

    pub fn nodes(&self, kind: SyntaxKind) -> impl Iterator<Item = &SyntaxNode> {
        self.root
            .descendants()
            .filter(move |node| node.kind == kind)
    }

    pub fn tokens(&self) -> &[LosslessToken] {
        self.source.tokens()
    }

    pub fn issues(&self) -> &[SyntaxIssue] {
        &self.issues
    }

    pub fn formatting_protected_ranges(&self) -> Vec<TextRange> {
        let mut ranges = Vec::new();
        collect_protected_ranges(&self.root, false, &mut ranges);
        ranges
    }

    pub fn reconstruct(&self) -> String {
        let mut output = String::with_capacity(self.source().len());
        for node in self.root.descendants() {
            if matches!(node.kind, SyntaxKind::Token(_)) {
                output.push_str(
                    self.source
                        .text(node.range)
                        .expect("syntax token ranges are valid UTF-8 boundaries"),
                );
            }
        }
        output
    }

    pub fn snapshot(&self) -> String {
        fn write_node(output: &mut String, node: &SyntaxNode, depth: usize) {
            writeln!(
                output,
                "{}{:?}@{}..{}",
                "  ".repeat(depth),
                node.kind,
                node.range.start().to_u32(),
                node.range.end().to_u32()
            )
            .expect("writing to a String cannot fail");
            for child in &node.children {
                if !matches!(child.kind, SyntaxKind::Token(_)) {
                    write_node(output, child, depth + 1);
                }
            }
        }

        let mut output = String::new();
        write_node(&mut output, &self.root, 0);
        output
    }
}

fn collect_protected_ranges(
    node: &SyntaxNode,
    parent_protected: bool,
    output: &mut Vec<TextRange>,
) {
    let protected = node.kind.protects_formatting();
    if protected && !parent_protected {
        output.push(node.range);
        return;
    }
    for child in &node.children {
        collect_protected_ranges(child, parent_protected || protected, output);
    }
}

fn materialize(
    source: &SourceDocument,
    node: &mut SyntaxNode,
    checkpoint: &mut crate::cancellation::CancellationCheckpoint<'_>,
) -> Result<(), SyntaxTreeBuildFailure> {
    if checkpoint.is_cancelled() {
        return Err(SyntaxTreeBuildFailure::Cancelled);
    }
    if source.text(node.range).is_none() {
        return Err(SyntaxTreeBuildFailure::Invariant(SyntaxInvariantError));
    }
    let mut annotations = std::mem::take(&mut node.children);
    crate::cancellation::sort_by_cancellable(
        &mut annotations,
        &mut |left, right| {
            (left.range.start(), left.range.end()).cmp(&(right.range.start(), right.range.end()))
        },
        checkpoint,
    )
    .map_err(|()| SyntaxTreeBuildFailure::Cancelled)?;
    let mut cursor = node.range.start();
    let mut children = Vec::new();
    for mut annotation in annotations {
        if checkpoint.is_cancelled() {
            return Err(SyntaxTreeBuildFailure::Cancelled);
        }
        if annotation.range.start() < node.range.start()
            || node.range.end() < annotation.range.end()
            || annotation.range.start() < cursor
        {
            return Err(SyntaxTreeBuildFailure::Invariant(SyntaxInvariantError));
        }
        append_tokens(
            source,
            TextRange::new(cursor, annotation.range.start()).expect("ordered"),
            &mut children,
            checkpoint,
        )?;
        materialize(source, &mut annotation, checkpoint)?;
        cursor = annotation.range.end();
        children.push(annotation);
    }
    append_tokens(
        source,
        TextRange::new(cursor, node.range.end()).expect("ordered"),
        &mut children,
        checkpoint,
    )?;
    node.children = children;
    Ok(())
}

fn token_leaves_partition_source(
    root: &SyntaxNode,
    end: TextSize,
    checkpoint: &mut crate::cancellation::CancellationCheckpoint<'_>,
) -> Result<bool, SyntaxTreeBuildFailure> {
    let mut cursor = TextSize::ZERO;
    for node in root.descendants() {
        if checkpoint.is_cancelled() {
            return Err(SyntaxTreeBuildFailure::Cancelled);
        }
        if !matches!(node.kind, SyntaxKind::Token(_)) {
            continue;
        }
        if node.range.start() != cursor || node.range.start() >= node.range.end() {
            return Ok(false);
        }
        cursor = node.range.end();
    }
    Ok(cursor == end)
}

fn append_tokens(
    source: &SourceDocument,
    range: TextRange,
    output: &mut Vec<SyntaxNode>,
    checkpoint: &mut crate::cancellation::CancellationCheckpoint<'_>,
) -> Result<(), SyntaxTreeBuildFailure> {
    if range.is_empty() {
        return Ok(());
    }
    let tokens = source.tokens();
    let first = tokens.partition_point(|token| token.range.end() <= range.start());
    for token in tokens[first..]
        .iter()
        .take_while(|token| token.range.start() < range.end())
    {
        if checkpoint.is_cancelled() {
            return Err(SyntaxTreeBuildFailure::Cancelled);
        }
        let start = token.range.start().max(range.start());
        let end = token.range.end().min(range.end());
        if start < end {
            output.push(SyntaxNode::leaf(
                SyntaxKind::Token(token.kind),
                TextRange::new(start, end).expect("token intersection is ordered"),
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::{SyntaxIssueClass, SyntaxKind, SyntaxNode, SyntaxTree};
    use crate::source::SourceDocument;
    use crate::source::{TextRange, TextSize};

    #[test]
    fn syntax_materialization_cancels_at_a_bounded_node_checkpoint() {
        struct CancelAfterFirstCheckpoint(AtomicUsize);

        impl crate::core::CancellationCheck for CancelAfterFirstCheckpoint {
            fn is_cancelled(&self) -> bool {
                self.0.fetch_add(1, Ordering::Relaxed) >= 1
            }
        }

        let source = "line\n".repeat(crate::cancellation::CHECKPOINT_INTERVAL * 2);
        let document = SourceDocument::new(&source).expect("source");
        let blocks = document
            .lines()
            .iter()
            .map(|line| SyntaxNode::leaf(SyntaxKind::Paragraph, line.full_range()))
            .collect();
        let cancellation = CancelAfterFirstCheckpoint(AtomicUsize::new(0));

        let result = SyntaxTree::from_blocks_cancellable(
            document,
            blocks,
            Vec::new(),
            &mut crate::cancellation::CancellationCheckpoint::new(&cancellation),
        );

        assert!(matches!(
            result,
            Err(super::SyntaxTreeBuildFailure::Cancelled)
        ));
        assert_eq!(cancellation.0.load(Ordering::Relaxed), 2);
    }

    #[test]
    fn tree_reconstructs_only_from_ordered_token_leaves() {
        let source = SourceDocument::new("text \r\n").expect("source");
        let range = TextRange::new(TextSize::ZERO, TextSize::new(7).expect("size")).expect("range");
        let tree = SyntaxTree::from_blocks(
            source,
            vec![SyntaxNode::leaf(SyntaxKind::Paragraph, range)],
            Vec::new(),
        )
        .expect("valid syntax partition");

        assert_eq!(tree.reconstruct(), "text \r\n");
        assert_eq!(tree.root().kind(), SyntaxKind::Document);
        assert_eq!(tree.blocks().len(), 1);
        assert!(
            tree.blocks()[0]
                .children()
                .iter()
                .all(|node| matches!(node.kind(), SyntaxKind::Token(_)))
        );
    }

    #[test]
    fn large_flat_syntax_materializes_without_rescanning_all_tokens_per_block() {
        let source = "line\n".repeat(10_000);
        let document = SourceDocument::new(&source).expect("source");
        let block_count = document.lines().len();
        let blocks = document
            .lines()
            .iter()
            .map(|line| SyntaxNode::leaf(SyntaxKind::Paragraph, line.full_range()))
            .collect();

        let tree =
            SyntaxTree::from_blocks(document, blocks, Vec::new()).expect("valid syntax partition");

        assert_eq!(tree.blocks().len(), block_count);
        assert_eq!(tree.reconstruct(), source);
    }

    #[test]
    fn tree_rejects_top_level_gaps_overlaps_and_wrong_kinds() {
        const SOURCE: &str = "first\nsecond\n";
        let first = TextRange::new(TextSize::ZERO, TextSize::new(6).expect("size")).expect("range");
        let second = TextRange::new(
            TextSize::new(6).expect("size"),
            TextSize::new(13).expect("size"),
        )
        .expect("range");
        let full = TextRange::new(TextSize::ZERO, TextSize::new(13).expect("size")).expect("range");
        let beyond_source =
            TextRange::new(TextSize::ZERO, TextSize::new(14).expect("size")).expect("range");
        let invalid_layouts = [
            vec![SyntaxNode::leaf(SyntaxKind::Paragraph, second)],
            vec![
                SyntaxNode::leaf(SyntaxKind::Paragraph, first),
                SyntaxNode::leaf(SyntaxKind::Paragraph, first),
            ],
            vec![SyntaxNode::leaf(SyntaxKind::InlineSpan, full)],
            vec![
                SyntaxNode::leaf(SyntaxKind::Paragraph, second),
                SyntaxNode::leaf(SyntaxKind::Paragraph, first),
            ],
            vec![SyntaxNode::leaf(SyntaxKind::Paragraph, first)],
            vec![SyntaxNode::leaf(SyntaxKind::Paragraph, beyond_source)],
        ];

        for blocks in invalid_layouts {
            assert!(
                SyntaxTree::from_blocks(
                    SourceDocument::new(SOURCE).expect("source"),
                    blocks,
                    Vec::new(),
                )
                .is_err()
            );
        }
    }

    #[test]
    fn tree_rejects_out_of_bounds_and_overlapping_annotations() {
        const SOURCE: &str = "first\nsecond\n";
        let full = TextRange::new(TextSize::ZERO, TextSize::new(13).expect("size")).expect("range");
        let first = TextRange::new(TextSize::ZERO, TextSize::new(6).expect("size")).expect("range");
        let overlapping = TextRange::new(
            TextSize::new(5).expect("size"),
            TextSize::new(13).expect("size"),
        )
        .expect("range");
        let out_of_bounds = TextRange::new(
            TextSize::new(12).expect("size"),
            TextSize::new(14).expect("size"),
        )
        .expect("range");
        let invalid_annotations = [
            vec![
                SyntaxNode::leaf(SyntaxKind::InlineSpan, first),
                SyntaxNode::leaf(SyntaxKind::InlineSpan, overlapping),
            ],
            vec![SyntaxNode::leaf(SyntaxKind::InlineSpan, out_of_bounds)],
        ];

        for children in invalid_annotations {
            assert!(
                SyntaxTree::from_blocks(
                    SourceDocument::new(SOURCE).expect("source"),
                    vec![SyntaxNode::new(SyntaxKind::Paragraph, full, children)],
                    Vec::new(),
                )
                .is_err()
            );
        }
    }

    #[test]
    fn empty_tree_is_a_valid_complete_partition() {
        let tree = SyntaxTree::from_blocks(
            SourceDocument::new("").expect("source"),
            Vec::new(),
            Vec::new(),
        )
        .expect("empty source is fully covered");

        assert_eq!(tree.reconstruct(), "");
    }

    #[test]
    fn structured_nodes_expose_macros_delimiters_attributes_and_recovery() {
        let link = crate::Engine::new(crate::AnalysisOptions::default())
            .analyze("https://example.test[*label*]\n")
            .expect("link source");
        assert_eq!(link.syntax().nodes(SyntaxKind::Macro).count(), 1);
        assert_eq!(link.syntax().nodes(SyntaxKind::Target).count(), 1);
        assert_eq!(link.syntax().nodes(SyntaxKind::Label).count(), 1);
        assert_eq!(link.syntax().nodes(SyntaxKind::InlineDelimiter).count(), 2);
        assert_eq!(
            link.syntax().reconstruct(),
            "https://example.test[*label*]\n"
        );

        let unclosed = crate::Engine::new(crate::AnalysisOptions::default())
            .analyze("[source,rust]\n----\nfn main() {}\n")
            .expect("unclosed source block");
        assert_eq!(
            unclosed.syntax().nodes(SyntaxKind::BlockAttribute).count(),
            1
        );
        assert_eq!(
            unclosed.syntax().nodes(SyntaxKind::BlockDelimiter).count(),
            1
        );
        assert_eq!(unclosed.syntax().nodes(SyntaxKind::Error).count(), 1);
        assert_eq!(unclosed.syntax().issues().len(), 1);
        assert_eq!(
            unclosed.syntax().issues()[0].class,
            SyntaxIssueClass::UnclosedBlock
        );

        let unknown = crate::Engine::new(crate::AnalysisOptions::default())
            .analyze("[quote]\n")
            .expect("unsupported block attribute");
        assert_eq!(unknown.syntax().nodes(SyntaxKind::Unknown).count(), 1);
    }
}
