//! Output-independent document indexes and editor-facing symbols.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;

use crate::block_model::{
    AstBlock, AstDocument, ElementAttribute, Heading, HeadingKind, MetadataValue, SourceInfo,
};
use crate::source::TextRange;

/// The immutable public semantic document model.
///
/// Parser implementation details remain private. Backends and hosts use this
/// type as the sole root for blocks, header metadata, anchors, and source facts.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Document {
    inner: AstDocument,
}

impl Document {
    pub(crate) const fn from_ast(inner: AstDocument) -> Self {
        Self { inner }
    }

    pub fn blocks(&self) -> &[crate::block_model::Block] {
        self.inner.blocks()
    }

    pub fn anchors(&self) -> &[crate::block_model::ExplicitAnchor] {
        self.inner.anchors()
    }

    /// Returns document-attribute occurrences in source order.
    ///
    /// This preserves duplicate definitions, set/unset operations, empty
    /// values, and source ranges. Use [`Self::presentation`] when only the
    /// final resolved values are needed.
    pub fn attribute_occurrences(&self) -> &[crate::attributes::DocumentAttributeOccurrence] {
        self.inner.attributes()
    }

    /// Returns the leading document-header attribute occurrences.
    pub fn header_attribute_occurrences(
        &self,
    ) -> &[crate::attributes::DocumentAttributeOccurrence] {
        self.inner.header_attributes()
    }

    pub const fn header(&self) -> &crate::block_model::DocumentHeader {
        self.inner.header()
    }

    pub fn preamble(&self) -> &[crate::block_model::Block] {
        self.inner.preamble()
    }

    pub fn node_count(&self) -> usize {
        self.inner.node_count()
    }

    pub fn snapshot(&self) -> String {
        self.inner.snapshot()
    }

    pub fn heading_ids(&self) -> Vec<HeadingId> {
        generate_heading_ids_ast(&self.inner)
    }

    pub fn element_at(&self, offset: u32) -> Option<DocumentElement<'_>> {
        document_element_at_ast(&self.inner, offset)
    }

    pub fn symbols(&self) -> Vec<DocumentSymbol> {
        document_symbols_ast(&self.inner)
    }

    pub const fn catalogs(&self) -> &crate::catalog::DocumentCatalogs {
        self.inner.catalogs()
    }

    pub const fn presentation(&self) -> &crate::presentation::DocumentPresentation {
        self.inner.presentation()
    }

    pub fn reference_targets(&self) -> &[ReferenceTarget] {
        self.inner.identifiers().targets()
    }

    pub(crate) const fn inner(&self) -> &AstDocument {
        &self.inner
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HeadingId {
    pub range: TextRange,
    pub id_range: TextRange,
    pub base: String,
    pub id: String,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct DocumentIdentifiers {
    heading_ids: Vec<HeadingId>,
    heading_ordinals: BTreeMap<TextRange, usize>,
    targets: Vec<ReferenceTarget>,
    target_ordinals_by_range: BTreeMap<TextRange, usize>,
    target_ordinals_by_id: BTreeMap<String, usize>,
}

impl DocumentIdentifiers {
    pub fn heading_ids(&self) -> &[HeadingId] {
        &self.heading_ids
    }

    pub fn targets(&self) -> &[ReferenceTarget] {
        &self.targets
    }

    pub fn target_at(&self, range: TextRange) -> Option<&ReferenceTarget> {
        self.target_ordinals_by_range
            .get(&range)
            .and_then(|ordinal| self.targets.get(*ordinal))
    }

    pub fn target_by_id(&self, id: &str) -> Option<&ReferenceTarget> {
        self.target_ordinals_by_id
            .get(id)
            .and_then(|ordinal| self.targets.get(*ordinal))
    }

    pub fn heading_at(&self, range: TextRange) -> Option<&HeadingId> {
        self.heading_ordinals
            .get(&range)
            .and_then(|ordinal| self.heading_ids.get(*ordinal))
    }
}

pub fn generate_heading_ids(document: &Document) -> Vec<HeadingId> {
    document.heading_ids()
}

pub(crate) fn generate_heading_ids_ast(document: &AstDocument) -> Vec<HeadingId> {
    document.identifiers().heading_ids.clone()
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReferenceTargetKind {
    DocumentTitle,
    Part,
    Section,
    ExplicitAnchor,
    InlineAnchor,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReferenceTarget {
    pub kind: ReferenceTargetKind,
    pub id: String,
    pub label: String,
    pub id_range: TextRange,
    pub target_range: TextRange,
}

pub fn reference_targets(document: &Document) -> Vec<ReferenceTarget> {
    reference_targets_ast(document.inner())
}

/// Returns whether `id` can be authored as an explicit anchor identifier.
///
/// Hosts that create or edit anchors must use the same acceptance rule as the
/// parser so an edit cannot turn a valid target into unsupported syntax.
pub fn is_valid_anchor_id(id: &str) -> bool {
    !id.is_empty()
        && id.chars().all(|character| {
            !character.is_control()
                && !character.is_whitespace()
                && !matches!(
                    character,
                    '[' | ']' | '<' | '>' | ',' | '#' | '"' | '\'' | '&' | '=' | '(' | ')'
                )
        })
}

pub(crate) fn reference_targets_ast(document: &AstDocument) -> Vec<ReferenceTarget> {
    document.identifiers().targets.clone()
}

pub(crate) fn build_identifiers(
    document: &AstDocument,
    checkpoint: &mut crate::cancellation::CancellationCheckpoint<'_>,
) -> Result<DocumentIdentifiers, ()> {
    let mut inline_anchors = Vec::new();
    let walked = crate::walker::try_walk_ast(document, |node| {
        if checkpoint.is_cancelled() {
            return std::ops::ControlFlow::Break(());
        }
        let crate::walker::SemanticNode::Inline(crate::inline_model::Inline::Macro(anchor)) = node
        else {
            return std::ops::ControlFlow::Continue(());
        };
        if matches!(
            anchor.kind,
            crate::inline_model::StandardMacroKind::Anchor
                | crate::inline_model::StandardMacroKind::BibliographyAnchor
        ) && !anchor.target.is_empty()
        {
            inline_anchors.push(anchor);
        }
        std::ops::ControlFlow::Continue(())
    });
    if walked.is_break() {
        return Err(());
    }
    let mut used = BTreeSet::new();
    for anchor in document.anchors().iter().filter(|anchor| anchor.valid) {
        if checkpoint.is_cancelled() {
            return Err(());
        }
        used.insert(anchor.id.clone());
    }
    for anchor in &inline_anchors {
        if checkpoint.is_cancelled() {
            return Err(());
        }
        used.insert(anchor.target.clone());
    }
    let mut occurrences = BTreeMap::<String, usize>::new();
    let mut heading_ids = Vec::new();
    let mut targets = Vec::new();
    let walked = crate::walker::try_walk_block_slice(document.blocks(), |node| {
        if checkpoint.is_cancelled() {
            return std::ops::ControlFlow::Break(());
        }
        let crate::walker::SemanticNode::Block(block) = node else {
            return std::ops::ControlFlow::Continue(());
        };
        let range = block.range();
        let mut attached = Vec::new();
        for anchor in document.anchors() {
            if checkpoint.is_cancelled() {
                return std::ops::ControlFlow::Break(());
            }
            if anchor.valid && anchor.target_range == Some(range) {
                attached.push(anchor);
            }
        }
        for anchor in &attached {
            if checkpoint.is_cancelled() {
                return std::ops::ControlFlow::Break(());
            }
            targets.push(ReferenceTarget {
                kind: match block {
                    AstBlock::Heading(heading) => match heading.kind {
                        HeadingKind::DocumentTitle => ReferenceTargetKind::DocumentTitle,
                        HeadingKind::Part => ReferenceTargetKind::Part,
                        HeadingKind::Section { .. } | HeadingKind::Discrete { .. } => {
                            ReferenceTargetKind::Section
                        }
                    },
                    _ => ReferenceTargetKind::ExplicitAnchor,
                },
                id: anchor.id.clone(),
                label: anchor.label.clone().unwrap_or_else(|| block_label(block)),
                id_range: anchor.id_range,
                target_range: range,
            });
        }
        if let AstBlock::Heading(heading) = block {
            let Ok(base) = heading_id_base_cancellable(&heading.text, checkpoint) else {
                return std::ops::ControlFlow::Break(());
            };
            let (id, id_range) = if let Some(anchor) = attached.first() {
                (anchor.id.clone(), anchor.id_range)
            } else {
                let Ok(id) = unique_heading_id(&base, &mut occurrences, &mut used, checkpoint)
                else {
                    return std::ops::ControlFlow::Break(());
                };
                (id, heading.text_range)
            };
            heading_ids.push(HeadingId {
                range: heading.text_range,
                id_range,
                base,
                id: id.clone(),
            });
            if attached.is_empty() {
                targets.push(ReferenceTarget {
                    kind: match heading.kind {
                        HeadingKind::DocumentTitle => ReferenceTargetKind::DocumentTitle,
                        HeadingKind::Part => ReferenceTargetKind::Part,
                        HeadingKind::Section { .. } | HeadingKind::Discrete { .. } => {
                            ReferenceTargetKind::Section
                        }
                    },
                    id,
                    label: heading.text.clone(),
                    id_range,
                    target_range: heading.range,
                });
            }
        }
        std::ops::ControlFlow::Continue(())
    });
    if walked.is_break() {
        return Err(());
    }
    for anchor in inline_anchors {
        if checkpoint.is_cancelled() {
            return Err(());
        }
        targets.push(ReferenceTarget {
            kind: ReferenceTargetKind::InlineAnchor,
            id: anchor.target.clone(),
            label: anchor.attributes.first().map_or_else(
                || anchor.target.clone(),
                |attribute| attribute.value.clone(),
            ),
            id_range: anchor.target_range,
            target_range: anchor.range,
        });
    }
    crate::cancellation::sort_by_cancellable(
        &mut targets,
        &mut |left, right| {
            (left.target_range.start(), left.target_range.end())
                .cmp(&(right.target_range.start(), right.target_range.end()))
        },
        checkpoint,
    )?;
    crate::cancellation::sort_by_cancellable(
        &mut heading_ids,
        &mut |left, right| left.range.cmp(&right.range),
        checkpoint,
    )?;
    let mut heading_ordinals = BTreeMap::new();
    for (ordinal, heading) in heading_ids.iter().enumerate() {
        if checkpoint.is_cancelled() {
            return Err(());
        }
        heading_ordinals.insert(heading.range, ordinal);
    }
    let mut target_ordinals_by_range = BTreeMap::new();
    let mut target_ordinals_by_id = BTreeMap::new();
    for (ordinal, target) in targets.iter().enumerate() {
        if checkpoint.is_cancelled() {
            return Err(());
        }
        target_ordinals_by_range
            .entry(target.target_range)
            .or_insert(ordinal);
        target_ordinals_by_id
            .entry(target.id.clone())
            .or_insert(ordinal);
    }
    Ok(DocumentIdentifiers {
        heading_ids,
        heading_ordinals,
        targets,
        target_ordinals_by_range,
        target_ordinals_by_id,
    })
}

fn unique_heading_id(
    base: &str,
    occurrences: &mut BTreeMap<String, usize>,
    used: &mut BTreeSet<String>,
    checkpoint: &mut crate::cancellation::CancellationCheckpoint<'_>,
) -> Result<String, ()> {
    let occurrence = occurrences.entry(base.to_owned()).or_default();
    loop {
        if checkpoint.is_cancelled() {
            return Err(());
        }
        *occurrence += 1;
        let candidate = if *occurrence == 1 {
            base.to_owned()
        } else {
            format!("{base}_{}", *occurrence)
        };
        if used.insert(candidate.clone()) {
            return Ok(candidate);
        }
    }
}

fn block_label(block: &AstBlock) -> String {
    match block {
        AstBlock::Heading(value) => value.text.clone(),
        AstBlock::Paragraph(value) => value.value.lines().next().unwrap_or_default().to_owned(),
        AstBlock::LiteralParagraph(_) => "literal paragraph".to_owned(),
        AstBlock::Break(_) => "break".to_owned(),
        AstBlock::Source(value) => value.language.as_ref().map_or_else(
            || "source block".to_owned(),
            |name| format!("{name} source block"),
        ),
        AstBlock::Verbatim(value) => match &value.kind {
            crate::block_model::VerbatimKind::Source(source) => {
                source.language.as_ref().map_or_else(
                    || "source block".to_owned(),
                    |name| format!("{name} source block"),
                )
            }
            crate::block_model::VerbatimKind::Listing => "listing block".to_owned(),
            crate::block_model::VerbatimKind::Literal => "literal block".to_owned(),
        },
        AstBlock::List(value) => value
            .items
            .first()
            .map_or_else(|| "list".to_owned(), |item| item.text.clone()),
        AstBlock::Math(_) => "math block".to_owned(),
        AstBlock::Delimited(value) => format!("{:?} block", value.kind).to_ascii_lowercase(),
        AstBlock::Unsupported(_) => "unsupported block".to_owned(),
    }
}

pub fn heading_id_base(text: &str) -> String {
    heading_id_base_cancellable(
        text,
        &mut crate::cancellation::CancellationCheckpoint::new(&crate::core::NeverCancel),
    )
    .expect("NeverCancel cannot cancel heading ID generation")
}

fn heading_id_base_cancellable(
    text: &str,
    checkpoint: &mut crate::cancellation::CancellationCheckpoint<'_>,
) -> Result<String, ()> {
    let mut id = String::from("_");
    let mut pending_separator = false;
    for character in text.chars() {
        if checkpoint.is_cancelled() {
            return Err(());
        }
        if character.is_alphanumeric() {
            if pending_separator && id.len() > 1 {
                id.push('_');
            }
            for lower in character.to_lowercase() {
                id.push(lower);
            }
            pending_separator = false;
        } else {
            pending_separator = true;
        }
    }
    if id.len() == 1 {
        id.push_str("section");
    }
    Ok(id)
}

pub fn source_language_candidates(prefix: &str) -> Vec<&'static str> {
    const LANGUAGES: [&str; 12] = [
        "bash",
        "c",
        "cpp",
        "css",
        "html",
        "javascript",
        "json",
        "python",
        "rust",
        "sql",
        "typescript",
        "yaml",
    ];
    let prefix = prefix.to_ascii_lowercase();
    LANGUAGES
        .into_iter()
        .filter(|language| language.starts_with(&prefix))
        .collect()
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DocumentElement<'document> {
    HeadingMarker(&'document Heading),
    HeadingText(&'document Heading),
    SourceLanguage(SourceInfo),
    SourceAttribute(SourceInfo),
    MetadataTitle(&'document crate::block_model::BlockTitle),
    MetadataId(&'document MetadataValue),
    MetadataRole(&'document MetadataValue),
    MetadataOption(&'document MetadataValue),
    ElementAttribute(&'document ElementAttribute),
}

pub fn document_element_at(document: &Document, offset: u32) -> Option<DocumentElement<'_>> {
    document.element_at(offset)
}

pub(crate) fn document_element_at_ast(
    document: &AstDocument,
    offset: u32,
) -> Option<DocumentElement<'_>> {
    document.blocks().iter().find_map(|block| {
        let structural = match block {
            AstBlock::Heading(heading) if contains(heading.marker_range, offset, false) => {
                Some(DocumentElement::HeadingMarker(heading))
            }
            AstBlock::Heading(heading) if contains(heading.text_range, offset, true) => {
                Some(DocumentElement::HeadingText(heading))
            }
            AstBlock::Source(source)
                if source
                    .language_range
                    .is_some_and(|range| contains(range, offset, true)) =>
            {
                Some(DocumentElement::SourceLanguage(SourceInfo {
                    attribute_range: source.attribute_range,
                    language_range: source.language_range,
                    language: source.language.clone(),
                    line_numbers: false,
                    start_line: None,
                }))
            }
            AstBlock::Source(source) if contains(source.attribute_range, offset, false) => {
                Some(DocumentElement::SourceAttribute(SourceInfo {
                    attribute_range: source.attribute_range,
                    language_range: source.language_range,
                    language: source.language.clone(),
                    line_numbers: false,
                    start_line: None,
                }))
            }
            AstBlock::Verbatim(block)
                if matches!(&block.kind, crate::block_model::VerbatimKind::Source(source) if source.language_range.is_some_and(|range| contains(range, offset, true))) =>
            {
                let crate::block_model::VerbatimKind::Source(source) = &block.kind else {
                    unreachable!("match guard ensures source block")
                };
                Some(DocumentElement::SourceLanguage(source.clone()))
            }
            AstBlock::Verbatim(block)
                if matches!(&block.kind, crate::block_model::VerbatimKind::Source(source) if contains(source.attribute_range, offset, false)) =>
            {
                let crate::block_model::VerbatimKind::Source(source) = &block.kind else {
                    unreachable!("match guard ensures source block")
                };
                Some(DocumentElement::SourceAttribute(source.clone()))
            }
            _ => None,
        };
        structural.or_else(|| {
            let metadata = block.metadata();
            metadata
                .title
                .as_ref()
                .filter(|value| contains(value.range, offset, true))
                .map(DocumentElement::MetadataTitle)
                .or_else(|| {
                    metadata
                        .id
                        .as_ref()
                        .filter(|value| contains(value.range, offset, true))
                        .map(DocumentElement::MetadataId)
                })
                .or_else(|| {
                    metadata
                        .roles
                        .iter()
                        .find(|value| contains(value.range, offset, true))
                        .map(DocumentElement::MetadataRole)
                })
                .or_else(|| {
                    metadata
                        .options
                        .iter()
                        .find(|value| contains(value.range, offset, true))
                        .map(DocumentElement::MetadataOption)
                })
                .or_else(|| {
                    metadata
                        .attributes
                        .iter()
                        .find(|value| contains(value.range, offset, true))
                        .map(DocumentElement::ElementAttribute)
                })
        })
    })
}

fn contains(range: TextRange, offset: u32, include_end: bool) -> bool {
    range.start().to_u32() <= offset
        && if include_end {
            offset <= range.end().to_u32()
        } else {
            offset < range.end().to_u32()
        }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SymbolKind {
    DocumentTitle,
    Part,
    Section,
    ListItem,
}

impl SymbolKind {
    const fn as_str(self) -> &'static str {
        match self {
            Self::DocumentTitle => "document-title",
            Self::Part => "part",
            Self::Section => "section",
            Self::ListItem => "list-item",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DocumentSymbol {
    pub name: String,
    pub kind: SymbolKind,
    pub range: TextRange,
    pub selection_range: TextRange,
    pub children: Vec<DocumentSymbol>,
}

pub fn document_symbols(document: &Document) -> Vec<DocumentSymbol> {
    document.symbols()
}

pub(crate) fn document_symbols_ast(document: &AstDocument) -> Vec<DocumentSymbol> {
    document_symbols_ast_checked(document, &mut || false)
        .expect("a noncancellable symbol query cannot be cancelled")
}

pub(crate) fn document_symbols_cancellable(
    document: &Document,
    checkpoint: &mut crate::cancellation::CancellationCheckpoint<'_>,
) -> Result<Vec<DocumentSymbol>, ()> {
    document_symbols_ast_checked(document.inner(), &mut || checkpoint.is_cancelled())
}

fn document_symbols_ast_checked(
    document: &AstDocument,
    is_cancelled: &mut impl FnMut() -> bool,
) -> Result<Vec<DocumentSymbol>, ()> {
    let mut symbols = Vec::with_capacity(document.structure().roots().len());
    for section in document.structure().roots() {
        symbols.push(section_symbol_checked(section, is_cancelled)?);
    }
    let mut current_heading = None;
    for block in document.blocks() {
        if is_cancelled() {
            return Err(());
        }
        match block {
            AstBlock::Heading(heading) if !matches!(heading.kind, HeadingKind::Discrete { .. }) => {
                current_heading = Some(heading.range);
            }
            AstBlock::List(list) => {
                let children = list_symbols_checked(list, is_cancelled)?;
                let parent = current_heading
                    .map(|range| find_symbol_mut_checked(&mut symbols, range, is_cancelled))
                    .transpose()?
                    .flatten();
                if let Some(parent) = parent {
                    parent.children.extend(children);
                } else {
                    symbols.extend(children);
                }
            }
            _ => {}
        }
    }
    Ok(symbols)
}

fn section_symbol_checked(
    section: &crate::structure::Section,
    is_cancelled: &mut impl FnMut() -> bool,
) -> Result<DocumentSymbol, ()> {
    if is_cancelled() {
        return Err(());
    }
    let mut children = Vec::with_capacity(section.children.len());
    for child in &section.children {
        children.push(section_symbol_checked(child, is_cancelled)?);
    }
    Ok(DocumentSymbol {
        name: section.heading.title.clone(),
        kind: match section.heading.kind {
            crate::structure::SectionKind::DocumentTitle => SymbolKind::DocumentTitle,
            crate::structure::SectionKind::Part => SymbolKind::Part,
            crate::structure::SectionKind::Section | crate::structure::SectionKind::Appendix => {
                SymbolKind::Section
            }
            crate::structure::SectionKind::Discrete => SymbolKind::Section,
        },
        range: section.heading.range,
        selection_range: section.heading.title_range,
        children,
    })
}

fn list_symbols_checked(
    list: &crate::block_model::ListBlock,
    is_cancelled: &mut impl FnMut() -> bool,
) -> Result<Vec<DocumentSymbol>, ()> {
    let mut symbols = Vec::with_capacity(list.items.len());
    for item in &list.items {
        if is_cancelled() {
            return Err(());
        }
        let mut children = Vec::new();
        for child in &item.children {
            children.extend(list_symbols_checked(child, is_cancelled)?);
        }
        for continuation in &item.continuations {
            if let AstBlock::List(list) = continuation {
                children.extend(list_symbols_checked(list, is_cancelled)?);
            }
        }
        symbols.push(DocumentSymbol {
            name: if item.terms.is_empty() {
                item.text.clone()
            } else {
                item.terms
                    .iter()
                    .map(|term| term.text.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            },
            kind: SymbolKind::ListItem,
            range: item.range,
            selection_range: item.text_range,
            children,
        });
    }
    Ok(symbols)
}

fn find_symbol_mut_checked<'a>(
    symbols: &'a mut [DocumentSymbol],
    range: TextRange,
    is_cancelled: &mut impl FnMut() -> bool,
) -> Result<Option<&'a mut DocumentSymbol>, ()> {
    for symbol in symbols {
        if is_cancelled() {
            return Err(());
        }
        if symbol.range == range {
            return Ok(Some(symbol));
        }
        if let Some(found) = find_symbol_mut_checked(&mut symbol.children, range, is_cancelled)? {
            return Ok(Some(found));
        }
    }
    Ok(None)
}

pub fn render_symbols_json(symbols: &[DocumentSymbol]) -> String {
    fn render(output: &mut String, symbols: &[DocumentSymbol]) {
        output.push('[');
        for (index, symbol) in symbols.iter().enumerate() {
            if index != 0 {
                output.push(',');
            }
            write!(output, "{{\"name\":",).expect("writing to a String cannot fail");
            write_json_string(output, &symbol.name);
            write!(
                output,
                ",\"kind\":\"{}\",\"range\":{{\"start\":{},\"end\":{}}},\
                 \"selectionRange\":{{\"start\":{},\"end\":{}}},\"children\":",
                symbol.kind.as_str(),
                symbol.range.start().to_u32(),
                symbol.range.end().to_u32(),
                symbol.selection_range.start().to_u32(),
                symbol.selection_range.end().to_u32()
            )
            .expect("writing to a String cannot fail");
            render(output, &symbol.children);
            output.push('}');
        }
        output.push(']');
    }

    let mut output = String::new();
    render(&mut output, symbols);
    output
}

fn write_json_string(output: &mut String, value: &str) {
    crate::json::write_string(output, value);
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::{
        DocumentElement, ReferenceTargetKind, build_identifiers,
        document_element_at_ast as document_element_at, document_symbols_ast as document_symbols,
        document_symbols_ast_checked, generate_heading_ids_ast as generate_heading_ids,
        reference_targets_ast as reference_targets, render_symbols_json,
        source_language_candidates,
    };
    use crate::parser::parse;

    #[test]
    fn identifier_build_cancels_during_the_semantic_walk() {
        struct CancelAfterFirstCheckpoint(AtomicUsize);

        impl crate::core::CancellationCheck for CancelAfterFirstCheckpoint {
            fn is_cancelled(&self) -> bool {
                self.0.fetch_add(1, Ordering::Relaxed) >= 1
            }
        }

        let source = (0..crate::cancellation::CHECKPOINT_INTERVAL * 2)
            .map(|index| format!("== Heading {index}\n"))
            .collect::<String>();
        let parsed = parse(&source).expect("parse");
        let cancellation = CancelAfterFirstCheckpoint(AtomicUsize::new(0));

        let result = build_identifiers(
            &parsed.ast,
            &mut crate::cancellation::CancellationCheckpoint::new(&cancellation),
        );

        assert_eq!(result, Err(()));
        assert_eq!(cancellation.0.load(Ordering::Relaxed), 2);
    }

    #[test]
    fn source_block_language_candidates_are_deterministic_and_filtered() {
        assert_eq!(source_language_candidates("ru"), ["rust"]);
        assert_eq!(
            source_language_candidates(""),
            source_language_candidates("")
        );
    }

    #[test]
    fn document_element_at_distinguishes_heading_and_source_parts() {
        let source = "= 題名😀\n\n[source, ru]\n----\ncode\n----\n";
        let parsed = parse(source).expect("valid source");

        assert!(matches!(
            document_element_at(&parsed.ast, 0),
            Some(super::DocumentElement::HeadingMarker(_))
        ));
        assert!(matches!(
            document_element_at(&parsed.ast, 2),
            Some(super::DocumentElement::HeadingText(_))
        ));
        let language_end = source.find("ru]").expect("language") as u32 + 2;
        assert!(matches!(
            document_element_at(&parsed.ast, language_end),
            Some(super::DocumentElement::SourceLanguage(_))
        ));
        assert!(document_element_at(&parsed.ast, 13).is_none());
    }

    #[test]
    fn document_element_at_queries_common_block_metadata() {
        let source = ".Visible\n[#item.lead%collapsible,cols=2]\nParagraph\n";
        let parsed = parse(source).expect("valid source");

        for (needle, expected) in [
            ("Visible", "title"),
            ("item", "id"),
            ("lead", "role"),
            ("collapsible", "option"),
            ("cols=2", "attribute"),
        ] {
            let offset =
                u32::try_from(source.find(needle).expect("fixture value")).expect("offset");
            let actual = match document_element_at(&parsed.ast, offset) {
                Some(DocumentElement::MetadataTitle(_)) => "title",
                Some(DocumentElement::MetadataId(_)) => "id",
                Some(DocumentElement::MetadataRole(_)) => "role",
                Some(DocumentElement::MetadataOption(_)) => "option",
                Some(DocumentElement::ElementAttribute(_)) => "attribute",
                other => panic!("unexpected element at {needle}: {other:?}"),
            };
            assert_eq!(actual, expected);
        }
    }

    #[test]
    fn document_symbols_follow_heading_hierarchy() {
        let parsed = parse("= Title\n\n== One\n=== Child\n== Two").expect("valid source");
        let symbols = document_symbols(&parsed.ast);

        assert_eq!(symbols.len(), 1);
        assert_eq!(symbols[0].name, "Title");
        assert_eq!(symbols[0].children.len(), 2);
        assert_eq!(symbols[0].children[0].name, "One");
        assert_eq!(symbols[0].children[0].children[0].name, "Child");
        assert_eq!(symbols[0].children[1].name, "Two");
    }

    #[test]
    fn document_symbol_generation_is_cancellable() {
        let source = (0..crate::cancellation::CHECKPOINT_INTERVAL * 2)
            .map(|index| format!("== Section {index}\n"))
            .collect::<String>();
        let parsed = parse(&source).expect("valid source");
        let mut checks = 0;

        let result = document_symbols_ast_checked(&parsed.ast, &mut || {
            checks += 1;
            checks > crate::cancellation::CHECKPOINT_INTERVAL
        });

        assert_eq!(result, Err(()));
        assert_eq!(checks, crate::cancellation::CHECKPOINT_INTERVAL + 1);
    }

    #[test]
    fn document_symbols_and_ids_are_deterministic() {
        let parsed = parse("== Same\n== Same").expect("valid source");

        assert_eq!(
            generate_heading_ids(&parsed.ast)
                .iter()
                .map(|heading| heading.id.as_str())
                .collect::<Vec<_>>(),
            ["_same", "_same_2"]
        );
        assert_eq!(
            render_symbols_json(&document_symbols(&parsed.ast)),
            render_symbols_json(&document_symbols(&parsed.ast))
        );
    }

    #[test]
    fn nested_headings_share_document_wide_ids_with_html_and_xrefs() {
        let parsed = parse("== Same\n\n====\n== Same\n\n<<_same_2,Nested>>\n====\n")
            .expect("nested headings");
        let ids = parsed.ast.identifiers().heading_ids();

        assert_eq!(
            ids.iter()
                .map(|heading| heading.id.as_str())
                .collect::<Vec<_>>(),
            ["_same", "_same_2"]
        );
        assert_eq!(parsed.ast.structure().headings().len(), 1);
        let html = crate::html::render_with_inputs_ast(
            &parsed.ast,
            &crate::html::RenderPolicy::default(),
            &crate::render::RenderInputs::default(),
        )
        .html;
        assert!(html.contains("<h1 id=\"_same\">Same</h1>"));
        assert!(html.contains("<h1 id=\"_same_2\">Same</h1>"));
        assert!(html.contains("href=\"#_same_2\""));
    }

    #[test]
    fn anchors_create_stable_reference_targets_and_override_heading_ids() {
        let parsed =
            parse("= Title\n\n[[stable,表示名]]\n== Generated title\n\n[#paragraph]\nParagraph\n")
                .expect("parse");
        let targets = reference_targets(&parsed.ast);

        assert_eq!(
            targets
                .iter()
                .map(|target| (target.kind, target.id.as_str(), target.label.as_str()))
                .collect::<Vec<_>>(),
            [
                (ReferenceTargetKind::DocumentTitle, "_title", "Title"),
                (ReferenceTargetKind::Section, "stable", "表示名"),
                (
                    ReferenceTargetKind::ExplicitAnchor,
                    "paragraph",
                    "Paragraph"
                ),
            ]
        );
        assert_eq!(
            generate_heading_ids(&parsed.ast)
                .iter()
                .map(|heading| heading.id.as_str())
                .collect::<Vec<_>>(),
            ["_title", "stable"]
        );
    }

    #[test]
    fn anchors_keep_unicode_combining_emoji_and_case_distinct() {
        let parsed =
            parse(include_str!("../../../fixtures/anchors/boundaries.adoc")).expect("parse");
        let ids = reference_targets(&parsed.ast)
            .into_iter()
            .map(|target| target.id)
            .collect::<Vec<_>>();

        assert_eq!(ids, ["_文書", "日本語", "Café", "😀", "Case", "case"]);
        assert_eq!(
            reference_targets(&parsed.ast),
            reference_targets(&parsed.ast)
        );
    }
}
