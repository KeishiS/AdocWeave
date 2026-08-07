//! Semantic lowering from parser facts into the output-independent document model.

use std::collections::BTreeSet;

use crate::attributes::DocumentAttributeOccurrence;
use crate::block_model::{AstBlock, AstDocument, DocumentHeader, DocumentType, ExplicitAnchor};
use crate::inline_model::Inline;
use crate::substitution::AttributeExpansionLimits;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum LoweringFailure {
    Limit(crate::catalog::CatalogLimitExceeded),
    Cancelled,
}

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
    checkpoint: &mut crate::cancellation::CancellationCheckpoint<'_>,
) -> Result<AstDocument, LoweringFailure> {
    if checkpoint.is_cancelled() {
        return Err(LoweringFailure::Cancelled);
    }
    let attribute_environment = crate::attributes::AttributeEnvironment::build(
        &facts.attributes,
        facts.external_attributes,
        facts.attribute_expansion_limits,
        checkpoint,
    )
    .map_err(|()| LoweringFailure::Cancelled)?;
    facts.blocks = normalize_verbatim_blocks(facts.blocks, &attribute_environment, checkpoint)?;
    resolve_delimited_presentations(&mut facts.blocks, checkpoint)?;
    attach_anchors(&mut facts.anchors, &facts.blocks, checkpoint)?;
    facts.header.doctype = document_type(&attribute_environment, facts.header.end);
    let mut document = AstDocument::new(
        facts.blocks,
        facts.attributes,
        facts.header_attribute_count,
        facts.anchors,
        facts.header,
    );
    normalize_heading_kinds(&mut document, checkpoint)?;
    resolve_inline_attributes(&mut document, &attribute_environment, checkpoint)?;
    document.resolved = crate::resolved::ResolvedDocument::build(
        &document,
        attribute_environment,
        facts.processing_limits,
        checkpoint,
    )
    .map_err(|error| match error {
        crate::resolved::ResolvedBuildFailure::Limit(error) => LoweringFailure::Limit(error),
        crate::resolved::ResolvedBuildFailure::Cancelled => LoweringFailure::Cancelled,
    })?;
    Ok(document)
}

fn normalize_heading_kinds(
    document: &mut AstDocument,
    checkpoint: &mut crate::cancellation::CancellationCheckpoint<'_>,
) -> Result<(), LoweringFailure> {
    let doctype = document.header.doctype;
    let cancellation = checkpoint.cancellation();
    let mut inner_checkpoint = crate::cancellation::CancellationCheckpoint::new(cancellation);
    let mut cancelled = false;
    crate::walker::walk_blocks_mut_cancellable(
        &mut document.blocks,
        &mut |block: &mut AstBlock| {
            if cancelled {
                return;
            }
            cancelled = normalize_heading_kind(block, doctype, &mut inner_checkpoint).is_err();
        },
        checkpoint,
    )
    .map_err(|()| LoweringFailure::Cancelled)?;
    if cancelled {
        Err(LoweringFailure::Cancelled)
    } else {
        Ok(())
    }
}

fn normalize_heading_kind(
    block: &mut AstBlock,
    doctype: DocumentType,
    checkpoint: &mut crate::cancellation::CancellationCheckpoint<'_>,
) -> Result<(), ()> {
    let AstBlock::Heading(heading) = block else {
        return Ok(());
    };
    let mut discrete = false;
    for value in &heading.metadata.roles {
        if checkpoint.is_cancelled() {
            return Err(());
        }
        if value.value == "discrete" {
            discrete = true;
            break;
        }
    }
    if !discrete {
        for attribute in &heading.metadata.attributes {
            if checkpoint.is_cancelled() {
                return Err(());
            }
            if attribute.name.is_none() && matches!(attribute.value.as_str(), "discrete" | "float")
            {
                discrete = true;
                break;
            }
        }
    }
    if discrete {
        let level = match heading.kind {
            crate::block_model::HeadingKind::DocumentTitle
            | crate::block_model::HeadingKind::Part => 1,
            crate::block_model::HeadingKind::Section { level }
            | crate::block_model::HeadingKind::Discrete { level } => level,
        };
        heading.kind = crate::block_model::HeadingKind::Discrete { level };
        heading.problems.retain(|problem| {
            *problem != crate::block_model::HeadingProblem::MisplacedDocumentTitle
        });
        heading.well_formed = heading.problems.is_empty();
        heading.hierarchy_valid = heading.well_formed;
    } else if doctype == DocumentType::Book
        && heading.kind == crate::block_model::HeadingKind::DocumentTitle
        && heading
            .problems
            .contains(&crate::block_model::HeadingProblem::MisplacedDocumentTitle)
    {
        heading.kind = crate::block_model::HeadingKind::Part;
        heading.problems.retain(|problem| {
            *problem != crate::block_model::HeadingProblem::MisplacedDocumentTitle
        });
        heading.well_formed = heading.problems.is_empty();
        heading.hierarchy_valid = heading.well_formed;
    }
    Ok(())
}

fn resolve_delimited_presentations(
    blocks: &mut [AstBlock],
    checkpoint: &mut crate::cancellation::CancellationCheckpoint<'_>,
) -> Result<(), LoweringFailure> {
    let cancellation = checkpoint.cancellation();
    let mut inner_checkpoint = crate::cancellation::CancellationCheckpoint::new(cancellation);
    let mut cancelled = false;
    crate::walker::walk_blocks_mut_cancellable(
        blocks,
        &mut |block: &mut AstBlock| {
            if cancelled {
                return;
            }
            if let AstBlock::Delimited(block) = block {
                cancelled = resolve_delimited_presentation(block, &mut inner_checkpoint).is_err();
            }
        },
        checkpoint,
    )
    .map_err(|()| LoweringFailure::Cancelled)?;
    if cancelled {
        Err(LoweringFailure::Cancelled)
    } else {
        Ok(())
    }
}

fn resolve_delimited_presentation(
    block: &mut crate::block_model::DelimitedBlock,
    checkpoint: &mut crate::cancellation::CancellationCheckpoint<'_>,
) -> Result<(), ()> {
    let mut positional = Vec::new();
    for attribute in &block.metadata.attributes {
        if checkpoint.is_cancelled() {
            return Err(());
        }
        if attribute.name.is_none() {
            positional.push(attribute);
        }
    }
    let style = positional.first().map(|attribute| attribute.value.as_str());
    block.presentation = match (block.kind, style) {
        (crate::block_model::DelimitedBlockKind::Example, Some(style))
        | (crate::block_model::DelimitedBlockKind::Open, Some(style))
            if crate::block_model::AdmonitionKind::parse(style).is_some() =>
        {
            let attribute = positional[0];
            Some(crate::block_model::DelimitedPresentation::Admonition(
                crate::block_model::AdmonitionPresentation {
                    kind: crate::block_model::AdmonitionKind::parse(&attribute.value)
                        .expect("guarded admonition style"),
                    label_range: attribute.range,
                },
            ))
        }
        (crate::block_model::DelimitedBlockKind::Quote, Some("quote")) => {
            Some(crate::block_model::DelimitedPresentation::Quote(
                crate::block_model::QuotePresentation {
                    kind: crate::block_model::QuoteKind::Quote,
                    attribution: positional.get(1).map(|attribute| {
                        crate::block_model::MetadataValue {
                            value: attribute.value.clone(),
                            range: attribute.range,
                        }
                    }),
                    citation: positional.get(2).map(|attribute| {
                        crate::block_model::MetadataValue {
                            value: attribute.value.clone(),
                            range: attribute.range,
                        }
                    }),
                },
            ))
        }
        (crate::block_model::DelimitedBlockKind::Quote, Some("verse")) => {
            Some(crate::block_model::DelimitedPresentation::Quote(
                crate::block_model::QuotePresentation {
                    kind: crate::block_model::QuoteKind::Verse,
                    attribution: positional.get(1).map(|attribute| {
                        crate::block_model::MetadataValue {
                            value: attribute.value.clone(),
                            range: attribute.range,
                        }
                    }),
                    citation: positional.get(2).map(|attribute| {
                        crate::block_model::MetadataValue {
                            value: attribute.value.clone(),
                            range: attribute.range,
                        }
                    }),
                },
            ))
        }
        _ => None,
    };
    Ok(())
}

fn normalize_verbatim_blocks(
    blocks: Vec<AstBlock>,
    attributes: &crate::attributes::AttributeEnvironment,
    checkpoint: &mut crate::cancellation::CancellationCheckpoint<'_>,
) -> Result<Vec<AstBlock>, LoweringFailure> {
    let mut normalized = Vec::with_capacity(blocks.len());
    for block in blocks {
        if checkpoint.is_cancelled() {
            return Err(LoweringFailure::Cancelled);
        }
        normalized.push(normalize_verbatim_block(block, attributes, checkpoint)?);
    }
    Ok(normalized)
}

fn normalize_verbatim_block(
    block: AstBlock,
    attributes: &crate::attributes::AttributeEnvironment,
    checkpoint: &mut crate::cancellation::CancellationCheckpoint<'_>,
) -> Result<AstBlock, LoweringFailure> {
    let block = match block {
        AstBlock::Source(mut source) => {
            let info = source_info(
                source.attribute_range,
                source.language_range,
                source.language,
                &source.metadata,
                &mut source.problems,
                checkpoint,
            )?;
            AstBlock::Verbatim(crate::block_model::VerbatimBlock {
                metadata: source.metadata,
                kind: crate::block_model::VerbatimKind::Source(info),
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
                crate::block_model::DelimitedContent::Compound(children) => {
                    *children = normalize_verbatim_blocks(
                        std::mem::take(children),
                        attributes,
                        checkpoint,
                    )?;
                }
                crate::block_model::DelimitedContent::Table(table) => {
                    for row in &mut table.rows {
                        if checkpoint.is_cancelled() {
                            return Err(LoweringFailure::Cancelled);
                        }
                        for cell in &mut row.cells {
                            if checkpoint.is_cancelled() {
                                return Err(LoweringFailure::Cancelled);
                            }
                            if let crate::table::TableCellContent::AsciiDoc(children) =
                                &mut cell.content
                            {
                                *children = normalize_verbatim_blocks(
                                    std::mem::take(children),
                                    attributes,
                                    checkpoint,
                                )?;
                            }
                        }
                    }
                }
                crate::block_model::DelimitedContent::Verbatim(_)
                | crate::block_model::DelimitedContent::Passthrough(_) => {}
            }
            let implicit_listing = block.kind == crate::block_model::DelimitedBlockKind::Listing
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
                && let crate::block_model::DelimitedContent::Verbatim(value) = block.content
            {
                let attribute_range = block
                    .metadata
                    .range
                    .unwrap_or(block.opening_delimiter_range);
                return Ok(AstBlock::Verbatim(crate::block_model::VerbatimBlock {
                    metadata: block.metadata,
                    kind: crate::block_model::VerbatimKind::Source(
                        crate::block_model::SourceInfo {
                            attribute_range,
                            language_range: None,
                            language: Some(language.to_owned()),
                            line_numbers: false,
                            start_line: None,
                        },
                    ),
                    range: block.range,
                    delimiter_range: block.opening_delimiter_range,
                    content_range: block.content_range,
                    value,
                    callouts: Vec::new(),
                    problems: block.problems,
                }));
            }
            let kind = match block.kind {
                crate::block_model::DelimitedBlockKind::Listing => {
                    Some(crate::block_model::VerbatimKind::Listing)
                }
                crate::block_model::DelimitedBlockKind::Literal => {
                    Some(crate::block_model::VerbatimKind::Literal)
                }
                _ => None,
            };
            if let Some(kind) = kind
                && let crate::block_model::DelimitedContent::Verbatim(value) = block.content
            {
                return Ok(AstBlock::Verbatim(crate::block_model::VerbatimBlock {
                    metadata: block.metadata,
                    kind,
                    range: block.range,
                    delimiter_range: block.opening_delimiter_range,
                    content_range: block.content_range,
                    value,
                    callouts: Vec::new(),
                    problems: block.problems,
                }));
            }
            AstBlock::Delimited(block)
        }
        AstBlock::List(mut list) => {
            resolve_list_presentation(&mut list, checkpoint)?;
            for item in &mut list.items {
                if checkpoint.is_cancelled() {
                    return Err(LoweringFailure::Cancelled);
                }
                for child in &mut item.children {
                    normalize_list(child, attributes, checkpoint)?;
                }
                item.continuations = normalize_verbatim_blocks(
                    std::mem::take(&mut item.continuations),
                    attributes,
                    checkpoint,
                )?;
            }
            AstBlock::List(list)
        }
        other => other,
    };
    Ok(block)
}

fn source_info(
    attribute_range: crate::source::TextRange,
    language_range: Option<crate::source::TextRange>,
    language: Option<String>,
    metadata: &crate::block_model::BlockMetadata,
    problems: &mut Vec<crate::block_model::BlockProblem>,
    checkpoint: &mut crate::cancellation::CancellationCheckpoint<'_>,
) -> Result<crate::block_model::SourceInfo, LoweringFailure> {
    let mut positional = Vec::new();
    for attribute in &metadata.attributes {
        if checkpoint.is_cancelled() {
            return Err(LoweringFailure::Cancelled);
        }
        if attribute.name.is_none() {
            positional.push(attribute);
        }
    }
    let mut line_numbers = false;
    let mut accept_option = |value: &str, range| {
        if value == "linenums" {
            line_numbers = true;
        } else {
            problems.push(crate::block_model::BlockProblem {
                kind: crate::block_model::BlockProblemKind::InvalidSourceOption,
                range,
            });
        }
    };
    for attribute in positional.into_iter().skip(2) {
        if checkpoint.is_cancelled() {
            return Err(LoweringFailure::Cancelled);
        }
        accept_option(&attribute.value, attribute.range);
    }
    for option in &metadata.options {
        if checkpoint.is_cancelled() {
            return Err(LoweringFailure::Cancelled);
        }
        accept_option(&option.value, option.range);
    }
    for attribute in metadata
        .attributes
        .iter()
        .filter(|attribute| attribute.name.as_deref() == Some("options"))
    {
        if checkpoint.is_cancelled() {
            return Err(LoweringFailure::Cancelled);
        }
        for option in attribute
            .value
            .split(',')
            .map(str::trim)
            .filter(|v| !v.is_empty())
        {
            if checkpoint.is_cancelled() {
                return Err(LoweringFailure::Cancelled);
            }
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
            _ => problems.push(crate::block_model::BlockProblem {
                kind: crate::block_model::BlockProblemKind::InvalidSourceStart,
                range: attribute.range,
            }),
        }
    }
    if line_numbers && start_line.is_none() {
        start_line = Some(1);
    }

    Ok(crate::block_model::SourceInfo {
        attribute_range,
        language_range,
        language,
        line_numbers,
        start_line,
    })
}

fn normalize_list(
    list: &mut crate::block_model::ListBlock,
    attributes: &crate::attributes::AttributeEnvironment,
    checkpoint: &mut crate::cancellation::CancellationCheckpoint<'_>,
) -> Result<(), LoweringFailure> {
    resolve_list_presentation(list, checkpoint)?;
    for item in &mut list.items {
        if checkpoint.is_cancelled() {
            return Err(LoweringFailure::Cancelled);
        }
        for child in &mut item.children {
            normalize_list(child, attributes, checkpoint)?;
        }
        item.continuations = normalize_verbatim_blocks(
            std::mem::take(&mut item.continuations),
            attributes,
            checkpoint,
        )?;
    }
    Ok(())
}

fn resolve_list_presentation(
    list: &mut crate::block_model::ListBlock,
    checkpoint: &mut crate::cancellation::CancellationCheckpoint<'_>,
) -> Result<(), LoweringFailure> {
    if list.kind != crate::block_model::ListKind::Ordered {
        return Ok(());
    }

    let mut presentation = crate::block_model::OrderedListPresentation::default();
    let mut problems = Vec::new();
    for attribute in &list.metadata.attributes {
        if checkpoint.is_cancelled() {
            return Err(LoweringFailure::Cancelled);
        }
        match attribute.name.as_deref() {
            Some("start") => {
                let start = attribute
                    .value
                    .trim()
                    .parse::<u32>()
                    .ok()
                    .filter(|value| *value > 0);
                if start.is_none() {
                    problems.push(crate::block_model::ListPresentationProblem {
                        kind: crate::block_model::ListPresentationProblemKind::InvalidStart,
                        range: attribute.range,
                    });
                }
                presentation.start = start;
            }
            Some("style") => {
                if let Some(style) = ordered_list_style(&attribute.value) {
                    presentation.style = style;
                } else {
                    problems.push(crate::block_model::ListPresentationProblem {
                        kind: crate::block_model::ListPresentationProblemKind::UnknownOrderedStyle,
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
        if checkpoint.is_cancelled() {
            return Err(LoweringFailure::Cancelled);
        }
        if item.invalid_explicit_number {
            problems.push(crate::block_model::ListPresentationProblem {
                kind: crate::block_model::ListPresentationProblemKind::InvalidExplicitNumber,
                range: item.marker_range,
            });
        }
        if let Some(number) = item.explicit_number
            && number != expected
        {
            problems.push(crate::block_model::ListPresentationProblem {
                kind: crate::block_model::ListPresentationProblemKind::InconsistentExplicitNumber,
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
    Ok(())
}

fn ordered_list_style(value: &str) -> Option<crate::block_model::OrderedListStyle> {
    use crate::block_model::OrderedListStyle;

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

fn attach_anchors(
    anchors: &mut [ExplicitAnchor],
    blocks: &[AstBlock],
    checkpoint: &mut crate::cancellation::CancellationCheckpoint<'_>,
) -> Result<(), LoweringFailure> {
    let mut ranges = Vec::new();
    let walked = crate::walker::try_walk_block_slice(blocks, |node| {
        if checkpoint.is_cancelled() {
            return std::ops::ControlFlow::Break(());
        }
        if let crate::walker::SemanticNode::Block(block) = node {
            ranges.push(block.range());
        }
        std::ops::ControlFlow::Continue(())
    });
    if walked.is_break() {
        return Err(LoweringFailure::Cancelled);
    }
    crate::cancellation::sort_by_cancellable(
        &mut ranges,
        &mut |left, right| (left.start(), left.end()).cmp(&(right.start(), right.end())),
        checkpoint,
    )
    .map_err(|()| LoweringFailure::Cancelled)?;
    for anchor in &mut *anchors {
        if checkpoint.is_cancelled() {
            return Err(LoweringFailure::Cancelled);
        }
        anchor.target_range = None;
        for range in &ranges {
            if checkpoint.is_cancelled() {
                return Err(LoweringFailure::Cancelled);
            }
            if range.start() >= anchor.range.end() {
                anchor.target_range = Some(*range);
                break;
            }
        }
    }
    let mut anchored_targets = BTreeSet::new();
    for anchor in anchors {
        if checkpoint.is_cancelled() {
            return Err(LoweringFailure::Cancelled);
        }
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
    Ok(())
}

fn resolve_inline_attributes(
    document: &mut AstDocument,
    attributes: &crate::attributes::AttributeEnvironment,
    checkpoint: &mut crate::cancellation::CancellationCheckpoint<'_>,
) -> Result<(), LoweringFailure> {
    crate::walker::walk_inline_sequences_mut_cancellable(
        &mut document.blocks,
        &mut |inlines, checkpoint| resolve_inlines(inlines, attributes, checkpoint),
        checkpoint,
    )
    .map_err(|()| LoweringFailure::Cancelled)
}

fn resolve_inlines(
    inlines: &mut [Inline],
    attributes: &crate::attributes::AttributeEnvironment,
    checkpoint: &mut crate::cancellation::CancellationCheckpoint<'_>,
) -> Result<(), ()> {
    for inline in inlines {
        if checkpoint.is_cancelled() {
            return Err(());
        }
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
                resolve_inlines(&mut link.label, attributes, checkpoint)?;
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
                resolve_inlines(&mut reference.label, attributes, checkpoint)?;
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
            Inline::Styled { children, .. } => {
                resolve_inlines(children, attributes, checkpoint)?;
            }
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
    Ok(())
}

#[cfg(test)]
mod responsibility_tests {
    #[test]
    fn parser_dispatch_does_not_own_ast_utilities_or_post_recognition_normalization() {
        let parser = include_str!("parser.rs");
        let ast_util = include_str!("ast_util.rs");
        let lowering = include_str!("lowering.rs");

        assert!(!parser.contains("impl AstDocument"));
        assert!(!parser.contains("impl AstBlock"));
        assert!(!parser.contains("fn normalize_heading_kinds"));
        assert!(ast_util.contains("impl AstDocument"));
        assert!(ast_util.contains("impl AstBlock"));
        assert!(lowering.contains("fn normalize_heading_kinds"));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::block_model::{HeadingKind, HeadingProblem, OrderedListStyle};

    fn checkpoint() -> crate::cancellation::CancellationCheckpoint<'static> {
        crate::cancellation::CancellationCheckpoint::new(&crate::core::NeverCancel)
    }

    fn range() -> crate::source::TextRange {
        crate::source::TextRange::new(crate::source::TextSize::ZERO, crate::source::TextSize::ZERO)
            .expect("zero range")
    }

    fn heading(kind: HeadingKind, problems: Vec<HeadingProblem>) -> AstBlock {
        AstBlock::Heading(crate::block_model::Heading {
            metadata: crate::block_model::BlockMetadata::default(),
            range: range(),
            marker_range: range(),
            separator_range: range(),
            text_range: range(),
            kind,
            well_formed: problems.is_empty(),
            hierarchy_valid: problems.is_empty(),
            text: String::new(),
            inlines: Vec::new(),
            inline_problems: Vec::new(),
            problems,
        })
    }

    fn kind_of(block: &AstBlock) -> HeadingKind {
        let AstBlock::Heading(heading) = block else {
            panic!("expected a heading");
        };
        heading.kind
    }

    fn role(block: &mut AstBlock, value: &str) {
        let AstBlock::Heading(heading) = block else {
            panic!("expected a heading");
        };
        heading
            .metadata
            .roles
            .push(crate::block_model::MetadataValue {
                value: value.to_owned(),
                range: range(),
            });
    }

    fn positional_attribute(block: &mut AstBlock, value: &str) {
        let AstBlock::Heading(heading) = block else {
            panic!("expected a heading");
        };
        heading
            .metadata
            .attributes
            .push(crate::block_model::ElementAttribute {
                name: None,
                value: value.to_owned(),
                range: range(),
            });
    }

    /// A heading marked discrete keeps its level and stops being a title.
    ///
    /// The mark reaches the same decision from a role and from a positional
    /// attribute, and `float` is the older spelling of `discrete`. All three
    /// spellings are checked together because they must not drift apart.
    #[test]
    fn a_discrete_mark_demotes_a_heading_and_keeps_its_level() {
        for spelling in ["discrete", "float"] {
            let mut block = heading(
                HeadingKind::Section { level: 3 },
                vec![HeadingProblem::MisplacedDocumentTitle],
            );
            positional_attribute(&mut block, spelling);
            normalize_heading_kind(&mut block, DocumentType::Article, &mut checkpoint())
                .expect("not cancelled");
            assert_eq!(
                kind_of(&block),
                HeadingKind::Discrete { level: 3 },
                "positional {spelling}"
            );
        }

        let mut block = heading(HeadingKind::Section { level: 2 }, Vec::new());
        role(&mut block, "discrete");
        normalize_heading_kind(&mut block, DocumentType::Article, &mut checkpoint())
            .expect("not cancelled");
        assert_eq!(kind_of(&block), HeadingKind::Discrete { level: 2 });

        // A named attribute is not the discrete mark, only a positional one is.
        let mut block = heading(HeadingKind::Section { level: 2 }, Vec::new());
        let AstBlock::Heading(inner) = &mut block else {
            panic!("expected a heading");
        };
        inner
            .metadata
            .attributes
            .push(crate::block_model::ElementAttribute {
                name: Some("role".to_owned()),
                value: "discrete".to_owned(),
                range: range(),
            });
        normalize_heading_kind(&mut block, DocumentType::Article, &mut checkpoint())
            .expect("not cancelled");
        assert_eq!(kind_of(&block), HeadingKind::Section { level: 2 });
    }

    /// A discrete document title becomes level 1, not level 0.
    #[test]
    fn a_discrete_document_title_and_part_both_become_level_one() {
        for kind in [HeadingKind::DocumentTitle, HeadingKind::Part] {
            let mut block = heading(kind, Vec::new());
            role(&mut block, "discrete");
            normalize_heading_kind(&mut block, DocumentType::Article, &mut checkpoint())
                .expect("not cancelled");
            assert_eq!(
                kind_of(&block),
                HeadingKind::Discrete { level: 1 },
                "{kind:?}"
            );
        }
    }

    /// Marking a heading discrete clears the misplaced-title complaint.
    ///
    /// The complaint says a document title appeared where one may not. Once the
    /// heading is not a title, the complaint no longer describes anything, so it
    /// is dropped and the heading counts as well formed again.
    #[test]
    fn a_discrete_mark_clears_the_misplaced_title_problem() {
        let mut block = heading(
            HeadingKind::DocumentTitle,
            vec![HeadingProblem::MisplacedDocumentTitle],
        );
        role(&mut block, "discrete");
        normalize_heading_kind(&mut block, DocumentType::Article, &mut checkpoint())
            .expect("not cancelled");
        let AstBlock::Heading(inner) = &block else {
            panic!("expected a heading");
        };
        assert!(inner.problems.is_empty());
        assert!(inner.well_formed);
        assert!(inner.hierarchy_valid);
    }

    /// In a book, a second document title becomes a part instead of an error.
    ///
    /// Only a book has parts, and only a misplaced title is promoted. An article
    /// keeps the complaint, and a book title that was not misplaced stays a
    /// title.
    #[test]
    fn only_a_book_promotes_a_misplaced_document_title_to_a_part() {
        let mut block = heading(
            HeadingKind::DocumentTitle,
            vec![HeadingProblem::MisplacedDocumentTitle],
        );
        normalize_heading_kind(&mut block, DocumentType::Book, &mut checkpoint())
            .expect("not cancelled");
        assert_eq!(kind_of(&block), HeadingKind::Part);
        let AstBlock::Heading(inner) = &block else {
            panic!("expected a heading");
        };
        assert!(inner.problems.is_empty());
        assert!(inner.well_formed);

        for doctype in [
            DocumentType::Article,
            DocumentType::Manpage,
            DocumentType::Inline,
        ] {
            let mut block = heading(
                HeadingKind::DocumentTitle,
                vec![HeadingProblem::MisplacedDocumentTitle],
            );
            normalize_heading_kind(&mut block, doctype, &mut checkpoint()).expect("not cancelled");
            assert_eq!(kind_of(&block), HeadingKind::DocumentTitle, "{doctype:?}");
        }

        let mut block = heading(HeadingKind::DocumentTitle, Vec::new());
        normalize_heading_kind(&mut block, DocumentType::Book, &mut checkpoint())
            .expect("not cancelled");
        assert_eq!(kind_of(&block), HeadingKind::DocumentTitle);
    }

    /// Blocks that are not headings pass through untouched.
    #[test]
    fn normalization_ignores_blocks_that_are_not_headings() {
        let mut block = AstBlock::Paragraph(crate::block_model::Paragraph {
            metadata: crate::block_model::BlockMetadata::default(),
            range: range(),
            content_range: range(),
            value: "text".to_owned(),
            inlines: Vec::new(),
            admonition: None,
            inline_problems: Vec::new(),
        });
        let before = block.clone();
        normalize_heading_kind(&mut block, DocumentType::Book, &mut checkpoint())
            .expect("not cancelled");
        assert_eq!(block, before);
    }

    /// The ordered list styles AsciiDoc names, and nothing else.
    ///
    /// An unknown name is rejected rather than falling back to a default, so a
    /// typo keeps its own diagnostic instead of silently numbering differently.
    #[test]
    fn ordered_list_styles_are_named_exactly_and_surrounding_space_is_ignored() {
        for (value, expected) in [
            ("arabic", OrderedListStyle::Arabic),
            ("decimal", OrderedListStyle::Decimal),
            ("loweralpha", OrderedListStyle::LowerAlpha),
            ("upperalpha", OrderedListStyle::UpperAlpha),
            ("lowerroman", OrderedListStyle::LowerRoman),
            ("upperroman", OrderedListStyle::UpperRoman),
            ("lowergreek", OrderedListStyle::LowerGreek),
        ] {
            assert_eq!(ordered_list_style(value), Some(expected), "{value}");
            assert_eq!(ordered_list_style(&format!("  {value}\t")), Some(expected));
        }

        for value in ["", "Arabic", "ARABIC", "roman", "arabic2", "arabic decimal"] {
            assert_eq!(ordered_list_style(value), None, "{value}");
        }
    }
}
