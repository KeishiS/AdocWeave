//! Queries and traversal helpers for the backend-independent block model.

use std::fmt::Write as _;

use crate::block_model::*;
use crate::inline::Inline;

impl AstDocument {
    pub(crate) fn new(
        blocks: Vec<AstBlock>,
        attributes: Vec<crate::attributes::DocumentAttributeOccurrence>,
        header_attribute_count: usize,
        anchors: Vec<ExplicitAnchor>,
        header: DocumentHeader,
    ) -> Self {
        Self {
            blocks,
            attributes,
            header_attribute_count,
            anchors,
            header,
            resolved: crate::resolved::ResolvedDocument::default(),
        }
    }

    pub fn blocks(&self) -> &[AstBlock] {
        &self.blocks
    }

    pub fn top_level_block(&self, id: crate::presentation::BlockId) -> Option<&AstBlock> {
        self.resolved
            .index()
            .top_level_ordinal(id)
            .and_then(|ordinal| self.blocks.get(ordinal))
    }

    pub(crate) fn attributes(&self) -> &[crate::attributes::DocumentAttributeOccurrence] {
        &self.attributes
    }

    pub(crate) fn header_attributes(&self) -> &[crate::attributes::DocumentAttributeOccurrence] {
        &self.attributes[..self.header_attribute_count]
    }

    pub fn anchors(&self) -> &[ExplicitAnchor] {
        &self.anchors
    }

    pub const fn header(&self) -> &DocumentHeader {
        &self.header
    }

    pub const fn catalogs(&self) -> &crate::catalog::DocumentCatalogs {
        self.resolved.catalogs()
    }

    pub const fn identifiers(&self) -> &crate::document::DocumentIdentifiers {
        self.resolved.identifiers()
    }

    pub const fn structure(&self) -> &crate::structure::DocumentStructure {
        self.resolved.structure()
    }

    #[cfg(test)]
    pub(crate) const fn index(&self) -> &crate::presentation::DocumentIndex {
        self.resolved.index()
    }

    pub const fn presentation(&self) -> &crate::presentation::DocumentPresentation {
        self.resolved.presentation()
    }

    pub(crate) const fn attribute_environment(&self) -> &crate::attributes::AttributeEnvironment {
        self.resolved.attribute_environment()
    }

    pub const fn layout(&self) -> &crate::presentation::DocumentLayout {
        self.resolved.layout()
    }

    pub fn preamble(&self) -> &[AstBlock] {
        let end = self
            .blocks
            .iter()
            .position(|block| {
                matches!(
                    block,
                    AstBlock::Heading(Heading {
                        kind: HeadingKind::Section { .. } | HeadingKind::Part,
                        ..
                    })
                )
            })
            .unwrap_or(self.blocks.len());
        let start = self
            .blocks
            .iter()
            .position(|block| {
                !matches!(
                    block,
                    AstBlock::Heading(Heading {
                        kind: HeadingKind::DocumentTitle,
                        ..
                    })
                )
            })
            .unwrap_or(end);
        &self.blocks[start.min(end)..end]
    }

    pub(crate) fn visit_blocks_mut(&mut self, mut visitor: impl FnMut(&mut AstBlock)) {
        crate::walker::walk_blocks_mut(&mut self.blocks, &mut visitor);
    }

    pub fn node_count(&self) -> usize {
        let mut count = 1;
        crate::walker::walk_ast(self, |_| count += 1);
        count
    }

    pub fn snapshot(&self) -> String {
        let mut output = String::from("Document\n");
        for block in &self.blocks {
            match block {
                AstBlock::Heading(heading) => {
                    writeln!(
                        output,
                        "  {:?}@{}..{} marker={}..{} text={}..{} {:?} problems={:?}",
                        heading.kind,
                        heading.range.start().to_u32(),
                        heading.range.end().to_u32(),
                        heading.marker_range.start().to_u32(),
                        heading.marker_range.end().to_u32(),
                        heading.text_range.start().to_u32(),
                        heading.text_range.end().to_u32(),
                        heading.text,
                        heading.problems
                    )
                    .expect("writing to a String cannot fail");
                }
                AstBlock::Paragraph(paragraph) => {
                    writeln!(
                        output,
                        "  Paragraph@{}..{}",
                        paragraph.range.start().to_u32(),
                        paragraph.range.end().to_u32()
                    )
                    .expect("writing to a String cannot fail");
                    writeln!(
                        output,
                        "    Text@{}..{} {:?}",
                        paragraph.content_range.start().to_u32(),
                        paragraph.content_range.end().to_u32(),
                        paragraph.value
                    )
                    .expect("writing to a String cannot fail");
                }
                AstBlock::LiteralParagraph(paragraph) => {
                    writeln!(
                        output,
                        "  LiteralParagraph@{}..{} {:?}",
                        paragraph.range.start().to_u32(),
                        paragraph.range.end().to_u32(),
                        paragraph.value
                    )
                    .expect("writing to a String cannot fail");
                }
                AstBlock::Break(block) => {
                    writeln!(
                        output,
                        "  {:?}Break@{}..{}",
                        block.kind,
                        block.range.start().to_u32(),
                        block.range.end().to_u32()
                    )
                    .expect("writing to a String cannot fail");
                }
                AstBlock::Source(source) => {
                    writeln!(
                        output,
                        "  Source@{}..{} language={:?} content={}..{} problems={:?}",
                        source.range.start().to_u32(),
                        source.range.end().to_u32(),
                        source.language,
                        source.content_range.start().to_u32(),
                        source.content_range.end().to_u32(),
                        source.problems
                    )
                    .expect("writing to a String cannot fail");
                }
                AstBlock::Verbatim(verbatim) => {
                    writeln!(
                        output,
                        "  Verbatim@{}..{} kind={:?} content={}..{} problems={:?}",
                        verbatim.range.start().to_u32(),
                        verbatim.range.end().to_u32(),
                        verbatim.kind,
                        verbatim.content_range.start().to_u32(),
                        verbatim.content_range.end().to_u32(),
                        verbatim.problems
                    )
                    .expect("writing to a String cannot fail");
                }
                AstBlock::List(list) => {
                    writeln!(
                        output,
                        "  {:?}List@{}..{} items={}",
                        list.kind,
                        list.range.start().to_u32(),
                        list.range.end().to_u32(),
                        list.items.len()
                    )
                    .expect("writing to a String cannot fail");
                }
                AstBlock::Math(math) => {
                    writeln!(
                        output,
                        "  Math({:?})@{}..{} content={}..{} {:?} problems={:?}",
                        math.language,
                        math.range.start().to_u32(),
                        math.range.end().to_u32(),
                        math.content_range.start().to_u32(),
                        math.content_range.end().to_u32(),
                        math.value,
                        math.problems
                    )
                    .expect("writing to a String cannot fail");
                }
                AstBlock::Delimited(block) => {
                    writeln!(
                        output,
                        "  {:?}@{}..{} delimiter={:?} content={}..{} {:?} problems={:?}",
                        block.kind,
                        block.range.start().to_u32(),
                        block.range.end().to_u32(),
                        block.delimiter,
                        block.content_range.start().to_u32(),
                        block.content_range.end().to_u32(),
                        block.content,
                        block.problems
                    )
                    .expect("writing to a String cannot fail");
                }
                AstBlock::Unsupported(unsupported) => {
                    writeln!(
                        output,
                        "  Unsupported@{}..{} {:?} ({})",
                        unsupported.range.start().to_u32(),
                        unsupported.range.end().to_u32(),
                        unsupported.raw,
                        unsupported.reason
                    )
                    .expect("writing to a String cannot fail");
                }
            }
        }
        output
    }

    pub(crate) fn visit_inline_sequences_mut(&mut self, mut visitor: impl FnMut(&mut Vec<Inline>)) {
        crate::walker::walk_inline_sequences_mut(&mut self.blocks, &mut visitor);
    }
}

impl AstBlock {
    pub const fn metadata(&self) -> &BlockMetadata {
        match self {
            Self::Heading(value) => &value.metadata,
            Self::Paragraph(value) => &value.metadata,
            Self::LiteralParagraph(value) => &value.metadata,
            Self::Break(value) => &value.metadata,
            Self::Source(value) => &value.metadata,
            Self::Verbatim(value) => &value.metadata,
            Self::List(value) => &value.metadata,
            Self::Math(value) => &value.metadata,
            Self::Delimited(value) => &value.metadata,
            Self::Unsupported(value) => &value.metadata,
        }
    }

    pub(crate) fn metadata_mut(&mut self) -> &mut BlockMetadata {
        match self {
            Self::Heading(value) => &mut value.metadata,
            Self::Paragraph(value) => &mut value.metadata,
            Self::LiteralParagraph(value) => &mut value.metadata,
            Self::Break(value) => &mut value.metadata,
            Self::Source(value) => &mut value.metadata,
            Self::Verbatim(value) => &mut value.metadata,
            Self::List(value) => &mut value.metadata,
            Self::Math(value) => &mut value.metadata,
            Self::Delimited(value) => &mut value.metadata,
            Self::Unsupported(value) => &mut value.metadata,
        }
    }

    pub const fn range(&self) -> crate::source::TextRange {
        match self {
            Self::Heading(value) => value.range,
            Self::Paragraph(value) => value.range,
            Self::LiteralParagraph(value) => value.range,
            Self::Break(value) => value.range,
            Self::Source(value) => value.range,
            Self::Verbatim(value) => value.range,
            Self::List(value) => value.range,
            Self::Math(value) => value.range,
            Self::Delimited(value) => value.range,
            Self::Unsupported(value) => value.range,
        }
    }
}
