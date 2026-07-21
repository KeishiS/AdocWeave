//! Builds lossless syntax nodes from recognized semantic facts.

use crate::inline::Inline;
use crate::parser::{
    BlockProblemKind, DelimitedBlock, DelimitedBlockKind, Heading, ListBlock, ListItem, MathBlock,
    MathProblemKind, Paragraph, SourceBlock,
};
use crate::source::TextRange;
use crate::syntax::{SyntaxKind, SyntaxNode};

pub(crate) fn heading(heading: &Heading, kind: SyntaxKind) -> SyntaxNode {
    let mut children = vec![SyntaxNode::leaf(
        SyntaxKind::HeadingMarker,
        heading.marker_range,
    )];
    children.extend(inlines(&heading.inlines));
    SyntaxNode::new(kind, heading.range, children)
}

pub(crate) fn paragraph(paragraph: &Paragraph) -> SyntaxNode {
    SyntaxNode::new(
        SyntaxKind::Paragraph,
        paragraph.range,
        inlines(&paragraph.inlines),
    )
}

pub(crate) fn source(source: &SourceBlock) -> SyntaxNode {
    SyntaxNode::new(
        SyntaxKind::SourceBlock,
        source.range,
        vec![
            SyntaxNode::leaf(SyntaxKind::BlockAttribute, source.attribute_range),
            delimiter(
                source.delimiter_range,
                source
                    .problems
                    .iter()
                    .any(|problem| problem.kind == BlockProblemKind::UnclosedBlock),
            ),
        ],
    )
}

pub(crate) fn delimited(block: &DelimitedBlock, nested: Vec<SyntaxNode>) -> SyntaxNode {
    let opening = SyntaxNode::leaf(SyntaxKind::BlockDelimiter, block.opening_delimiter_range);
    let mut children = vec![if block.closing_delimiter_range.is_none() {
        SyntaxNode::new(
            SyntaxKind::Error,
            block.opening_delimiter_range,
            vec![opening],
        )
    } else {
        opening
    }];
    children.extend(nested);
    if let Some(range) = block.closing_delimiter_range {
        children.push(SyntaxNode::leaf(SyntaxKind::BlockDelimiter, range));
    }
    let kind = if block.kind == DelimitedBlockKind::Literal {
        SyntaxKind::LiteralBlock
    } else {
        SyntaxKind::DelimitedBlock
    };
    SyntaxNode::new(kind, block.range, children)
}

pub(crate) fn math(math: &MathBlock) -> SyntaxNode {
    SyntaxNode::new(
        SyntaxKind::MathBlock,
        math.range,
        vec![
            SyntaxNode::leaf(SyntaxKind::BlockAttribute, math.attribute_range),
            delimiter(
                math.delimiter_range,
                math.problems
                    .iter()
                    .any(|problem| problem.kind == MathProblemKind::Unclosed),
            ),
        ],
    )
}

fn delimiter(range: TextRange, is_error: bool) -> SyntaxNode {
    let delimiter = SyntaxNode::leaf(SyntaxKind::BlockDelimiter, range);
    if is_error {
        SyntaxNode::new(SyntaxKind::Error, range, vec![delimiter])
    } else {
        delimiter
    }
}

pub(crate) fn list(range: TextRange, lists: &[ListBlock]) -> SyntaxNode {
    fn item_node(item: &ListItem) -> SyntaxNode {
        let mut children = vec![SyntaxNode::leaf(SyntaxKind::ListMarker, item.marker_range)];
        children.extend(item.terms.iter().flat_map(|term| inlines(&term.inlines)));
        children.extend(inlines(&item.inlines));
        children.extend(
            item.children
                .iter()
                .flat_map(|list| list.items.iter().map(item_node)),
        );
        SyntaxNode::new(SyntaxKind::ListItem, item.range, children)
    }
    SyntaxNode::new(
        SyntaxKind::List,
        range,
        lists
            .iter()
            .flat_map(|list| list.items.iter().map(item_node))
            .collect(),
    )
}

fn inlines(values: &[Inline]) -> Vec<SyntaxNode> {
    values.iter().filter_map(inline).collect()
}

fn inline(value: &Inline) -> Option<SyntaxNode> {
    match value {
        Inline::Text(_) => None,
        Inline::Literal {
            range,
            content_range,
            ..
        } => Some(span(*range, *content_range, Vec::new())),
        Inline::Styled {
            range,
            content_range,
            children,
            ..
        } => Some(span(*range, *content_range, inlines(children))),
        Inline::AttributeReference {
            range, name_range, ..
        } => Some(SyntaxNode::new(
            SyntaxKind::Macro,
            *range,
            vec![SyntaxNode::leaf(SyntaxKind::Target, *name_range)],
        )),
        Inline::Link(link) => Some(macro_node(
            link.range,
            link.target_range,
            link.label_range,
            &link.label,
        )),
        Inline::Reference(reference) => Some(macro_node(
            reference.range,
            reference.target_range,
            reference.label_range,
            &reference.label,
        )),
        Inline::Formula(formula) => {
            Some(macro_node(formula.range, formula.content_range, None, &[]))
        }
        Inline::Macro(node) => Some(SyntaxNode::new(
            SyntaxKind::Macro,
            node.range,
            vec![SyntaxNode::leaf(SyntaxKind::Target, node.target_range)],
        )),
        Inline::HardBreak { range } => Some(SyntaxNode::leaf(SyntaxKind::HardBreak, *range)),
        Inline::Passthrough {
            range,
            content_range,
            ..
        } => Some(span(*range, *content_range, Vec::new())),
    }
}

fn span(range: TextRange, content_range: TextRange, mut content: Vec<SyntaxNode>) -> SyntaxNode {
    let mut children = vec![SyntaxNode::leaf(
        SyntaxKind::InlineDelimiter,
        TextRange::new(range.start(), content_range.start()).expect("ordered inline range"),
    )];
    children.append(&mut content);
    if content_range.end() < range.end() {
        children.push(SyntaxNode::leaf(
            SyntaxKind::InlineDelimiter,
            TextRange::new(content_range.end(), range.end()).expect("ordered inline range"),
        ));
    }
    SyntaxNode::new(SyntaxKind::InlineSpan, range, children)
}

fn macro_node(
    range: TextRange,
    target_range: TextRange,
    label_range: Option<TextRange>,
    label: &[Inline],
) -> SyntaxNode {
    let mut children = vec![SyntaxNode::leaf(SyntaxKind::Target, target_range)];
    if let Some(label_range) = label_range {
        children.push(SyntaxNode::new(
            SyntaxKind::Label,
            label_range,
            inlines(label),
        ));
    }
    SyntaxNode::new(SyntaxKind::Macro, range, children)
}
