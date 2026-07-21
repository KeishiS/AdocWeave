//! Lossless concrete syntax tree over one [`SourceDocument`].

use std::fmt::Write as _;

use crate::source::{TextRange, TextSize};
use crate::source_document::{LosslessToken, LosslessTokenKind, SourceDocument};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SyntaxKind {
    Document,
    DocumentTitle,
    Heading,
    MalformedHeading,
    Paragraph,
    LiteralBlock,
    SourceBlock,
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
    pub fix: Option<SyntaxFix>,
}

impl SyntaxKind {
    pub const fn is_block(self) -> bool {
        matches!(
            self,
            Self::DocumentTitle
                | Self::Heading
                | Self::MalformedHeading
                | Self::Paragraph
                | Self::LiteralBlock
                | Self::SourceBlock
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
    pub(crate) fn from_blocks(
        source: SourceDocument,
        mut blocks: Vec<SyntaxNode>,
        issues: Vec<SyntaxIssue>,
    ) -> Self {
        for block in &mut blocks {
            debug_assert!(block.kind.is_block());
            materialize(&source, block);
        }
        let end = TextSize::new(source.source().len()).expect("validated source length");
        Self {
            source,
            root: SyntaxNode::new(
                SyntaxKind::Document,
                TextRange::new(TextSize::ZERO, end).expect("document range is ordered"),
                blocks,
            ),
            issues,
        }
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

fn materialize(source: &SourceDocument, node: &mut SyntaxNode) {
    let mut annotations = std::mem::take(&mut node.children);
    annotations.sort_by_key(|child| (child.range.start(), child.range.end()));
    let mut cursor = node.range.start();
    let mut children = Vec::new();
    for mut annotation in annotations {
        assert!(
            node.range.start() <= annotation.range.start()
                && annotation.range.end() <= node.range.end(),
            "syntax child must be contained by its parent"
        );
        assert!(
            cursor <= annotation.range.start(),
            "syntax children must not overlap"
        );
        append_tokens(
            source,
            TextRange::new(cursor, annotation.range.start()).expect("ordered"),
            &mut children,
        );
        materialize(source, &mut annotation);
        cursor = annotation.range.end();
        children.push(annotation);
    }
    append_tokens(
        source,
        TextRange::new(cursor, node.range.end()).expect("ordered"),
        &mut children,
    );
    node.children = children;
}

fn append_tokens(source: &SourceDocument, range: TextRange, output: &mut Vec<SyntaxNode>) {
    if range.is_empty() {
        return;
    }
    for token in source.tokens() {
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
        );

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
    fn structured_nodes_expose_macros_delimiters_attributes_and_recovery() {
        let link = crate::Engine::new(crate::ParseOptions::default())
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

        let unclosed = crate::Engine::new(crate::ParseOptions::default())
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

        let unknown = crate::Engine::new(crate::ParseOptions::default())
            .analyze("[quote]\n")
            .expect("unsupported block attribute");
        assert_eq!(unknown.syntax().nodes(SyntaxKind::Unknown).count(), 1);
    }
}
