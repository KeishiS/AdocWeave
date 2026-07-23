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
    let source_language = source_language(&facts.attributes);
    facts.blocks = normalize_verbatim_blocks(facts.blocks, source_language.as_deref());
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

fn source_language(attributes: &[DocumentAttribute]) -> Option<String> {
    let mut language = None;
    for attribute in attributes {
        if attribute.name != "source-language" {
            continue;
        }
        match attribute.operation {
            AttributeOperation::Set if !attribute.raw_value.trim().is_empty() => {
                language = Some(attribute.raw_value.trim().to_owned());
            }
            AttributeOperation::Set | AttributeOperation::Unset => language = None,
        }
    }
    language
}

fn normalize_verbatim_blocks(
    blocks: Vec<AstBlock>,
    source_language: Option<&str>,
) -> Vec<AstBlock> {
    blocks
        .into_iter()
        .map(|block| normalize_verbatim_block(block, source_language))
        .collect()
}

fn normalize_verbatim_block(block: AstBlock, source_language: Option<&str>) -> AstBlock {
    match block {
        AstBlock::Source(source) => AstBlock::Verbatim(crate::parser::VerbatimBlock {
            metadata: source.metadata,
            kind: crate::parser::VerbatimKind::Source(crate::parser::SourceInfo {
                attribute_range: source.attribute_range,
                language_range: source.language_range,
                language: source.language,
            }),
            range: source.range,
            delimiter_range: source.delimiter_range,
            content_range: source.content_range,
            value: source.value,
            callouts: source.callouts,
            problems: source.problems,
        }),
        AstBlock::Delimited(mut block) => {
            if let crate::parser::DelimitedContent::Compound(children) = &mut block.content {
                *children = normalize_verbatim_blocks(std::mem::take(children), source_language);
            }
            if block.kind == crate::parser::DelimitedBlockKind::Listing
                && !block
                    .metadata
                    .attributes
                    .iter()
                    .any(|attribute| attribute.name.is_none() && attribute.value == "listing")
                && let Some(language) = source_language
                && let crate::parser::DelimitedContent::Verbatim(value) = block.content
            {
                let attribute_range = block
                    .metadata
                    .range
                    .unwrap_or(block.opening_delimiter_range);
                return AstBlock::Verbatim(crate::parser::VerbatimBlock {
                    metadata: block.metadata,
                    kind: crate::parser::VerbatimKind::Source(crate::parser::SourceInfo {
                        attribute_range,
                        language_range: None,
                        language: Some(language.to_owned()),
                    }),
                    range: block.range,
                    delimiter_range: block.opening_delimiter_range,
                    content_range: block.content_range,
                    value,
                    callouts: Vec::new(),
                    problems: block.problems,
                });
            }
            let kind = match block.kind {
                crate::parser::DelimitedBlockKind::Listing => {
                    Some(crate::parser::VerbatimKind::Listing)
                }
                crate::parser::DelimitedBlockKind::Literal => {
                    Some(crate::parser::VerbatimKind::Literal)
                }
                _ => None,
            };
            if let Some(kind) = kind {
                if let crate::parser::DelimitedContent::Verbatim(value) = block.content {
                    return AstBlock::Verbatim(crate::parser::VerbatimBlock {
                        metadata: block.metadata,
                        kind,
                        range: block.range,
                        delimiter_range: block.opening_delimiter_range,
                        content_range: block.content_range,
                        value,
                        callouts: Vec::new(),
                        problems: block.problems,
                    });
                }
            }
            AstBlock::Delimited(block)
        }
        AstBlock::List(mut list) => {
            resolve_list_presentation(&mut list);
            for item in &mut list.items {
                for child in &mut item.children {
                    normalize_list(child, source_language);
                }
                item.continuations = normalize_verbatim_blocks(
                    std::mem::take(&mut item.continuations),
                    source_language,
                );
            }
            AstBlock::List(list)
        }
        other => other,
    }
}

fn normalize_list(list: &mut crate::parser::ListBlock, source_language: Option<&str>) {
    resolve_list_presentation(list);
    for item in &mut list.items {
        for child in &mut item.children {
            normalize_list(child, source_language);
        }
        item.continuations =
            normalize_verbatim_blocks(std::mem::take(&mut item.continuations), source_language);
    }
}

fn resolve_list_presentation(list: &mut crate::parser::ListBlock) {
    if list.kind != crate::parser::ListKind::Ordered {
        return;
    }

    let mut presentation = crate::parser::OrderedListPresentation::default();
    for attribute in &list.metadata.attributes {
        match attribute.name.as_deref() {
            Some("start") => {
                presentation.start = attribute
                    .value
                    .trim()
                    .parse::<u32>()
                    .ok()
                    .filter(|value| *value > 0);
            }
            Some("style") => {
                if let Some(style) = ordered_list_style(&attribute.value) {
                    presentation.style = style;
                }
            }
            Some("options") => {
                if attribute
                    .value
                    .split(',')
                    .any(|option| option.trim() == "reversed")
                {
                    presentation.reversed = true;
                }
            }
            None => {
                if attribute.value == "reversed" {
                    presentation.reversed = true;
                } else if let Some(style) = ordered_list_style(&attribute.value) {
                    presentation.style = style;
                }
            }
            Some(_) => {}
        }
    }
    if list
        .metadata
        .options
        .iter()
        .any(|option| option.value == "reversed")
    {
        presentation.reversed = true;
    }
    list.presentation = presentation;
}

fn ordered_list_style(value: &str) -> Option<crate::parser::OrderedListStyle> {
    use crate::parser::OrderedListStyle;

    match value.trim() {
        "arabic" => Some(OrderedListStyle::Arabic),
        "decimal" => Some(OrderedListStyle::Decimal),
        "loweralpha" => Some(OrderedListStyle::LowerAlpha),
        "upperalpha" => Some(OrderedListStyle::UpperAlpha),
        "lowerroman" => Some(OrderedListStyle::LowerRoman),
        "upperroman" => Some(OrderedListStyle::UpperRoman),
        "lowergreek" => Some(OrderedListStyle::LowerGreek),
        _ => None,
    }
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
