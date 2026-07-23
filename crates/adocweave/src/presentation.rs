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

/// Immutable lookup table between semantic block identities and their source
/// locations. Catalogs and layouts use this table instead of treating a range
/// as an identity.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct DocumentIndex {
    block_ranges: Vec<TextRange>,
    block_ids_by_range: BTreeMap<TextRange, BlockId>,
    top_level_blocks: Vec<BlockId>,
    top_level_ordinals: Vec<Option<usize>>,
}

impl DocumentIndex {
    pub fn block_id_at(&self, range: TextRange) -> Option<BlockId> {
        self.block_ids_by_range.get(&range).copied()
    }

    pub fn block_range(&self, id: BlockId) -> Option<TextRange> {
        self.block_ranges.get(id.get() as usize).copied()
    }

    pub fn block_containing(&self, range: TextRange) -> Option<BlockId> {
        self.block_ranges
            .iter()
            .enumerate()
            .filter(|(_, block_range)| {
                block_range.start() <= range.start() && range.end() <= block_range.end()
            })
            .min_by_key(|(_, block_range)| block_range.len())
            .map(|(index, _)| BlockId(u32::try_from(index).expect("block count fits u32")))
    }

    pub fn len(&self) -> usize {
        self.block_ranges.len()
    }

    pub fn is_empty(&self) -> bool {
        self.block_ranges.is_empty()
    }

    pub fn top_level_blocks(&self) -> &[BlockId] {
        &self.top_level_blocks
    }

    pub fn top_level_ordinal(&self, id: BlockId) -> Option<usize> {
        self.top_level_ordinals
            .get(id.get() as usize)
            .copied()
            .flatten()
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
    bibliography_sections: Vec<BibliographySection>,
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

    pub fn bibliography_section_at(&self, range: TextRange) -> Option<&BibliographySection> {
        self.bibliography_sections
            .iter()
            .find(|section| section.range == range)
    }

    pub fn bibliography_sections(&self) -> &[BibliographySection] {
        &self.bibliography_sections
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

/// A section explicitly styled as an AsciiDoc bibliography section.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BibliographySection {
    pub block: BlockId,
    pub range: TextRange,
}

/// Typed document-level TOC configuration. Placement is intentionally absent:
/// a backend-independent layout decides where generated material is inserted.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct TocPolicy {
    pub enabled: bool,
    pub max_level: Option<u8>,
    pub invalid_level_range: Option<TextRange>,
}

/// A generated document-level item. It is not a source AST node.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GeneratedLayoutNode {
    TableOfContents,
    FootnoteCatalog,
}

/// Semantic scope attached to a nested layout region.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LayoutScope {
    Bibliography,
}

/// One item in a backend-independent document layout.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum LayoutNode {
    Block(BlockId),
    Generated(GeneratedLayoutNode),
    Section {
        scope: LayoutScope,
        nodes: Vec<LayoutNode>,
    },
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
    fn index_list(list: &crate::parser::ListBlock, block_ranges: &mut Vec<TextRange>) {
        for item in &list.items {
            for child in &item.children {
                index_list(child, block_ranges);
            }
            for continuation in &item.continuations {
                index_block(continuation, block_ranges);
            }
        }
    }

    fn index_block(block: &crate::parser::AstBlock, block_ranges: &mut Vec<TextRange>) -> BlockId {
        let id = BlockId(u32::try_from(block_ranges.len()).expect("block count fits u32"));
        block_ranges.push(block.range());
        match block {
            crate::parser::AstBlock::List(list) => index_list(list, block_ranges),
            crate::parser::AstBlock::Delimited(block) => match &block.content {
                crate::parser::DelimitedContent::Compound(children) => {
                    for child in children {
                        index_block(child, block_ranges);
                    }
                }
                crate::parser::DelimitedContent::Table(table) => {
                    for row in &table.rows {
                        for cell in &row.cells {
                            if let crate::table::TableCellContent::AsciiDoc(children) =
                                &cell.content
                            {
                                for child in children {
                                    index_block(child, block_ranges);
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

    let mut block_ranges = Vec::new();
    let top_level_blocks = document
        .blocks()
        .iter()
        .map(|block| index_block(block, &mut block_ranges))
        .collect::<Vec<_>>();
    let mut top_level_ordinals = vec![None; block_ranges.len()];
    for (ordinal, id) in top_level_blocks.iter().copied().enumerate() {
        top_level_ordinals[id.get() as usize] = Some(ordinal);
    }
    let mut block_ids_by_range = BTreeMap::new();
    for (index, range) in block_ranges.iter().copied().enumerate() {
        block_ids_by_range
            .entry(range)
            .or_insert_with(|| BlockId(u32::try_from(index).expect("block count fits u32")));
    }
    DocumentIndex {
        block_ranges,
        block_ids_by_range,
        top_level_blocks,
        top_level_ordinals,
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
    let toclevels = attributes.get("toclevels");
    let max_level = toclevels
        .and_then(|value| value.trim().parse::<u8>().ok())
        .filter(|level| (1..=5).contains(level));
    let invalid_level_range = toclevels.filter(|_| max_level.is_none()).and_then(|_| {
        document
            .attributes()
            .iter()
            .rev()
            .find(|attribute| attribute.name == "toclevels")
            .map(|attribute| attribute.value_range)
    });
    let toc_policy = TocPolicy {
        enabled: attributes.get("toc").is_some(),
        max_level,
        invalid_level_range,
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
    let bibliography_sections = document
        .blocks()
        .iter()
        .filter_map(|block| {
            let crate::parser::AstBlock::Heading(heading) = block else {
                return None;
            };
            block
                .metadata()
                .attributes
                .iter()
                .any(|attribute| attribute.name.is_none() && attribute.value == "bibliography")
                .then(|| BibliographySection {
                    block: document
                        .index()
                        .block_id_at(heading.range)
                        .expect("every heading is indexed"),
                    range: heading.range,
                })
        })
        .collect();
    DocumentPresentation {
        attributes,
        source_language,
        toc_policy,
        section_numbers,
        headings,
        toc,
        bibliography_sections,
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
    fn structural_heading_level(block: &crate::parser::AstBlock) -> Option<u8> {
        let crate::parser::AstBlock::Heading(heading) = block else {
            return None;
        };
        match heading.kind {
            crate::parser::HeadingKind::DocumentTitle | crate::parser::HeadingKind::Part => Some(0),
            crate::parser::HeadingKind::Section { level } => Some(level),
            crate::parser::HeadingKind::Discrete { .. } => None,
        }
    }

    let mut nodes = Vec::new();
    let mut bibliography_scope: Option<(u8, Vec<LayoutNode>)> = None;
    for id in document.index().top_level_blocks().iter().copied() {
        let block = document
            .top_level_block(id)
            .expect("indexed top-level block");
        let heading_level = structural_heading_level(block);
        if bibliography_scope
            .as_ref()
            .is_some_and(|(level, _)| heading_level.is_some_and(|next_level| next_level <= *level))
        {
            let (_, scoped_nodes) = bibliography_scope
                .take()
                .expect("scope existence was checked above");
            nodes.push(LayoutNode::Section {
                scope: LayoutScope::Bibliography,
                nodes: scoped_nodes,
            });
        }

        if let Some((_, scoped_nodes)) = &mut bibliography_scope {
            scoped_nodes.push(LayoutNode::Block(id));
            continue;
        }

        let bibliography_level = matches!(block, crate::parser::AstBlock::Heading(heading)
            if document.presentation().bibliography_section_at(heading.range).is_some())
        .then_some(heading_level)
        .flatten();
        if let Some(level) = bibliography_level {
            bibliography_scope = Some((level, vec![LayoutNode::Block(id)]));
        } else {
            nodes.push(LayoutNode::Block(id));
        }
    }
    if let Some((_, scoped_nodes)) = bibliography_scope {
        nodes.push(LayoutNode::Section {
            scope: LayoutScope::Bibliography,
            nodes: scoped_nodes,
        });
    }
    if document.presentation().toc_policy().enabled {
        let insertion = nodes
            .iter()
            .position(|node| {
                let LayoutNode::Block(id) = node else {
                    return false;
                };
                matches!(
                    document.top_level_block(*id),
                    Some(crate::parser::AstBlock::Heading(heading))
                        if matches!(heading.kind, crate::parser::HeadingKind::DocumentTitle)
                )
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
    use super::{GeneratedLayoutNode, LayoutNode, LayoutScope};
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
                invalid_level_range: None,
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

    #[test]
    fn bibliography_section_is_resolved_once_from_heading_style() {
        let parsed = parse("= References\n\n[bibliography]\n== Sources\n").expect("parse");
        let document = parsed.ast;

        assert_eq!(document.presentation().bibliography_sections().len(), 1);
        assert_eq!(
            document.presentation().bibliography_sections()[0].range,
            document.blocks()[1].range()
        );
    }

    #[test]
    fn bibliography_sections_own_their_layout_scope() {
        let parsed =
            parse("= Title\n\n[bibliography]\n== Sources\n\n* entry\n\n== After\n").expect("parse");
        let nodes = parsed.ast.layout().nodes();

        assert!(nodes.iter().any(|node| {
            matches!(
                node,
                LayoutNode::Section {
                    scope: LayoutScope::Bibliography,
                    nodes
                } if matches!(nodes.first(), Some(LayoutNode::Block(_)))
            )
        }));
    }
}
