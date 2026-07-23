//! Semantic lowering from parser facts into the output-independent document model.

use std::collections::{BTreeMap, BTreeSet};

use crate::attributes::{AttributeOperation, DocumentAttribute};
use crate::inline::Inline;
use crate::parser::{AstBlock, AstDocument, DocumentHeader, DocumentType, ExplicitAnchor};
use crate::substitution::{AttributeEvaluator, AttributeExpansionLimits};

pub(crate) struct ParsedFacts {
    pub blocks: Vec<AstBlock>,
    pub attributes: Vec<DocumentAttribute>,
    pub anchors: Vec<ExplicitAnchor>,
    pub header: DocumentHeader,
    pub attribute_expansion_limits: AttributeExpansionLimits,
}

pub(crate) fn lower(mut facts: ParsedFacts) -> AstDocument {
    configure_tables(&mut facts.blocks);
    attach_anchors(&mut facts.anchors, &facts.blocks);
    facts.header.doctype = document_type(&facts.attributes);
    let mut document =
        AstDocument::new(facts.blocks, facts.attributes, facts.anchors, facts.header);
    document.normalize_heading_kinds();
    resolve_document_attributes(&mut document, facts.attribute_expansion_limits);
    document.identifiers = crate::document::build_identifiers(&document);
    document.structure = crate::structure::build(&document);
    document.index = crate::presentation::build_index(&document);
    document.presentation = crate::presentation::build_presentation(&document);
    document.layout = crate::presentation::build_layout(&document);
    document
}

fn configure_tables(blocks: &mut [AstBlock]) {
    crate::walker::walk_blocks_mut(blocks, &mut |block: &mut AstBlock| {
        if let AstBlock::Delimited(block) = block {
            if let crate::parser::DelimitedContent::Table(table) = &mut block.content {
                crate::table::configure(table, &block.metadata);
            }
        }
    });
}

fn document_type(attributes: &[DocumentAttribute]) -> DocumentType {
    let mut doctype = DocumentType::Article;
    for attribute in attributes
        .iter()
        .filter(|attribute| attribute.name == "doctype")
    {
        doctype = match attribute.raw_value.trim() {
            "book" => DocumentType::Book,
            "manpage" => DocumentType::Manpage,
            "inline" => DocumentType::Inline,
            _ => DocumentType::Article,
        };
    }
    doctype
}

fn attach_anchors(anchors: &mut [ExplicitAnchor], blocks: &[AstBlock]) {
    let mut ranges = Vec::new();
    crate::walker::walk_block_slice(blocks, |node| {
        if let crate::walker::SemanticNode::Block(block) = node {
            ranges.push(block.range());
        }
    });
    ranges.sort_unstable_by_key(|range| (range.start(), range.end()));
    for anchor in &mut *anchors {
        anchor.target_range = ranges
            .iter()
            .copied()
            .find(|range| range.start() >= anchor.range.end());
    }
    let mut anchored_targets = BTreeSet::new();
    for anchor in anchors {
        if anchor.valid {
            if let Some(target) = anchor.target_range {
                if !anchored_targets.insert((target.start().to_u32(), target.end().to_u32())) {
                    anchor.valid = false;
                }
            } else {
                anchor.valid = false;
            }
        }
    }
}

fn resolve_document_attributes(document: &mut AstDocument, limits: AttributeExpansionLimits) {
    let mut attributes = BTreeMap::new();
    for attribute in document.attributes() {
        match &attribute.operation {
            AttributeOperation::Set => {
                attributes.insert(attribute.name.clone(), attribute.raw_value.clone());
            }
            AttributeOperation::Unset => {
                attributes.remove(&attribute.name);
            }
        }
    }

    let evaluator = AttributeEvaluator::new(&attributes, limits);
    document.visit_inline_sequences_mut(|inlines| resolve_inlines(inlines, &evaluator));
}

fn resolve_inlines(inlines: &mut [Inline], evaluator: &AttributeEvaluator<'_>) {
    for inline in inlines {
        match inline {
            Inline::Link(link) => {
                match evaluator.expand_text(&link.target_source) {
                    Ok(value) => {
                        link.target = value;
                        link.target_expansion_error = None;
                    }
                    Err(error) => {
                        link.target = link.target_source.clone();
                        link.target_expansion_error = Some(error);
                    }
                }
                resolve_inlines(&mut link.label, evaluator);
            }
            Inline::Reference(reference) => resolve_inlines(&mut reference.label, evaluator),
            Inline::Macro(node) => match evaluator.expand_text(&node.target_source) {
                Ok(value) => {
                    node.target = value;
                    node.target_expansion_error = None;
                }
                Err(error) => {
                    node.target = node.target_source.clone();
                    node.target_expansion_error = Some(error);
                }
            },
            Inline::Styled { children, .. } => resolve_inlines(children, evaluator),
            Inline::AttributeReference {
                name,
                value,
                expansion_error,
                ..
            } => match evaluator.expand_name(name) {
                Ok(resolved) => {
                    *value = Some(resolved);
                    *expansion_error = None;
                }
                Err(error) => {
                    *value = None;
                    *expansion_error = Some(error);
                }
            },
            Inline::Text(text) => {
                text.value = crate::substitution::apply_replacements(&text.value);
            }
            Inline::Literal { .. }
            | Inline::HardBreak { .. }
            | Inline::Passthrough { .. }
            | Inline::Formula(_) => {}
        }
    }
}
