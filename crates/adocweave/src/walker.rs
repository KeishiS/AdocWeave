//! Shared immutable traversal of the output-independent semantic tree.

use crate::attributes::DocumentAttribute;
use crate::inline::Inline;
use crate::parser::{
    AstBlock, AstDocument, BlockMetadata, ElementAttribute, ExplicitAnchor, ListBlock, ListItem,
    MetadataValue,
};

#[derive(Clone, Copy, Debug)]
pub enum SemanticNode<'document> {
    Block(&'document AstBlock),
    List(&'document ListBlock),
    ListItem(&'document ListItem),
    Table(&'document crate::table::Table),
    TableRow(&'document crate::table::TableRow),
    TableCell(&'document crate::table::TableCell),
    Inline(&'document Inline),
    Attribute(&'document DocumentAttribute),
    Anchor(&'document ExplicitAnchor),
    Metadata(&'document BlockMetadata),
    MetadataTitle(&'document MetadataValue),
    MetadataId(&'document MetadataValue),
    MetadataRole(&'document MetadataValue),
    MetadataOption(&'document MetadataValue),
    ElementAttribute(&'document ElementAttribute),
}

pub fn walk<'document>(
    document: &'document AstDocument,
    mut visitor: impl FnMut(SemanticNode<'document>),
) {
    for attribute in document.attributes() {
        visitor(SemanticNode::Attribute(attribute));
    }
    for anchor in document.anchors() {
        visitor(SemanticNode::Anchor(anchor));
    }
    walk_blocks(document.blocks(), &mut visitor);
}

pub(crate) fn walk_block_slice<'document>(
    blocks: &'document [AstBlock],
    mut visitor: impl FnMut(SemanticNode<'document>),
) {
    walk_blocks(blocks, &mut visitor);
}

fn walk_blocks<'document>(
    blocks: &'document [AstBlock],
    visitor: &mut impl FnMut(SemanticNode<'document>),
) {
    for block in blocks {
        visitor(SemanticNode::Block(block));
        walk_metadata(block.metadata(), visitor);
        match block {
            AstBlock::Heading(heading) => walk_inlines(&heading.inlines, visitor),
            AstBlock::Paragraph(paragraph) => walk_inlines(&paragraph.inlines, visitor),
            AstBlock::List(list) => walk_list_contents(list, visitor),
            AstBlock::Delimited(block) => match &block.content {
                crate::parser::DelimitedContent::Compound(children) => {
                    walk_blocks(children, visitor)
                }
                crate::parser::DelimitedContent::Table(table) => {
                    visitor(SemanticNode::Table(table));
                    for row in &table.rows {
                        visitor(SemanticNode::TableRow(row));
                        for cell in &row.cells {
                            visitor(SemanticNode::TableCell(cell));
                            match &cell.content {
                                crate::table::TableCellContent::Inlines(inlines) => {
                                    walk_inlines(inlines, visitor)
                                }
                                crate::table::TableCellContent::AsciiDoc(blocks) => {
                                    walk_blocks(blocks, visitor)
                                }
                                crate::table::TableCellContent::Verbatim(_) => {}
                            }
                        }
                    }
                }
                crate::parser::DelimitedContent::Verbatim(_)
                | crate::parser::DelimitedContent::Passthrough(_) => {}
            },
            AstBlock::Literal(_)
            | AstBlock::LiteralParagraph(_)
            | AstBlock::Break(_)
            | AstBlock::Source(_)
            | AstBlock::Math(_)
            | AstBlock::Unsupported(_) => {}
        }
    }
}

fn walk_metadata<'document>(
    metadata: &'document BlockMetadata,
    visitor: &mut impl FnMut(SemanticNode<'document>),
) {
    visitor(SemanticNode::Metadata(metadata));
    if let Some(title) = &metadata.title {
        visitor(SemanticNode::MetadataTitle(title));
    }
    if let Some(id) = &metadata.id {
        visitor(SemanticNode::MetadataId(id));
    }
    for role in &metadata.roles {
        visitor(SemanticNode::MetadataRole(role));
    }
    for option in &metadata.options {
        visitor(SemanticNode::MetadataOption(option));
    }
    for attribute in &metadata.attributes {
        visitor(SemanticNode::ElementAttribute(attribute));
    }
}

fn walk_list_contents<'document>(
    list: &'document ListBlock,
    visitor: &mut impl FnMut(SemanticNode<'document>),
) {
    for item in &list.items {
        visitor(SemanticNode::ListItem(item));
        for term in &item.terms {
            walk_inlines(&term.inlines, visitor);
        }
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
            Inline::Macro(_) => {}
            Inline::Text(_)
            | Inline::Literal { .. }
            | Inline::AttributeReference { .. }
            | Inline::HardBreak { .. }
            | Inline::Passthrough { .. }
            | Inline::Formula(_) => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{SemanticNode, walk};
    use crate::parser::AstBlock;

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
            SemanticNode::Table(_) | SemanticNode::TableRow(_) | SemanticNode::TableCell(_) => {}
            SemanticNode::Inline(_) => inlines += 1,
            SemanticNode::Attribute(_)
            | SemanticNode::Anchor(_)
            | SemanticNode::Metadata(_)
            | SemanticNode::MetadataTitle(_)
            | SemanticNode::MetadataId(_)
            | SemanticNode::MetadataRole(_)
            | SemanticNode::MetadataOption(_)
            | SemanticNode::ElementAttribute(_) => {}
        });
        assert_eq!(blocks, 2);
        assert_eq!(lists, 1);
        assert_eq!(items, 2);
        assert!(inlines >= 3);
    }

    #[test]
    fn every_semantic_query_observes_the_same_nested_reachability() {
        let source = concat!(
            "====\n",
            "xref:top[]\n",
            "\n",
            "* image:outer.png[]\n",
            "+\n",
            "[cols=\"a\"]\n",
            "|===\n",
            "|xref:cell[] image:cell.png[]\n",
            "|===\n",
            "====\n",
        );
        let analysis = crate::Engine::new(crate::ParseOptions::default())
            .analyze(source)
            .expect("source");
        let mut walked_references = 0;
        let mut walked_macros = 0;
        walk(analysis.ast(), |node| {
            if let SemanticNode::Inline(inline) = node {
                match inline {
                    crate::inline::Inline::Reference(_) => walked_references += 1,
                    crate::inline::Inline::Macro(_) => walked_macros += 1,
                    _ => {}
                }
            }
        });

        assert_eq!(analysis.references().len(), walked_references);
        assert_eq!(analysis.macros().len(), walked_macros);
        assert_eq!(analysis.resources().len(), walked_macros);
        assert_eq!(walked_references, 2);
        assert_eq!(walked_macros, 2);
    }

    #[test]
    fn final_semantic_tree_contains_no_parser_recovery_state() {
        for source in [
            "==Missing\n",
            "paragraph **open\n",
            "[source]\n----\n== Next\n",
            "*  item\n",
            "[stem]\n++++\nopen\n== Next\n",
        ] {
            let analysis = crate::Engine::new(crate::ParseOptions::default())
                .analyze(source)
                .expect("recoverable source");
            walk(analysis.ast(), |node| match node {
                SemanticNode::Block(block) => match block {
                    AstBlock::Heading(value) => {
                        assert!(value.problems.is_empty());
                        assert!(value.inline_problems.is_empty());
                    }
                    AstBlock::Paragraph(value) => assert!(value.inline_problems.is_empty()),
                    AstBlock::Literal(value) => assert!(value.problems.is_empty()),
                    AstBlock::Source(value) => assert!(value.problems.is_empty()),
                    AstBlock::Math(value) => assert!(value.problems.is_empty()),
                    AstBlock::Delimited(value) => assert!(value.problems.is_empty()),
                    AstBlock::List(_)
                    | AstBlock::LiteralParagraph(_)
                    | AstBlock::Break(_)
                    | AstBlock::Unsupported(_) => {}
                },
                SemanticNode::ListItem(item) => {
                    assert!(item.problems.is_empty());
                    assert!(item.inline_problems.is_empty());
                    assert!(
                        item.terms
                            .iter()
                            .all(|term| term.inline_problems.is_empty())
                    );
                }
                SemanticNode::List(_)
                | SemanticNode::Table(_)
                | SemanticNode::TableRow(_)
                | SemanticNode::TableCell(_)
                | SemanticNode::Inline(_)
                | SemanticNode::Attribute(_)
                | SemanticNode::Anchor(_)
                | SemanticNode::Metadata(_)
                | SemanticNode::MetadataTitle(_)
                | SemanticNode::MetadataId(_)
                | SemanticNode::MetadataRole(_)
                | SemanticNode::MetadataOption(_)
                | SemanticNode::ElementAttribute(_) => {}
            });
        }
    }
}
