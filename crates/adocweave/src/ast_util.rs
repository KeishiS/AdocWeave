//! Queries and traversal helpers for the backend-independent block model.

use std::fmt::Write as _;

use crate::block_model::*;

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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::block_model::{BlockMetadata, DocumentHeader, Heading, HeadingKind, Paragraph};

    fn range() -> crate::source::TextRange {
        crate::source::TextRange::new(crate::source::TextSize::ZERO, crate::source::TextSize::ZERO)
            .expect("zero range")
    }

    fn heading(kind: HeadingKind) -> AstBlock {
        AstBlock::Heading(Heading {
            metadata: BlockMetadata::default(),
            range: range(),
            marker_range: range(),
            separator_range: range(),
            text_range: range(),
            kind,
            well_formed: true,
            hierarchy_valid: true,
            text: String::new(),
            inlines: Vec::new(),
            inline_problems: Vec::new(),
            problems: Vec::new(),
        })
    }

    fn paragraph(value: &str) -> AstBlock {
        AstBlock::Paragraph(Paragraph {
            metadata: BlockMetadata::default(),
            range: range(),
            content_range: range(),
            value: value.to_owned(),
            inlines: Vec::new(),
            admonition: None,
            inline_problems: Vec::new(),
        })
    }

    fn document(blocks: Vec<AstBlock>) -> AstDocument {
        AstDocument::new(
            blocks,
            Vec::new(),
            0,
            Vec::new(),
            DocumentHeader {
                range: None,
                authors: Vec::new(),
                revision: None,
                doctype: crate::block_model::DocumentType::Article,
                end: crate::source::TextSize::ZERO,
            },
        )
    }

    fn texts(blocks: &[AstBlock]) -> Vec<&str> {
        blocks
            .iter()
            .map(|block| match block {
                AstBlock::Paragraph(paragraph) => paragraph.value.as_str(),
                AstBlock::Heading(_) => "<heading>",
                _ => "<other>",
            })
            .collect()
    }

    /// The preamble is what sits between the document title and the first
    /// section.
    ///
    /// Both ends are found independently, so the cases where one end is missing
    /// decide the answer on their own and are worth stating separately.
    #[test]
    fn the_preamble_starts_after_the_title_and_ends_at_the_first_section() {
        let ast = document(vec![
            heading(HeadingKind::DocumentTitle),
            paragraph("lead"),
            paragraph("more"),
            heading(HeadingKind::Section { level: 1 }),
            paragraph("body"),
        ]);
        assert_eq!(texts(ast.preamble()), ["lead", "more"]);
    }

    /// With no section, everything after the title is the preamble.
    #[test]
    fn a_document_without_a_section_treats_the_rest_as_preamble() {
        let ast = document(vec![heading(HeadingKind::DocumentTitle), paragraph("only")]);
        assert_eq!(texts(ast.preamble()), ["only"]);
    }

    /// With no title, the preamble starts at the first block.
    #[test]
    fn a_document_without_a_title_starts_its_preamble_at_the_first_block() {
        let ast = document(vec![
            paragraph("lead"),
            heading(HeadingKind::Section { level: 1 }),
        ]);
        assert_eq!(texts(ast.preamble()), ["lead"]);

        let ast = document(vec![paragraph("lead"), paragraph("more")]);
        assert_eq!(texts(ast.preamble()), ["lead", "more"]);
    }

    /// A section or part immediately after the title leaves no preamble.
    ///
    /// A part ends the preamble for the same reason a section does: both begin
    /// the body of the document.
    #[test]
    fn a_section_or_part_right_after_the_title_leaves_no_preamble() {
        for kind in [HeadingKind::Section { level: 1 }, HeadingKind::Part] {
            let ast = document(vec![heading(HeadingKind::DocumentTitle), heading(kind)]);
            assert!(ast.preamble().is_empty(), "{kind:?}");

            // The same holds when the body starts the document outright.
            let ast = document(vec![heading(kind), paragraph("body")]);
            assert!(ast.preamble().is_empty(), "{kind:?}");
        }
    }

    /// A discrete heading does not end the preamble.
    ///
    /// It carries no place in the hierarchy, so the document body has not
    /// started yet.
    #[test]
    fn a_discrete_heading_does_not_end_the_preamble() {
        let ast = document(vec![
            heading(HeadingKind::DocumentTitle),
            heading(HeadingKind::Discrete { level: 2 }),
            paragraph("still lead"),
            heading(HeadingKind::Section { level: 1 }),
        ]);
        assert_eq!(texts(ast.preamble()), ["<heading>", "still lead"]);
    }

    /// An empty document and a title-only document both have no preamble.
    #[test]
    fn an_empty_or_title_only_document_has_no_preamble() {
        assert!(document(Vec::new()).preamble().is_empty());
        assert!(
            document(vec![heading(HeadingKind::DocumentTitle)])
                .preamble()
                .is_empty()
        );
        assert!(
            document(vec![
                heading(HeadingKind::DocumentTitle),
                heading(HeadingKind::DocumentTitle),
            ])
            .preamble()
            .is_empty()
        );
    }

    /// Header attributes are the leading run of the recorded attributes.
    ///
    /// The count decides the split, so the document keeps one list and the
    /// header view is a slice of it rather than a second copy.
    #[test]
    fn header_attributes_are_the_leading_run_of_all_attributes() {
        fn occurrence(name: &str) -> crate::attributes::DocumentAttributeOccurrence {
            crate::attributes::DocumentAttributeOccurrence {
                range: range(),
                name_range: range(),
                name: name.to_owned(),
                value: crate::attributes::DocumentAttributeValue {
                    source_range: range(),
                    source_text: String::new(),
                    folded_text: String::new(),
                    lines: Vec::new(),
                },
                operation: crate::attributes::DocumentAttributeOperation::Set,
                valid: true,
            }
        }

        let ast = AstDocument::new(
            Vec::new(),
            vec![occurrence("a"), occurrence("b"), occurrence("c")],
            2,
            Vec::new(),
            DocumentHeader {
                range: None,
                authors: Vec::new(),
                revision: None,
                doctype: crate::block_model::DocumentType::Article,
                end: crate::source::TextSize::ZERO,
            },
        );
        assert_eq!(ast.attributes().len(), 3);
        let header = ast
            .header_attributes()
            .iter()
            .map(|attribute| attribute.name.as_str())
            .collect::<Vec<_>>();
        assert_eq!(header, ["a", "b"]);
    }

    /// The node count includes the document itself and each block's metadata.
    ///
    /// It starts at one for the document, so an empty document is one node
    /// rather than none. Every block then contributes two: the block and the
    /// metadata node the walker always visits for it, whether or not any
    /// metadata was written. A caller sizing work from this number is counting
    /// the traversal, not the blocks.
    #[test]
    fn the_node_count_covers_the_document_and_every_block_with_its_metadata() {
        assert_eq!(document(Vec::new()).node_count(), 1);
        assert_eq!(document(vec![paragraph("one")]).node_count(), 3);
        assert_eq!(
            document(vec![paragraph("one"), heading(HeadingKind::Part)]).node_count(),
            5
        );
    }
}
