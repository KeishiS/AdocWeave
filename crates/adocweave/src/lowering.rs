//! Semantic lowering from parser facts into the output-independent document model.

use std::collections::BTreeSet;

use crate::attributes::DocumentAttributeOccurrence;
use crate::inline::Inline;
use crate::parser::{AstBlock, AstDocument, DocumentHeader, DocumentType, ExplicitAnchor};
use crate::substitution::AttributeExpansionLimits;

pub(crate) struct ParsedFacts<'a> {
    pub blocks: Vec<AstBlock>,
    pub attributes: Vec<DocumentAttributeOccurrence>,
    pub header_attribute_count: usize,
    pub anchors: Vec<ExplicitAnchor>,
    pub header: DocumentHeader,
    pub attribute_expansion_limits: AttributeExpansionLimits,
    pub processing_limits: crate::limits::AnalysisLimits,
    pub external_attributes: &'a std::collections::BTreeMap<String, Option<String>>,
}

pub(crate) fn lower(
    mut facts: ParsedFacts<'_>,
) -> Result<AstDocument, crate::catalog::CatalogLimitExceeded> {
    let attribute_environment = crate::attributes::AttributeEnvironment::build(
        &facts.attributes,
        facts.external_attributes,
        facts.attribute_expansion_limits,
    );
    facts.blocks = normalize_verbatim_blocks(facts.blocks, &attribute_environment);
    resolve_delimited_presentations(&mut facts.blocks);
    attach_anchors(&mut facts.anchors, &facts.blocks);
    facts.header.doctype = document_type(&attribute_environment, facts.header.end);
    let mut document = AstDocument::new(
        facts.blocks,
        facts.attributes,
        facts.header_attribute_count,
        facts.anchors,
        facts.header,
    );
    document.normalize_heading_kinds();
    resolve_inline_attributes(&mut document, &attribute_environment);
    document.resolved = crate::resolved::ResolvedDocument::build(
        &document,
        attribute_environment,
        facts.processing_limits,
    )?;
    Ok(document)
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
    attributes: &crate::attributes::AttributeEnvironment,
) -> Vec<AstBlock> {
    blocks
        .into_iter()
        .map(|block| normalize_verbatim_block(block, attributes))
        .collect()
}

fn normalize_verbatim_block(
    block: AstBlock,
    attributes: &crate::attributes::AttributeEnvironment,
) -> AstBlock {
    match block {
        AstBlock::Source(mut source) => {
            let info = source_info(
                source.attribute_range,
                source.language_range,
                source.language,
                &source.metadata,
                &mut source.problems,
            );
            AstBlock::Verbatim(crate::parser::VerbatimBlock {
                metadata: source.metadata,
                kind: crate::parser::VerbatimKind::Source(info),
                range: source.range,
                delimiter_range: source.delimiter_range,
                content_range: source.content_range,
                value: source.value,
                callouts: source.callouts,
                problems: source.problems,
            })
        }
        AstBlock::Delimited(mut block) => {
            match &mut block.content {
                crate::parser::DelimitedContent::Compound(children) => {
                    *children = normalize_verbatim_blocks(std::mem::take(children), attributes);
                }
                crate::parser::DelimitedContent::Table(table) => {
                    for row in &mut table.rows {
                        for cell in &mut row.cells {
                            if let crate::table::TableCellContent::AsciiDoc(children) =
                                &mut cell.content
                            {
                                *children =
                                    normalize_verbatim_blocks(std::mem::take(children), attributes);
                            }
                        }
                    }
                }
                crate::parser::DelimitedContent::Verbatim(_)
                | crate::parser::DelimitedContent::Passthrough(_) => {}
            }
            let implicit_listing = block.kind == crate::parser::DelimitedBlockKind::Listing
                && !block
                    .metadata
                    .attributes
                    .iter()
                    .any(|attribute| attribute.name.is_none() && attribute.value == "listing");
            if implicit_listing
                && let Some(language) = attributes
                    .resolve_at("source-language", block.range.start())
                    .and_then(|resolved| resolved.value.ok().flatten())
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
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
                        line_numbers: false,
                        start_line: None,
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
                    normalize_list(child, attributes);
                }
                item.continuations =
                    normalize_verbatim_blocks(std::mem::take(&mut item.continuations), attributes);
            }
            AstBlock::List(list)
        }
        other => other,
    }
}

fn source_info(
    attribute_range: crate::source::TextRange,
    language_range: Option<crate::source::TextRange>,
    language: Option<String>,
    metadata: &crate::parser::BlockMetadata,
    problems: &mut Vec<crate::parser::BlockProblem>,
) -> crate::parser::SourceInfo {
    let positional = metadata
        .attributes
        .iter()
        .filter(|attribute| attribute.name.is_none())
        .collect::<Vec<_>>();
    let mut line_numbers = false;
    let mut accept_option = |value: &str, range| {
        if value == "linenums" {
            line_numbers = true;
        } else {
            problems.push(crate::parser::BlockProblem {
                kind: crate::parser::BlockProblemKind::InvalidSourceOption,
                range,
            });
        }
    };
    for attribute in positional.into_iter().skip(2) {
        accept_option(&attribute.value, attribute.range);
    }
    for option in &metadata.options {
        accept_option(&option.value, option.range);
    }
    for attribute in metadata
        .attributes
        .iter()
        .filter(|attribute| attribute.name.as_deref() == Some("options"))
    {
        for option in attribute
            .value
            .split(',')
            .map(str::trim)
            .filter(|v| !v.is_empty())
        {
            accept_option(option, attribute.range);
        }
    }

    let mut start_line = None;
    if let Some(attribute) = metadata
        .attributes
        .iter()
        .find(|attribute| attribute.name.as_deref() == Some("start"))
    {
        match attribute.value.parse::<u32>() {
            Ok(value) if value > 0 && line_numbers => start_line = Some(value),
            _ => problems.push(crate::parser::BlockProblem {
                kind: crate::parser::BlockProblemKind::InvalidSourceStart,
                range: attribute.range,
            }),
        }
    }
    if line_numbers && start_line.is_none() {
        start_line = Some(1);
    }

    crate::parser::SourceInfo {
        attribute_range,
        language_range,
        language,
        line_numbers,
        start_line,
    }
}

fn normalize_list(
    list: &mut crate::parser::ListBlock,
    attributes: &crate::attributes::AttributeEnvironment,
) {
    resolve_list_presentation(list);
    for item in &mut list.items {
        for child in &mut item.children {
            normalize_list(child, attributes);
        }
        item.continuations =
            normalize_verbatim_blocks(std::mem::take(&mut item.continuations), attributes);
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

fn document_type(
    attributes: &crate::attributes::AttributeEnvironment,
    header_end: crate::source::TextSize,
) -> DocumentType {
    attributes
        .resolve_at("doctype", header_end)
        .and_then(|resolved| resolved.value.ok().flatten())
        .map_or(DocumentType::Article, |value| match value.trim() {
            "book" => DocumentType::Book,
            "manpage" => DocumentType::Manpage,
            "inline" => DocumentType::Inline,
            _ => DocumentType::Article,
        })
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
    attributes: &crate::attributes::AttributeEnvironment,
) {
    document.visit_inline_sequences_mut(|inlines| resolve_inlines(inlines, attributes));
}

fn resolve_inlines(inlines: &mut [Inline], attributes: &crate::attributes::AttributeEnvironment) {
    for inline in inlines {
        let offset = inline.range().start();
        match inline {
            Inline::Link(link) => {
                match attributes.expand_at(&link.target_source, link.target_range.start()) {
                    Ok(value) => {
                        link.target = value;
                        link.target_expansion_error = None;
                    }
                    Err(error) => {
                        link.target = link.target_source.clone();
                        link.target_expansion_error = Some(error);
                    }
                }
                resolve_inlines(&mut link.label, attributes);
            }
            Inline::Reference(reference) => {
                match attributes.expand_at(&reference.target_source, reference.target_range.start())
                {
                    Ok(value) => {
                        reference.expanded_target = value;
                        reference.target_expansion_error = None;
                        reference.target = if reference.macro_name_range.is_none() {
                            (!reference.expanded_target.is_empty()).then(|| {
                                crate::reference::ReferenceKey::Local {
                                    anchor: reference.expanded_target.clone(),
                                }
                            })
                        } else {
                            crate::reference::ReferenceKey::parse(&reference.expanded_target)
                        };
                    }
                    Err(error) => {
                        reference.expanded_target = reference.target_source.clone();
                        reference.target_expansion_error = Some(error);
                        reference.target = None;
                    }
                }
                resolve_inlines(&mut reference.label, attributes);
            }
            Inline::Macro(node) => {
                match attributes.expand_at(&node.target_source, node.target_range.start()) {
                    Ok(value) => {
                        node.target = value;
                        node.target_expansion_error = None;
                    }
                    Err(error) => {
                        node.target = node.target_source.clone();
                        node.target_expansion_error = Some(error);
                    }
                }
            }
            Inline::Styled { children, .. } => resolve_inlines(children, attributes),
            Inline::AttributeReference {
                name,
                value,
                expansion_error,
                ..
            } => match attributes
                .resolve_at(name, offset)
                .map(|resolved| resolved.value)
            {
                Some(Ok(Some(resolved))) => {
                    *value = Some(resolved.to_owned());
                    *expansion_error = None;
                }
                Some(Ok(None)) | None => {
                    *value = None;
                    *expansion_error =
                        Some(crate::substitution::AttributeExpansionError::Undefined);
                }
                Some(Err(error)) => {
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
