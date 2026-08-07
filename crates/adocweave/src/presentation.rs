//! Resolved document presentation facts and backend-independent layout.
//!
//! Source attributes remain available in the lossless syntax tree. This module
//! owns the final, immutable document-wide attribute state and the order in
//! which semantic blocks and generated document material are presented.

use std::collections::BTreeMap;

use crate::block_model::AstDocument;
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

/// Document-wide facts that affect presentation but are not backend policy.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct DocumentPresentation {
    source_language: Option<String>,
    toc_policy: TocPolicy,
    section_numbers: bool,
    headings: Vec<HeadingPresentation>,
    heading_ordinals: BTreeMap<TextRange, usize>,
    toc: Vec<crate::structure::TocEntry>,
    bibliography_sections: Vec<BibliographySection>,
    bibliography_ordinals: BTreeMap<TextRange, usize>,
}

impl DocumentPresentation {
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
        self.heading_ordinals
            .get(&range)
            .and_then(|ordinal| self.headings.get(*ordinal))
    }

    pub fn toc(&self) -> &[crate::structure::TocEntry] {
        &self.toc
    }

    pub fn bibliography_section_at(&self, range: TextRange) -> Option<&BibliographySection> {
        self.bibliography_ordinals
            .get(&range)
            .and_then(|ordinal| self.bibliography_sections.get(*ordinal))
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
    pub numbered: bool,
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

pub(crate) fn build_index(
    document: &AstDocument,
    checkpoint: &mut crate::cancellation::CancellationCheckpoint<'_>,
) -> Result<DocumentIndex, ()> {
    let mut block_ranges = Vec::new();
    let mut block_ids_by_address = BTreeMap::new();
    let walked = crate::walker::try_walk_block_slice(document.blocks(), |node| {
        if checkpoint.is_cancelled() {
            return std::ops::ControlFlow::Break(());
        }
        if let crate::walker::SemanticNode::Block(block) = node {
            let id = BlockId(u32::try_from(block_ranges.len()).expect("block count fits u32"));
            block_ids_by_address.insert(block as *const crate::block_model::AstBlock, id);
            block_ranges.push(block.range());
        }
        std::ops::ControlFlow::Continue(())
    });
    if walked.is_break() {
        return Err(());
    }
    let mut top_level_blocks = Vec::with_capacity(document.blocks().len());
    for block in document.blocks() {
        if checkpoint.is_cancelled() {
            return Err(());
        }
        top_level_blocks.push(
            block_ids_by_address
                .get(&(block as *const crate::block_model::AstBlock))
                .copied()
                .expect("the shared topology visits every top-level block"),
        );
    }
    let mut top_level_ordinals = vec![None; block_ranges.len()];
    for (ordinal, id) in top_level_blocks.iter().copied().enumerate() {
        if checkpoint.is_cancelled() {
            return Err(());
        }
        top_level_ordinals[id.get() as usize] = Some(ordinal);
    }
    let mut block_ids_by_range = BTreeMap::new();
    for (index, range) in block_ranges.iter().copied().enumerate() {
        if checkpoint.is_cancelled() {
            return Err(());
        }
        block_ids_by_range
            .entry(range)
            .or_insert_with(|| BlockId(u32::try_from(index).expect("block count fits u32")));
    }
    Ok(DocumentIndex {
        block_ranges,
        block_ids_by_range,
        top_level_blocks,
        top_level_ordinals,
    })
}

pub(crate) fn build_presentation(
    document: &AstDocument,
    structure: &crate::structure::DocumentStructure,
    index: &DocumentIndex,
    attribute_environment: &crate::attributes::AttributeEnvironment,
    checkpoint: &mut crate::cancellation::CancellationCheckpoint<'_>,
) -> Result<DocumentPresentation, ()> {
    // These document-wide presentation controls are header metadata. Body
    // bindings remain queryable but do not retroactively reconfigure them.
    let header_offset = document.header().end;
    let header_values = |name: &str| {
        attribute_environment
            .resolve_at(name, header_offset)
            .and_then(|resolved| resolved.value.ok().flatten())
    };
    let source_language = header_values("source-language")
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned);
    let toclevels = header_values("toclevels");
    let max_level = toclevels
        .and_then(|value| value.trim().parse::<u8>().ok())
        .filter(|level| (1..=5).contains(level));
    let invalid_level_range = toclevels.filter(|_| max_level.is_none()).and_then(|_| {
        document
            .attributes()
            .iter()
            .rev()
            .find(|attribute| {
                attribute.name == "toclevels" && attribute.range.end() <= header_offset
            })
            .map(|attribute| attribute.value.source_range)
    });
    let toc_policy = TocPolicy {
        enabled: header_values("toc").is_some(),
        max_level,
        invalid_level_range,
    };
    let mut section_numbers = header_values("sectnums").is_some();
    let mut counters = [0_u32; 6];
    let mut headings = Vec::with_capacity(structure.headings().len());
    for heading in structure.headings() {
        if checkpoint.is_cancelled() {
            return Err(());
        }
        let numbered = attribute_environment
            .resolve_at("sectnums", heading.range.start())
            .and_then(|resolved| resolved.value.ok().flatten())
            .is_some();
        section_numbers |= numbered;
        let level_index = usize::from(heading.level.min(5));
        let number = if matches!(
            heading.kind,
            crate::structure::SectionKind::DocumentTitle | crate::structure::SectionKind::Discrete
        ) {
            Vec::new()
        } else {
            counters[level_index] += 1;
            counters[level_index + 1..].fill(0);
            counters[..=level_index]
                .iter()
                .copied()
                .filter(|number| *number != 0)
                .collect()
        };
        let block = index
            .block_id_at(heading.range)
            .expect("every structured heading is indexed");
        let toc_included = !matches!(
            heading.kind,
            crate::structure::SectionKind::DocumentTitle | crate::structure::SectionKind::Discrete
        ) && !index
            .top_level_ordinal(block)
            .and_then(|ordinal| document.blocks().get(ordinal))
            .is_some_and(|block| {
                block
                    .metadata()
                    .roles
                    .iter()
                    .any(|item| item.value == "notoc")
            });
        headings.push(HeadingPresentation {
            block,
            range: heading.range,
            number,
            numbered,
            toc_included,
        });
    }
    let mut heading_ordinals = BTreeMap::new();
    for (ordinal, heading) in headings.iter().enumerate() {
        if checkpoint.is_cancelled() {
            return Err(());
        }
        heading_ordinals.insert(heading.range, ordinal);
    }
    let toc = toc_entries(
        structure.roots(),
        &headings,
        &heading_ordinals,
        toc_policy.max_level,
        checkpoint,
    )?;
    let mut bibliography_sections = Vec::new();
    for block in document.blocks() {
        if checkpoint.is_cancelled() {
            return Err(());
        }
        let crate::block_model::AstBlock::Heading(heading) = block else {
            continue;
        };
        if block
            .metadata()
            .attributes
            .iter()
            .any(|attribute| attribute.name.is_none() && attribute.value == "bibliography")
        {
            bibliography_sections.push(BibliographySection {
                block: index
                    .block_id_at(heading.range)
                    .expect("every heading is indexed"),
                range: heading.range,
            });
        }
    }
    let mut bibliography_ordinals = BTreeMap::new();
    for (ordinal, section) in bibliography_sections.iter().enumerate() {
        if checkpoint.is_cancelled() {
            return Err(());
        }
        bibliography_ordinals.insert(section.range, ordinal);
    }
    Ok(DocumentPresentation {
        source_language,
        toc_policy,
        section_numbers,
        headings,
        heading_ordinals,
        toc,
        bibliography_sections,
        bibliography_ordinals,
    })
}

fn toc_entries(
    sections: &[crate::structure::Section],
    headings: &[HeadingPresentation],
    heading_ordinals: &BTreeMap<TextRange, usize>,
    max_level: Option<u8>,
    checkpoint: &mut crate::cancellation::CancellationCheckpoint<'_>,
) -> Result<Vec<crate::structure::TocEntry>, ()> {
    let mut entries = Vec::new();
    for section in sections {
        if checkpoint.is_cancelled() {
            return Err(());
        }
        if max_level.is_some_and(|max_level| section.heading.level > max_level) {
            continue;
        }
        let children = toc_entries(
            &section.children,
            headings,
            heading_ordinals,
            max_level,
            checkpoint,
        )?;
        let presentation = heading_ordinals
            .get(&section.heading.range)
            .and_then(|ordinal| headings.get(*ordinal))
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
    Ok(entries)
}

pub(crate) fn build_layout(
    document: &AstDocument,
    index: &DocumentIndex,
    presentation: &DocumentPresentation,
    checkpoint: &mut crate::cancellation::CancellationCheckpoint<'_>,
) -> Result<DocumentLayout, ()> {
    fn structural_heading_level(block: &crate::block_model::AstBlock) -> Option<u8> {
        let crate::block_model::AstBlock::Heading(heading) = block else {
            return None;
        };
        match heading.kind {
            crate::block_model::HeadingKind::DocumentTitle
            | crate::block_model::HeadingKind::Part => Some(0),
            crate::block_model::HeadingKind::Section { level } => Some(level),
            crate::block_model::HeadingKind::Discrete { .. } => None,
        }
    }

    let mut nodes = Vec::new();
    let mut bibliography_scope: Option<(u8, Vec<LayoutNode>)> = None;
    for id in index.top_level_blocks().iter().copied() {
        if checkpoint.is_cancelled() {
            return Err(());
        }
        let block = index
            .top_level_ordinal(id)
            .and_then(|ordinal| document.blocks().get(ordinal))
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

        let bibliography_level = matches!(block, crate::block_model::AstBlock::Heading(heading)
            if presentation.bibliography_section_at(heading.range).is_some())
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
    if presentation.toc_policy().enabled {
        let mut insertion = None;
        for (node_index, node) in nodes.iter().enumerate() {
            if checkpoint.is_cancelled() {
                return Err(());
            }
            let LayoutNode::Block(id) = node else {
                continue;
            };
            if matches!(
                index
                    .top_level_ordinal(*id)
                    .and_then(|ordinal| document.blocks().get(ordinal)),
                Some(crate::block_model::AstBlock::Heading(heading))
                    if matches!(heading.kind, crate::block_model::HeadingKind::DocumentTitle)
            ) {
                insertion = Some(node_index + 1);
                break;
            }
        }
        nodes.insert(
            insertion.unwrap_or(0),
            LayoutNode::Generated(GeneratedLayoutNode::TableOfContents),
        );
    }
    nodes.push(LayoutNode::Generated(GeneratedLayoutNode::FootnoteCatalog));
    Ok(DocumentLayout { nodes })
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::{GeneratedLayoutNode, LayoutNode, LayoutScope, build_index};
    use crate::parser::parse;

    #[test]
    fn document_index_build_cancels_during_the_block_walk() {
        struct CancelAfterFirstCheckpoint(AtomicUsize);

        impl crate::core::CancellationCheck for CancelAfterFirstCheckpoint {
            fn is_cancelled(&self) -> bool {
                self.0.fetch_add(1, Ordering::Relaxed) >= 1
            }
        }

        let source = (0..crate::cancellation::CHECKPOINT_INTERVAL * 2)
            .map(|index| format!("paragraph {index}\n\n"))
            .collect::<String>();
        let parsed = parse(&source).expect("parse");
        let cancellation = CancelAfterFirstCheckpoint(AtomicUsize::new(0));

        let result = build_index(
            &parsed.ast,
            &mut crate::cancellation::CancellationCheckpoint::new(&cancellation),
        );

        assert_eq!(result, Err(()));
        assert_eq!(cancellation.0.load(Ordering::Relaxed), 2);
    }

    #[test]
    fn resolves_final_attributes_and_indexes_layout_without_ranges_as_ids() {
        let parsed = parse(
            "= Title\n:source-language: rust\n:source-language!:\n:toc:\n:toclevels: 3\n:sectnums:\n\nfirst\n\nsecond\n",
        )
        .expect("parse");
        let document = parsed.ast;

        assert_eq!(
            document
                .attribute_environment()
                .final_values()
                .get("source-language"),
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
