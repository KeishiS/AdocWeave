//! Resolved document presentation facts and backend-independent layout.
//!
//! Source attributes remain available in the lossless syntax tree. This module
//! owns the final, immutable document-wide attribute state and the order in
//! which semantic blocks and generated document material are presented.

use std::collections::BTreeMap;

use crate::attributes::{AttributeOperation, DocumentAttribute};
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

    pub fn block_containing(&self, range: TextRange) -> Option<BlockId> {
        self.blocks
            .iter()
            .filter(|block| {
                block.range.start() <= range.start() && range.end() <= block.range.end()
            })
            .min_by_key(|block| block.range.end().to_u32() - block.range.start().to_u32())
            .map(|block| block.id)
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

/// Resolve document attribute set/unset operations once in source order.
pub(crate) fn resolve_document_attributes(
    attributes: &[DocumentAttribute],
) -> ResolvedDocumentAttributes {
    let mut values = BTreeMap::new();
    for attribute in attributes {
        match attribute.operation {
            AttributeOperation::Set => {
                values.insert(attribute.name.clone(), attribute.raw_value.clone());
            }
            AttributeOperation::Unset => {
                values.remove(&attribute.name);
            }
        }
    }
    ResolvedDocumentAttributes { values }
}

/// Document-wide facts that affect presentation but are not backend policy.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct DocumentPresentation {
    attributes: ResolvedDocumentAttributes,
    source_language: Option<String>,
    toc_policy: TocPolicy,
    section_numbers: bool,
    headings: Vec<HeadingPresentation>,
    toc: Vec<crate::structure::TocEntry>,
}

impl DocumentPresentation {
    pub const fn attributes(&self) -> &ResolvedDocumentAttributes {
        &self.attributes
    }

    pub fn headings(&self) -> &[HeadingPresentation] {
        &self.headings
    }

    pub fn source_language(&self) -> Option<&str> {
        self.source_language.as_deref()
    }

    pub const fn toc_policy(&self) -> TocPolicy {
        self.toc_policy
    }

    pub const fn section_numbers_enabled(&self) -> bool {
        self.section_numbers
    }

    pub fn heading_at(&self, range: TextRange) -> Option<&HeadingPresentation> {
        self.headings.iter().find(|heading| heading.range == range)
    }

    pub fn toc(&self) -> &[crate::structure::TocEntry] {
        &self.toc
    }
}

/// Presentation facts derived from a structural heading.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HeadingPresentation {
    pub block: BlockId,
    pub range: TextRange,
    pub number: Vec<u32>,
    pub toc_included: bool,
}

/// Typed document-level TOC configuration. Placement is intentionally absent:
/// a backend-independent layout decides where generated material is inserted.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TocPolicy {
    pub enabled: bool,
    pub max_level: Option<u8>,
}

impl Default for TocPolicy {
    fn default() -> Self {
        Self {
            enabled: false,
            max_level: None,
        }
    }
}

/// A generated document-level item. It is not a source AST node.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GeneratedLayoutNode {
    TableOfContents,
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
            | crate::parser::AstBlock::Verbatim(_)
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

pub(crate) fn build_presentation(
    document: &AstDocument,
    attributes: ResolvedDocumentAttributes,
) -> DocumentPresentation {
    let source_language = attributes
        .get("source-language")
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned);
    let toc_policy = TocPolicy {
        enabled: attributes.get("toc").is_some(),
        max_level: attributes
            .get("toclevels")
            .and_then(|value| value.trim().parse::<u8>().ok())
            .filter(|level| (1..=5).contains(level)),
    };
    let section_numbers = attributes.get("sectnums").is_some();
    let mut counters = [0_u32; 6];
    let headings = document
        .structure()
        .headings()
        .iter()
        .map(|heading| {
            let index = usize::from(heading.level.min(5));
            let number = if matches!(
                heading.kind,
                crate::structure::SectionKind::DocumentTitle
                    | crate::structure::SectionKind::Discrete
            ) {
                Vec::new()
            } else {
                counters[index] += 1;
                counters[index + 1..].fill(0);
                counters[..=index]
                    .iter()
                    .copied()
                    .filter(|number| *number != 0)
                    .collect()
            };
            let toc_included = !matches!(
                heading.kind,
                crate::structure::SectionKind::DocumentTitle
                    | crate::structure::SectionKind::Discrete
            ) && !heading_has_role(document, heading.range, "notoc");
            HeadingPresentation {
                block: document
                    .index()
                    .block_id_at(heading.range)
                    .expect("every structured heading is indexed"),
                range: heading.range,
                number,
                toc_included,
            }
        })
        .collect::<Vec<_>>();
    let toc = toc_entries(
        document.structure().roots(),
        &headings,
        toc_policy.max_level,
    );
    DocumentPresentation {
        attributes,
        source_language,
        toc_policy,
        section_numbers,
        headings,
        toc,
    }
}

fn heading_has_role(document: &AstDocument, range: TextRange, role: &str) -> bool {
    document.blocks().iter().any(|block| {
        matches!(block, crate::parser::AstBlock::Heading(heading) if heading.range == range)
            && block.metadata().roles.iter().any(|item| item.value == role)
    })
}

fn toc_entries(
    sections: &[crate::structure::Section],
    headings: &[HeadingPresentation],
    max_level: Option<u8>,
) -> Vec<crate::structure::TocEntry> {
    let mut entries = Vec::new();
    for section in sections {
        if max_level.is_some_and(|max_level| section.heading.level > max_level) {
            continue;
        }
        let children = toc_entries(&section.children, headings, max_level);
        let presentation = headings
            .iter()
            .find(|item| item.range == section.heading.range)
            .expect("every section heading has presentation facts");
        if presentation.toc_included {
            entries.push(crate::structure::TocEntry {
                id: section.heading.id.clone(),
                title: section.heading.title.clone(),
                level: section.heading.level,
                number: presentation.number.clone(),
                range: section.heading.range,
                children,
            });
        } else {
            entries.extend(children);
        }
    }
    entries
}

pub(crate) fn build_layout(document: &AstDocument) -> DocumentLayout {
    let mut nodes = document
        .index()
        .top_level_blocks()
        .iter()
        .copied()
        .map(LayoutNode::Block)
        .collect::<Vec<_>>();
    if document.presentation().toc_policy().enabled {
        let insertion = nodes
            .iter()
            .position(|node| {
                let LayoutNode::Block(id) = node else {
                    return false;
                };
                document.index().block_range(*id).is_some_and(|range| {
                    document.blocks().iter().any(|block| {
                        matches!(block, crate::parser::AstBlock::Heading(heading) if matches!(heading.kind, crate::parser::HeadingKind::DocumentTitle))
                            && block.range() == range
                    })
                })
            })
            .map_or(0, |index| index + 1);
        nodes.insert(
            insertion,
            LayoutNode::Generated(GeneratedLayoutNode::TableOfContents),
        );
    }
    nodes.push(LayoutNode::Generated(GeneratedLayoutNode::FootnoteCatalog));
    DocumentLayout { nodes }
}

#[cfg(test)]
mod tests {
    use super::{GeneratedLayoutNode, LayoutNode};
    use crate::parser::parse;

    #[test]
    fn resolves_final_attributes_and_indexes_layout_without_ranges_as_ids() {
        let parsed = parse(
            "= Title\n:source-language: rust\n:source-language!:\n:toc:\n:toclevels: 3\n:sectnums:\n\nfirst\n\nsecond\n",
        )
        .expect("parse");
        let document = parsed.ast;

        assert_eq!(
            document.presentation().attributes().get("source-language"),
            None
        );
        assert_eq!(document.presentation().source_language(), None);
        assert_eq!(
            document.presentation().toc_policy(),
            super::TocPolicy {
                enabled: true,
                max_level: Some(3),
            }
        );
        assert!(document.presentation().section_numbers_enabled());
        assert!(document.index().len() >= document.blocks().len());
        assert_eq!(document.layout().nodes().len(), document.blocks().len() + 2);
        assert_eq!(
            document.layout().nodes()[1],
            LayoutNode::Generated(GeneratedLayoutNode::TableOfContents)
        );
        for (node, block) in document.layout().nodes()[..1]
            .iter()
            .chain(document.layout().nodes()[2..document.blocks().len() + 1].iter())
            .zip(document.blocks())
        {
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
