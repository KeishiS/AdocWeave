//! Resolved document presentation facts and backend-independent layout.
//!
//! Source attributes remain available in the lossless syntax tree. This module
//! owns the final, immutable document-wide attribute state and the order in
//! which semantic blocks and generated document material are presented.

use std::collections::BTreeMap;

use crate::attributes::AttributeOperation;
use crate::parser::AstDocument;
use crate::source::TextRange;

/// Stable identity of a semantic block within one [`crate::Analysis`].
///
/// Values are allocated in deterministic document order. They are opaque to
/// callers and must not be inferred from source offsets.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Hash)]
pub struct BlockId(u32);

impl BlockId {
    pub const fn get(self) -> u32 {
        self.0
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct IndexedBlock {
    id: BlockId,
    range: TextRange,
}

/// Immutable lookup table between semantic block identities and their source
/// locations. Catalogs and layouts use this table instead of treating a range
/// as an identity.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct DocumentIndex {
    blocks: Vec<IndexedBlock>,
    top_level_blocks: Vec<BlockId>,
}

impl DocumentIndex {
    pub fn block_id_at(&self, range: TextRange) -> Option<BlockId> {
        self.blocks
            .iter()
            .find(|block| block.range == range)
            .map(|block| block.id)
    }

    pub fn block_range(&self, id: BlockId) -> Option<TextRange> {
        self.blocks
            .iter()
            .find(|block| block.id == id)
            .map(|block| block.range)
    }

    pub fn len(&self) -> usize {
        self.blocks.len()
    }

    pub const fn is_empty(&self) -> bool {
        self.blocks.is_empty()
    }

    pub fn top_level_blocks(&self) -> &[BlockId] {
        &self.top_level_blocks
    }
}

/// Final document-wide attribute state after applying set and unset operations.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ResolvedDocumentAttributes {
    values: BTreeMap<String, String>,
}

impl ResolvedDocumentAttributes {
    pub fn get(&self, name: &str) -> Option<&str> {
        self.values.get(name).map(String::as_str)
    }

    pub fn values(&self) -> &BTreeMap<String, String> {
        &self.values
    }
}

/// Document-wide facts that affect presentation but are not backend policy.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct DocumentPresentation {
    attributes: ResolvedDocumentAttributes,
}

impl DocumentPresentation {
    pub const fn attributes(&self) -> &ResolvedDocumentAttributes {
        &self.attributes
    }
}

/// A generated document-level item. It is not a source AST node.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GeneratedLayoutNode {
    FootnoteCatalog,
}

/// One item in a backend-independent document layout.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LayoutNode {
    Block(BlockId),
    Generated(GeneratedLayoutNode),
}

/// Immutable presentation order for top-level semantic blocks and generated
/// document material.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct DocumentLayout {
    nodes: Vec<LayoutNode>,
}

impl DocumentLayout {
    pub fn nodes(&self) -> &[LayoutNode] {
        &self.nodes
    }
}

pub(crate) fn build_index(document: &AstDocument) -> DocumentIndex {
    fn index_list(list: &crate::parser::ListBlock, blocks: &mut Vec<IndexedBlock>) {
        for item in &list.items {
            for child in &item.children {
                index_list(child, blocks);
            }
            for continuation in &item.continuations {
                index_block(continuation, blocks);
            }
        }
    }

    fn index_block(block: &crate::parser::AstBlock, blocks: &mut Vec<IndexedBlock>) -> BlockId {
        let id = BlockId(u32::try_from(blocks.len()).expect("block count fits u32"));
        blocks.push(IndexedBlock {
            id,
            range: block.range(),
        });
        match block {
            crate::parser::AstBlock::List(list) => index_list(list, blocks),
            crate::parser::AstBlock::Delimited(block) => match &block.content {
                crate::parser::DelimitedContent::Compound(children) => {
                    for child in children {
                        index_block(child, blocks);
                    }
                }
                crate::parser::DelimitedContent::Table(table) => {
                    for row in &table.rows {
                        for cell in &row.cells {
                            if let crate::table::TableCellContent::AsciiDoc(children) =
                                &cell.content
                            {
                                for child in children {
                                    index_block(child, blocks);
                                }
                            }
                        }
                    }
                }
                crate::parser::DelimitedContent::Verbatim(_)
                | crate::parser::DelimitedContent::Passthrough(_) => {}
            },
            crate::parser::AstBlock::Heading(_)
            | crate::parser::AstBlock::Paragraph(_)
            | crate::parser::AstBlock::LiteralParagraph(_)
            | crate::parser::AstBlock::Break(_)
            | crate::parser::AstBlock::Source(_)
            | crate::parser::AstBlock::Math(_)
            | crate::parser::AstBlock::Unsupported(_) => {}
        }
        id
    }

    let mut blocks = Vec::new();
    let top_level_blocks = document
        .blocks()
        .iter()
        .map(|block| index_block(block, &mut blocks))
        .collect();
    DocumentIndex {
        blocks,
        top_level_blocks,
    }
}

pub(crate) fn build_presentation(document: &AstDocument) -> DocumentPresentation {
    let mut values = BTreeMap::new();
    for attribute in document.attributes() {
        match attribute.operation {
            AttributeOperation::Set => {
                values.insert(attribute.name.clone(), attribute.raw_value.clone());
            }
            AttributeOperation::Unset => {
                values.remove(&attribute.name);
            }
        }
    }
    DocumentPresentation {
        attributes: ResolvedDocumentAttributes { values },
    }
}

pub(crate) fn build_layout(document: &AstDocument) -> DocumentLayout {
    let mut nodes = document
        .index()
        .top_level_blocks()
        .iter()
        .copied()
        .map(LayoutNode::Block)
        .collect::<Vec<_>>();
    nodes.push(LayoutNode::Generated(GeneratedLayoutNode::FootnoteCatalog));
    DocumentLayout { nodes }
}

#[cfg(test)]
mod tests {
    use super::{GeneratedLayoutNode, LayoutNode};
    use crate::parser::parse;

    #[test]
    fn resolves_final_attributes_and_indexes_layout_without_ranges_as_ids() {
        let parsed = parse(":source-language: rust\n:source-language!:\n\nfirst\n\nsecond\n")
            .expect("parse");
        let document = parsed.ast;

        assert_eq!(
            document.presentation().attributes().get("source-language"),
            None
        );
        assert!(document.index().len() >= document.blocks().len());
        assert_eq!(document.layout().nodes().len(), document.blocks().len() + 1);
        for (node, block) in document.layout().nodes().iter().zip(document.blocks()) {
            assert_eq!(
                *node,
                LayoutNode::Block(
                    document
                        .index()
                        .block_id_at(block.range())
                        .expect("indexed block")
                )
            );
        }
        assert_eq!(
            document.layout().nodes().last(),
            Some(&LayoutNode::Generated(GeneratedLayoutNode::FootnoteCatalog))
        );
    }
}
