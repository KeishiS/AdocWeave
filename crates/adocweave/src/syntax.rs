//! Lossless concrete syntax tree over one [`SourceDocument`].

use std::fmt::Write as _;

use crate::source::{TextRange, TextSize};
use crate::source_document::{LosslessToken, LosslessTokenKind, SourceDocument};

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

impl SyntaxTree {
    /// Builds a tree only when top-level blocks and materialized token leaves
    /// form ordered, non-overlapping partitions of the source.
    pub(crate) fn from_blocks(
        source: SourceDocument,
        mut blocks: Vec<SyntaxNode>,
        issues: Vec<SyntaxIssue>,
    ) -> Result<Self, SyntaxInvariantError> {
        let end = TextSize::new(source.source().len()).expect("validated source length");
        let mut cursor = TextSize::ZERO;
        for block in &mut blocks {
            if !block.kind.is_block()
                || block.range.start() != cursor
                || end < block.range.end()
                || source.text(block.range).is_none()
            {
                return Err(SyntaxInvariantError);
            }
            cursor = block.range.end();
            materialize(&source, block)?;
        }
        if cursor != end {
            return Err(SyntaxInvariantError);
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
        if !token_leaves_partition_source(&tree.root, end) {
            return Err(SyntaxInvariantError);
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

fn materialize(source: &SourceDocument, node: &mut SyntaxNode) -> Result<(), SyntaxInvariantError> {
    if source.text(node.range).is_none() {
        return Err(SyntaxInvariantError);
    }
    let mut annotations = std::mem::take(&mut node.children);
    annotations.sort_by_key(|child| (child.range.start(), child.range.end()));
    let mut cursor = node.range.start();
    let mut children = Vec::new();
    for mut annotation in annotations {
        if annotation.range.start() < node.range.start()
            || node.range.end() < annotation.range.end()
            || annotation.range.start() < cursor
        {
            return Err(SyntaxInvariantError);
        }
        append_tokens(
            source,
            TextRange::new(cursor, annotation.range.start()).expect("ordered"),
            &mut children,
        );
        materialize(source, &mut annotation)?;
        cursor = annotation.range.end();
        children.push(annotation);
    }
    append_tokens(
        source,
        TextRange::new(cursor, node.range.end()).expect("ordered"),
        &mut children,
    );
    node.children = children;
    Ok(())
}

fn token_leaves_partition_source(root: &SyntaxNode, end: TextSize) -> bool {
    let mut cursor = TextSize::ZERO;
    for node in root.descendants() {
        if !matches!(node.kind, SyntaxKind::Token(_)) {
            continue;
        }
        if node.range.start() != cursor || node.range.start() >= node.range.end() {
            return false;
        }
        cursor = node.range.end();
    }
    cursor == end
}

fn append_tokens(source: &SourceDocument, range: TextRange, output: &mut Vec<SyntaxNode>) {
    if range.is_empty() {
        return;
    }
    let tokens = source.tokens();
    let first = tokens.partition_point(|token| token.range.end() <= range.start());
    for token in tokens[first..]
        .iter()
        .take_while(|token| token.range.start() < range.end())
    {
        let start = token.range.start().max(range.start());
        let end = token.range.end().min(range.end());
        if start < end {
            output.push(SyntaxNode::leaf(
                SyntaxKind::Token(token.kind),
                TextRange::new(start, end).expect("token intersection is ordered"),
            ));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{SyntaxIssueClass, SyntaxKind, SyntaxNode, SyntaxTree};
    use crate::source::{TextRange, TextSize};
    use crate::source_document::SourceDocument;

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
