//! Semantic lowering from parser facts into the output-independent document model.

use std::collections::BTreeSet;

use crate::attributes::DocumentAttribute;
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
    let resolved_attributes = crate::presentation::resolve_document_attributes(&facts.attributes);
    let source_language = resolved_attributes
        .get("source-language")
        .map(str::trim)
        .filter(|value| !value.is_empty());
    facts.blocks = normalize_verbatim_blocks(facts.blocks, source_language);
    resolve_delimited_presentations(&mut facts.blocks);
    attach_anchors(&mut facts.anchors, &facts.blocks);
    facts.header.doctype = document_type(&facts.attributes);
    let mut document =
        AstDocument::new(facts.blocks, facts.attributes, facts.anchors, facts.header);
    document.normalize_heading_kinds();
    resolve_inline_attributes(
        &mut document,
        &resolved_attributes,
        facts.attribute_expansion_limits,
    );
    configure_tables(&mut document.blocks);
    document.identifiers = crate::document::build_identifiers(&document);
    document.structure = crate::structure::build(&document);
    document.index = crate::presentation::build_index(&document);
    document.presentation = crate::presentation::build_presentation(&document, resolved_attributes);
    document.layout = crate::presentation::build_layout(&document);
    document
}

fn resolve_delimited_presentations(blocks: &mut [AstBlock]) {
    crate::walker::walk_blocks_mut(blocks, &mut |block: &mut AstBlock| {
        if let AstBlock::Delimited(block) = block {
            resolve_delimited_presentation(block);
        }
    });
}

fn resolve_delimited_presentation(block: &mut crate::parser::DelimitedBlock) {
    let positional: Vec<_> = block
        .metadata
        .attributes
        .iter()
        .filter(|attribute| attribute.name.is_none())
        .collect();
    let style = positional.first().map(|attribute| attribute.value.as_str());
    block.presentation = match (block.kind, style) {
        (crate::parser::DelimitedBlockKind::Example, Some(style))
        | (crate::parser::DelimitedBlockKind::Open, Some(style))
            if crate::parser::AdmonitionKind::parse(style).is_some() =>
        {
            let attribute = positional[0];
            Some(crate::parser::DelimitedPresentation::Admonition(
                crate::parser::AdmonitionPresentation {
                    kind: crate::parser::AdmonitionKind::parse(&attribute.value)
                        .expect("guarded admonition style"),
                    label_range: attribute.range,
                },
            ))
        }
        (crate::parser::DelimitedBlockKind::Quote, Some("quote")) => Some(
            crate::parser::DelimitedPresentation::Quote(crate::parser::QuotePresentation {
                kind: crate::parser::QuoteKind::Quote,
                attribution: positional
                    .get(1)
                    .map(|attribute| crate::parser::MetadataValue {
                        value: attribute.value.clone(),
                        range: attribute.range,
                    }),
                citation: positional
                    .get(2)
                    .map(|attribute| crate::parser::MetadataValue {
                        value: attribute.value.clone(),
                        range: attribute.range,
                    }),
            }),
        ),
        (crate::parser::DelimitedBlockKind::Quote, Some("verse")) => Some(
            crate::parser::DelimitedPresentation::Quote(crate::parser::QuotePresentation {
                kind: crate::parser::QuoteKind::Verse,
                attribution: positional
                    .get(1)
                    .map(|attribute| crate::parser::MetadataValue {
                        value: attribute.value.clone(),
                        range: attribute.range,
                    }),
                citation: positional
                    .get(2)
                    .map(|attribute| crate::parser::MetadataValue {
                        value: attribute.value.clone(),
                        range: attribute.range,
                    }),
            }),
        ),
        _ => None,
    };
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
            let implicit_listing = block.kind == crate::parser::DelimitedBlockKind::Listing
                && !block
                    .metadata
                    .attributes
                    .iter()
                    .any(|attribute| attribute.name.is_none() && attribute.value == "listing");
            if implicit_listing
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
            if let Some(kind) = kind
                && let crate::parser::DelimitedContent::Verbatim(value) = block.content
            {
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
    let mut problems = Vec::new();
    for attribute in &list.metadata.attributes {
        match attribute.name.as_deref() {
            Some("start") => {
                let start = attribute
                    .value
                    .trim()
                    .parse::<u32>()
                    .ok()
                    .filter(|value| *value > 0);
                if start.is_none() {
                    problems.push(crate::parser::ListPresentationProblem {
                        kind: crate::parser::ListPresentationProblemKind::InvalidStart,
                        range: attribute.range,
                    });
                }
                presentation.start = start;
            }
            Some("style") => {
                if let Some(style) = ordered_list_style(&attribute.value) {
                    presentation.style = style;
                } else {
                    problems.push(crate::parser::ListPresentationProblem {
                        kind: crate::parser::ListPresentationProblemKind::UnknownOrderedStyle,
                        range: attribute.range,
                    });
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
    if presentation.start.is_none() {
        presentation.start = list.items.first().and_then(|item| item.explicit_number);
    }
    let mut expected = presentation.start.unwrap_or(1);
    for item in &list.items {
        if item.invalid_explicit_number {
            problems.push(crate::parser::ListPresentationProblem {
                kind: crate::parser::ListPresentationProblemKind::InvalidExplicitNumber,
                range: item.marker_range,
            });
        }
        if let Some(number) = item.explicit_number
            && number != expected
        {
            problems.push(crate::parser::ListPresentationProblem {
                kind: crate::parser::ListPresentationProblemKind::InconsistentExplicitNumber,
                range: item.marker_range,
            });
        }
        expected = if presentation.reversed {
            expected.saturating_sub(1)
        } else {
            expected.saturating_add(1)
        };
    }
    list.presentation = presentation;
    list.presentation_problems = problems;
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
        if let AstBlock::Delimited(block) = block
            && let crate::parser::DelimitedContent::Table(table) = &mut block.content
        {
            crate::table::configure(table, &block.metadata);
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

fn resolve_inline_attributes(
    document: &mut AstDocument,
    attributes: &crate::presentation::ResolvedDocumentAttributes,
    limits: AttributeExpansionLimits,
) {
    let evaluator = AttributeEvaluator::new(attributes.values(), limits);
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
