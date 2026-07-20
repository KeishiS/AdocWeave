//! Shared immutable traversal of the output-independent semantic tree.

use crate::attributes::DocumentAttribute;
use crate::inline::Inline;
use crate::parser::{AstBlock, AstDocument, ExplicitAnchor, ListBlock, ListItem};

#[derive(Clone, Copy, Debug)]
pub enum SemanticNode<'document> {
    Block(&'document AstBlock),
    List(&'document ListBlock),
    ListItem(&'document ListItem),
    Inline(&'document Inline),
    Attribute(&'document DocumentAttribute),
    Anchor(&'document ExplicitAnchor),
}

pub fn walk(document: &AstDocument, mut visitor: impl FnMut(SemanticNode<'_>)) {
    for attribute in document.attributes() {
        visitor(SemanticNode::Attribute(attribute));
    }
    for anchor in document.anchors() {
        visitor(SemanticNode::Anchor(anchor));
    }
    walk_blocks(document.blocks(), &mut visitor);
}

fn walk_blocks<'document>(
    blocks: &'document [AstBlock],
    visitor: &mut impl FnMut(SemanticNode<'document>),
) {
    for block in blocks {
        visitor(SemanticNode::Block(block));
        match block {
            AstBlock::Heading(heading) => walk_inlines(&heading.inlines, visitor),
            AstBlock::Paragraph(paragraph) => walk_inlines(&paragraph.inlines, visitor),
            AstBlock::List(list) => walk_list_contents(list, visitor),
            AstBlock::Literal(_)
            | AstBlock::Source(_)
            | AstBlock::Math(_)
            | AstBlock::Unsupported(_) => {}
        }
    }
}

fn walk_list_contents<'document>(
    list: &'document ListBlock,
    visitor: &mut impl FnMut(SemanticNode<'document>),
) {
    for item in &list.items {
        visitor(SemanticNode::ListItem(item));
        walk_inlines(&item.inlines, visitor);
        for child in &item.children {
            visitor(SemanticNode::List(child));
            walk_list_contents(child, visitor);
        }
        walk_blocks(&item.continuations, visitor);
    }
}

fn walk_inlines<'document>(
    inlines: &'document [Inline],
    visitor: &mut impl FnMut(SemanticNode<'document>),
) {
    for inline in inlines {
        visitor(SemanticNode::Inline(inline));
        match inline {
            Inline::Styled { children, .. } => walk_inlines(children, visitor),
            Inline::Link(link) => walk_inlines(&link.label, visitor),
            Inline::Reference(reference) => walk_inlines(&reference.label, visitor),
            Inline::Text(_)
            | Inline::Literal { .. }
            | Inline::AttributeReference { .. }
            | Inline::Formula(_) => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{SemanticNode, walk};

    #[test]
    fn walk_visits_nested_lists_continuations_and_inline_labels_once() {
        let analysis = crate::Engine::new(crate::ParseOptions::default())
            .analyze("* outer\n** https://example.test[*label*]\n+\n....\nbody\n....\n")
            .expect("source");
        let mut blocks = 0;
        let mut lists = 0;
        let mut items = 0;
        let mut inlines = 0;
        walk(analysis.ast(), |node| match node {
            SemanticNode::Block(_) => blocks += 1,
            SemanticNode::List(_) => lists += 1,
            SemanticNode::ListItem(_) => items += 1,
            SemanticNode::Inline(_) => inlines += 1,
            SemanticNode::Attribute(_) | SemanticNode::Anchor(_) => {}
        });
        assert_eq!(blocks, 2);
        assert_eq!(lists, 1);
        assert_eq!(items, 2);
        assert!(inlines >= 3);
    }
}
