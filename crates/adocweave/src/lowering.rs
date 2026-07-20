//! Semantic lowering from parser facts into the output-independent document model.

use std::collections::{BTreeMap, BTreeSet};

use crate::attributes::{AttributeOperation, AttributeProblem, DocumentAttribute};
use crate::inline::Inline;
use crate::parser::{AstBlock, AstDocument, ExplicitAnchor};

pub(crate) struct ParsedFacts {
    pub blocks: Vec<AstBlock>,
    pub attributes: Vec<DocumentAttribute>,
    pub attribute_problems: Vec<AttributeProblem>,
    pub anchors: Vec<ExplicitAnchor>,
}

pub(crate) fn lower(mut facts: ParsedFacts) -> AstDocument {
    attach_anchors(&mut facts.anchors, &facts.blocks);
    let mut document = AstDocument::new(
        facts.blocks,
        facts.attributes,
        facts.attribute_problems,
        facts.anchors,
    );
    resolve_document_attributes(&mut document);
    document
}

fn attach_anchors(anchors: &mut [ExplicitAnchor], blocks: &[AstBlock]) {
    for anchor in &mut *anchors {
        anchor.target_range = blocks
            .iter()
            .map(AstBlock::range)
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

fn resolve_document_attributes(document: &mut AstDocument) {
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

    document.visit_inline_sequences_mut(|inlines| resolve_inlines(inlines, &attributes));
}

fn resolve_inlines(inlines: &mut [Inline], attributes: &BTreeMap<String, String>) {
    for inline in inlines {
        match inline {
            Inline::Link(link) => {
                let mut value = String::new();
                let mut cursor = 0;
                for attribute in &link.target_attributes {
                    let name_start = attribute.name_range.start().to_usize()
                        - link.target_range.start().to_usize();
                    let name_end = attribute.name_range.end().to_usize()
                        - link.target_range.start().to_usize();
                    let open = name_start.saturating_sub(1);
                    let close_end = (name_end + 1).min(link.target_source.len());
                    value.push_str(&link.target_source[cursor..open]);
                    if let Some(replacement) = attributes.get(&attribute.name) {
                        value.push_str(replacement);
                    } else {
                        value.push_str(&link.target_source[open..close_end]);
                    }
                    cursor = close_end;
                }
                value.push_str(&link.target_source[cursor..]);
                link.target = value;
                resolve_inlines(&mut link.label, attributes);
            }
            Inline::Reference(reference) => resolve_inlines(&mut reference.label, attributes),
            Inline::Styled { children, .. } => resolve_inlines(children, attributes),
            Inline::Text(_)
            | Inline::Literal { .. }
            | Inline::AttributeReference { .. }
            | Inline::Formula(_) => {}
        }
    }
}
