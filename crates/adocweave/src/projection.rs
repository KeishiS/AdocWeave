//! Deterministic, host-independent projections derived from one [`Analysis`].

use std::collections::BTreeSet;

use crate::block_model::AstBlock;
use crate::core::{Analysis, SourceId};
use crate::document::{ReferenceTarget, ReferenceTargetKind};
use crate::inline_model::{Inline, Link};
use crate::reference::{ReferenceKey, ResolutionOutcome};
use crate::render::{RenderInputs, ResolutionMatch};
use crate::source::TextRange;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DocumentProjection {
    pub package_version: &'static str,
    pub source_id: Option<SourceId>,
    pub title: Option<ProjectedText>,
    pub targets: Vec<ReferenceTarget>,
    pub external_links: Vec<ExternalLink>,
    pub reference_edges: Vec<ReferenceEdge>,
    pub source_blocks: Vec<SourceBlockProjection>,
    pub ordered_lists: Vec<OrderedListProjection>,
    pub block_presentations: Vec<BlockPresentationProjection>,
    pub formulas: Vec<FormulaProjection>,
    /// Citations of entries held by a bibliography library outside the document.
    ///
    /// AdocWeave never resolves these keys. A host reads them, resolves them
    /// against its own library, and passes the result back for rendering.
    pub citations: Vec<crate::citation::Citation>,
    pub searchable_text: SearchableText,
    pub catalogs: crate::catalog::DocumentCatalogs,
    pub structure: crate::structure::DocumentStructure,
    pub presentation: crate::presentation::DocumentPresentation,
}

/// Semantic features that a host may need to render a document.
///
/// The values describe document content only. They do not select renderer
/// implementations, JavaScript libraries, themes, or asset URLs.
#[non_exhaustive]
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct RenderingFeatures {
    pub math_languages: Vec<String>,
    pub source_languages: Vec<String>,
    pub table_of_contents: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SourceBlockProjection {
    pub source_range: TextRange,
    pub content_range: TextRange,
    pub title: Option<ProjectedText>,
    pub language_range: Option<TextRange>,
    pub language: Option<String>,
    pub line_numbers: bool,
    pub start_line: Option<u32>,
    pub source: String,
}

/// Presentation facts for an ordered list, resolved once during lowering.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct OrderedListProjection {
    pub source_range: TextRange,
    pub start: Option<u32>,
    pub reversed: bool,
    pub style: crate::block_model::OrderedListStyle,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BlockPresentationKind {
    Admonition,
    Quote,
    Verse,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BlockPresentationProjection {
    pub kind: BlockPresentationKind,
    pub source_range: TextRange,
    pub content_range: TextRange,
    pub title: Option<String>,
    pub attribution: Option<String>,
    pub citation: Option<String>,
}

impl BlockPresentationKind {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Admonition => "admonition",
            Self::Quote => "quote",
            Self::Verse => "verse",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FormulaKind {
    Inline,
    Block,
}

impl OrderedListProjection {
    const fn style_name(self) -> &'static str {
        match self.style {
            crate::block_model::OrderedListStyle::Arabic => "arabic",
            crate::block_model::OrderedListStyle::Decimal => "decimal",
            crate::block_model::OrderedListStyle::LowerAlpha => "loweralpha",
            crate::block_model::OrderedListStyle::UpperAlpha => "upperalpha",
            crate::block_model::OrderedListStyle::LowerRoman => "lowerroman",
            crate::block_model::OrderedListStyle::UpperRoman => "upperroman",
            crate::block_model::OrderedListStyle::LowerGreek => "lowergreek",
        }
    }
}

impl FormulaKind {
    /// Stable display-form name used by serialized and HTML contracts.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Inline => "inline",
            Self::Block => "block",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FormulaProjection {
    pub kind: FormulaKind,
    pub language: crate::inline_model::MathLanguage,
    pub source_range: TextRange,
    pub content_range: TextRange,
    pub source: String,
}

impl FormulaProjection {
    /// Inline or block display form without inferring it from source syntax.
    pub const fn display(&self) -> FormulaKind {
        self.kind
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProjectedText {
    pub source_range: TextRange,
    pub text: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExternalLink {
    pub source_range: TextRange,
    pub target_range: TextRange,
    pub target: String,
    pub label: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReferenceEdge {
    pub source_id: Option<SourceId>,
    pub source_range: TextRange,
    pub target: ReferenceKey,
    pub resolution: Option<ResolutionOutcome>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SearchTextKind {
    Prose,
    Code,
}

impl SearchTextKind {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Prose => "prose",
            Self::Code => "code",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SearchTextSegment {
    pub kind: SearchTextKind,
    pub source_range: TextRange,
    pub text: String,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct SearchableText {
    pub text: String,
    pub segments: Vec<SearchTextSegment>,
}

pub fn project(analysis: &Analysis, inputs: &RenderInputs) -> DocumentProjection {
    let title = analysis
        .ast()
        .blocks()
        .iter()
        .find_map(|block| match block {
            AstBlock::Heading(heading)
                if matches!(heading.kind, crate::block_model::HeadingKind::DocumentTitle) =>
            {
                Some(ProjectedText {
                    source_range: heading.text_range,
                    text: inline_text(&heading.inlines),
                })
            }
            _ => None,
        });

    let mut external_links = Vec::new();
    crate::walker::walk(analysis.document(), |node| {
        if let crate::walker::SemanticNode::Inline(Inline::Link(link)) = node {
            external_links.push(project_link(link));
        }
    });
    external_links.sort_by_key(|link| (link.source_range.start(), link.source_range.end()));

    let reference_edges = analysis
        .references()
        .iter()
        .filter_map(|reference| {
            let target = reference.target.clone()?;
            let resolution = match inputs.reference_at(reference.range) {
                ResolutionMatch::Unique(resolution) => Some(resolution.outcome.clone()),
                ResolutionMatch::Missing | ResolutionMatch::Duplicate => None,
            };
            Some(ReferenceEdge {
                source_id: analysis.source_id().cloned(),
                source_range: reference.range,
                target,
                resolution,
            })
        })
        .collect();

    let mut source_blocks = Vec::new();
    let mut ordered_lists = Vec::new();
    let mut block_presentations = Vec::new();
    let mut formulas = Vec::new();
    crate::walker::walk(analysis.document(), |node| match node {
        crate::walker::SemanticNode::Block(AstBlock::Source(source)) => {
            source_blocks.push(SourceBlockProjection {
                source_range: source.range,
                content_range: source.content_range,
                title: source.metadata.title.as_ref().map(|title| ProjectedText {
                    source_range: title.range,
                    text: resolved_inline_text(&title.inlines),
                }),
                language_range: source.language_range,
                language: source.language.clone(),
                line_numbers: false,
                start_line: None,
                source: source.value.clone(),
            });
        }
        crate::walker::SemanticNode::Block(AstBlock::Verbatim(block))
            if matches!(block.kind, crate::block_model::VerbatimKind::Source(_)) =>
        {
            let crate::block_model::VerbatimKind::Source(source) = &block.kind else {
                unreachable!("match guard ensures source verbatim block")
            };
            source_blocks.push(SourceBlockProjection {
                source_range: block.range,
                content_range: block.content_range,
                title: block.metadata.title.as_ref().map(|title| ProjectedText {
                    source_range: title.range,
                    text: resolved_inline_text(&title.inlines),
                }),
                language_range: source.language_range,
                language: source.language.clone(),
                line_numbers: source.line_numbers,
                start_line: source.start_line,
                source: block.value.clone(),
            });
        }
        crate::walker::SemanticNode::Inline(Inline::Formula(formula)) => {
            formulas.push(FormulaProjection {
                kind: FormulaKind::Inline,
                language: formula.language,
                source_range: formula.range,
                content_range: formula.content_range,
                source: formula.value.clone(),
            });
        }
        crate::walker::SemanticNode::Block(AstBlock::Math(formula)) => {
            formulas.push(FormulaProjection {
                kind: FormulaKind::Block,
                language: formula.language,
                source_range: formula.range,
                content_range: formula.content_range,
                source: formula.value.clone(),
            });
        }
        crate::walker::SemanticNode::Block(AstBlock::List(list))
            if list.kind == crate::block_model::ListKind::Ordered =>
        {
            ordered_lists.push(OrderedListProjection {
                source_range: list.range,
                start: list.presentation.start,
                reversed: list.presentation.reversed,
                style: list.presentation.style,
            });
        }
        crate::walker::SemanticNode::Block(AstBlock::Paragraph(value))
            if value.admonition.is_some() =>
        {
            block_presentations.push(BlockPresentationProjection {
                kind: BlockPresentationKind::Admonition,
                source_range: value.range,
                content_range: value.content_range,
                title: value
                    .metadata
                    .title
                    .as_ref()
                    .map(|value| value.value.clone()),
                attribution: None,
                citation: None,
            });
        }
        crate::walker::SemanticNode::Block(AstBlock::Delimited(value)) => {
            if let Some(presentation) = &value.presentation {
                match presentation {
                    crate::block_model::DelimitedPresentation::Admonition(_) => block_presentations
                        .push(BlockPresentationProjection {
                            kind: BlockPresentationKind::Admonition,
                            source_range: value.range,
                            content_range: value.content_range,
                            title: value
                                .metadata
                                .title
                                .as_ref()
                                .map(|item| resolved_inline_text(&item.inlines)),
                            attribution: None,
                            citation: None,
                        }),
                    crate::block_model::DelimitedPresentation::Quote(quote) => block_presentations
                        .push(BlockPresentationProjection {
                            kind: match quote.kind {
                                crate::block_model::QuoteKind::Quote => {
                                    BlockPresentationKind::Quote
                                }
                                crate::block_model::QuoteKind::Verse => {
                                    BlockPresentationKind::Verse
                                }
                            },
                            source_range: value.range,
                            content_range: value.content_range,
                            title: value
                                .metadata
                                .title
                                .as_ref()
                                .map(|item| resolved_inline_text(&item.inlines)),
                            attribution: quote.attribution.as_ref().map(|item| item.value.clone()),
                            citation: quote.citation.as_ref().map(|item| item.value.clone()),
                        }),
                }
            }
        }
        _ => {}
    });
    source_blocks.sort_by_key(|source| (source.source_range.start(), source.source_range.end()));
    ordered_lists.sort_by_key(|list| (list.source_range.start(), list.source_range.end()));
    block_presentations.sort_by_key(|block| (block.source_range.start(), block.source_range.end()));
    formulas.sort_by_key(|formula| (formula.source_range.start(), formula.source_range.end()));

    DocumentProjection {
        package_version: crate::VERSION,
        source_id: analysis.source_id().cloned(),
        title,
        targets: analysis.reference_targets().to_vec(),
        external_links,
        reference_edges,
        source_blocks,
        ordered_lists,
        block_presentations,
        formulas,
        citations: analysis.citations(),
        searchable_text: searchable_text(analysis),
        catalogs: analysis.catalogs().clone(),
        structure: analysis.structure().clone(),
        presentation: analysis.presentation().clone(),
    }
}

pub fn searchable_text(analysis: &Analysis) -> SearchableText {
    let mut segments = Vec::new();
    collect_search_blocks(analysis.ast().blocks(), &mut segments);
    let text = segments
        .iter()
        .map(|segment| segment.text.as_str())
        .filter(|text| !text.is_empty())
        .collect::<Vec<_>>()
        .join("\n");
    SearchableText { text, segments }
}

fn project_link(link: &Link) -> ExternalLink {
    let label = inline_text(&link.label);
    ExternalLink {
        source_range: link.range,
        target_range: link.target_range,
        target: link.target.clone(),
        label: if label.is_empty() {
            link.target.clone()
        } else {
            label
        },
    }
}

const fn role_search_kind(role: crate::text_role::BlockTextRole) -> Option<SearchTextKind> {
    match role {
        crate::text_role::BlockTextRole::Prose => Some(SearchTextKind::Prose),
        crate::text_role::BlockTextRole::Code => Some(SearchTextKind::Code),
        crate::text_role::BlockTextRole::Container | crate::text_role::BlockTextRole::Excluded => {
            None
        }
    }
}

fn push_search_role(
    output: &mut Vec<SearchTextSegment>,
    role: crate::text_role::BlockTextRole,
    source_range: TextRange,
    text: String,
) {
    if let Some(kind) = role_search_kind(role) {
        push_search(output, kind, source_range, text);
    }
}

fn collect_search_blocks(blocks: &[AstBlock], output: &mut Vec<SearchTextSegment>) {
    crate::walker::walk_block_slice(blocks, |node| match node {
        crate::walker::SemanticNode::Block(block @ AstBlock::Heading(heading)) => push_search_role(
            output,
            crate::text_role::block_text_role(block),
            heading.text_range,
            inline_text(&heading.inlines),
        ),
        crate::walker::SemanticNode::Block(block @ AstBlock::Paragraph(paragraph)) => {
            push_search_role(
                output,
                crate::text_role::block_text_role(block),
                paragraph.content_range,
                fold_line_endings(&inline_text(&paragraph.inlines)),
            );
        }
        crate::walker::SemanticNode::Block(block @ AstBlock::LiteralParagraph(literal)) => {
            push_search_role(
                output,
                crate::text_role::block_text_role(block),
                literal.content_range,
                literal.value.clone(),
            );
        }
        crate::walker::SemanticNode::Block(
            block @ (AstBlock::Source(_) | AstBlock::Verbatim(_)),
        ) => {
            let (content_range, value) = match block {
                AstBlock::Source(source) => (source.content_range, source.value.clone()),
                AstBlock::Verbatim(source) => (source.content_range, source.value.clone()),
                _ => unreachable!("the pattern admits only source and verbatim blocks"),
            };
            push_search_role(
                output,
                crate::text_role::block_text_role(block),
                content_range,
                value,
            );
        }
        crate::walker::SemanticNode::Block(AstBlock::Delimited(block)) => {
            if let crate::block_model::DelimitedContent::Verbatim(value) = &block.content
                && matches!(
                    crate::text_role::delimited_text_role(block.kind),
                    crate::text_role::BlockTextRole::Code
                )
            {
                push_search(
                    output,
                    SearchTextKind::Code,
                    block.content_range,
                    value.clone(),
                );
            }
        }
        crate::walker::SemanticNode::ListItem(item) => {
            for term in &item.terms {
                push_search(
                    output,
                    SearchTextKind::Prose,
                    term.range,
                    inline_text(&term.inlines),
                );
            }
            push_search(
                output,
                SearchTextKind::Prose,
                item.text_range,
                inline_text(&item.inlines),
            );
        }
        crate::walker::SemanticNode::TableCell(cell) => {
            let role = crate::text_role::table_cell_text_role(&cell.content);
            match &cell.content {
                crate::table::TableCellContent::Inlines(inlines) => {
                    push_search_role(output, role, cell.content_range, inline_text(inlines));
                }
                crate::table::TableCellContent::Verbatim(value) => {
                    push_search_role(output, role, cell.content_range, value.clone());
                }
                crate::table::TableCellContent::AsciiDoc(_) => {}
            }
        }
        crate::walker::SemanticNode::Block(
            AstBlock::Break(_) | AstBlock::List(_) | AstBlock::Math(_) | AstBlock::Unsupported(_),
        )
        | crate::walker::SemanticNode::List(_)
        | crate::walker::SemanticNode::Table(_)
        | crate::walker::SemanticNode::TableRow(_)
        | crate::walker::SemanticNode::Inline(_)
        | crate::walker::SemanticNode::Attribute(_)
        | crate::walker::SemanticNode::Anchor(_)
        | crate::walker::SemanticNode::Metadata(_)
        | crate::walker::SemanticNode::MetadataTitle(_)
        | crate::walker::SemanticNode::MetadataId(_)
        | crate::walker::SemanticNode::MetadataRole(_)
        | crate::walker::SemanticNode::MetadataOption(_)
        | crate::walker::SemanticNode::ElementAttribute(_) => {}
    });
}

fn push_search(
    output: &mut Vec<SearchTextSegment>,
    kind: SearchTextKind,
    source_range: TextRange,
    text: String,
) {
    let text = text.trim_end_matches(['\r', '\n']).to_owned();
    if !text.is_empty() {
        output.push(SearchTextSegment {
            kind,
            source_range,
            text,
        });
    }
}

fn inline_text(inlines: &[Inline]) -> String {
    inline_text_with_attributes(inlines, false)
}

pub(crate) fn resolved_inline_text(inlines: &[Inline]) -> String {
    inline_text_with_attributes(inlines, true)
}

fn inline_text_with_attributes(inlines: &[Inline], include_attribute_values: bool) -> String {
    let mut output = String::new();
    for inline in inlines {
        match inline {
            Inline::Text(text) => output.push_str(&text.value),
            Inline::Literal { value, .. } => output.push_str(value),
            Inline::Styled { children, .. } => output.push_str(&inline_text_with_attributes(
                children,
                include_attribute_values,
            )),
            Inline::AttributeReference { value, .. } => {
                if include_attribute_values {
                    push_attribute_text(&mut output, value.as_deref().unwrap_or_default());
                }
            }
            Inline::Formula(_) => {}
            Inline::Macro(node) => {
                use crate::inline_model::StandardMacroKind as Kind;
                match node.kind {
                    // A citation carries no readable text of its own: the display
                    // string comes from the host that resolves the key.
                    Kind::Anchor | Kind::BibliographyAnchor | Kind::Citation | Kind::IndexTerm => {}
                    Kind::Email => output.push_str(&node.target),
                    Kind::Footnote
                    | Kind::Keyboard
                    | Kind::Button
                    | Kind::Menu
                    | Kind::Image
                    | Kind::Icon
                    | Kind::Audio
                    | Kind::Video => {
                        if let Some(label) = node.attributes.first() {
                            output.push_str(&label.value);
                        } else {
                            output.push_str(&node.target);
                        }
                    }
                }
            }
            Inline::HardBreak { .. } => output.push('\n'),
            Inline::Passthrough { value, .. } => output.push_str(value),
            Inline::Link(link) => {
                let label = inline_text_with_attributes(&link.label, include_attribute_values);
                output.push_str(if label.is_empty() {
                    &link.target
                } else {
                    &label
                });
            }
            Inline::Reference(reference) => {
                let label = inline_text_with_attributes(&reference.label, include_attribute_values);
                output.push_str(if label.is_empty() {
                    &reference.target_source
                } else {
                    &label
                });
            }
        }
    }
    output
}

fn push_attribute_text(output: &mut String, value: &str) {
    let mut remaining = value;
    while let Some(index) = remaining.find(" +\n") {
        output.push_str(&remaining[..index]);
        output.push('\n');
        remaining = &remaining[index + 3..];
    }
    output.push_str(remaining);
}

fn fold_line_endings(value: &str) -> String {
    value
        .lines()
        .map(|line| line.trim_end_matches([' ', '\t']))
        .collect::<Vec<_>>()
        .join(" ")
}

impl DocumentProjection {
    /// Returns normalized rendering requirements for this projected document.
    ///
    /// Languages are canonicalized, unique, and returned in their documented
    /// stable order.
    /// The TOC value reports whether the projected TOC has any entries.
    pub fn rendering_features(&self) -> RenderingFeatures {
        let math_languages = self
            .formulas
            .iter()
            .map(|formula| math_language_feature(formula.language))
            .collect::<BTreeSet<_>>()
            .into_iter()
            .map(|(_, name)| name.to_owned())
            .collect();
        let source_languages = self
            .source_blocks
            .iter()
            .filter_map(|source| source.language.as_deref())
            .map(canonical_source_language)
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();

        RenderingFeatures {
            math_languages,
            source_languages,
            table_of_contents: self.presentation.toc_policy().enabled
                && !self.presentation.toc().is_empty(),
        }
    }

    /// Stable JSON with the crate's fixed key order and escaping.
    pub fn render_json(&self) -> String {
        serde_json::to_string(&wire::Doc::new(self)).expect("projection serializes to JSON")
    }
}

pub(crate) fn canonical_source_language(language: &str) -> String {
    language
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '-' | '_') {
                character.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect()
}

const fn math_language_feature(language: crate::inline_model::MathLanguage) -> (u8, &'static str) {
    match language {
        crate::inline_model::MathLanguage::Latex => (0, "latexmath"),
        crate::inline_model::MathLanguage::Typst => (1, "typst"),
    }
}

const fn math_language(language: crate::inline_model::MathLanguage) -> &'static str {
    match language {
        crate::inline_model::MathLanguage::Latex => "latex",
        crate::inline_model::MathLanguage::Typst => "typst",
    }
}

const fn structure_kind(kind: crate::structure::SectionKind) -> &'static str {
    match kind {
        crate::structure::SectionKind::DocumentTitle => "document-title",
        crate::structure::SectionKind::Part => "part",
        crate::structure::SectionKind::Section => "section",
        crate::structure::SectionKind::Appendix => "appendix",
        crate::structure::SectionKind::Discrete => "discrete",
    }
}

const fn reference_target_kind(kind: ReferenceTargetKind) -> &'static str {
    match kind {
        ReferenceTargetKind::DocumentTitle => "document-title",
        ReferenceTargetKind::Part => "part",
        ReferenceTargetKind::Section => "section",
        ReferenceTargetKind::ExplicitAnchor => "explicit-anchor",
        ReferenceTargetKind::InlineAnchor => "inline-anchor",
    }
}

/// Serde views that pin the projection's public JSON key order.
mod wire {
    use serde::Serialize;

    use super::{
        DocumentProjection, ProjectedText, ReferenceKey, ResolutionOutcome, SourceId, TextRange,
        math_language, reference_target_kind, structure_kind,
    };

    #[derive(Serialize)]
    struct RangeW {
        start: u32,
        end: u32,
    }

    impl From<TextRange> for RangeW {
        fn from(range: TextRange) -> Self {
            Self {
                start: range.start().to_u32(),
                end: range.end().to_u32(),
            }
        }
    }

    #[derive(Serialize)]
    struct TextW<'a> {
        #[serde(rename = "sourceRange")]
        source_range: RangeW,
        text: &'a str,
    }

    impl<'a> From<&'a ProjectedText> for TextW<'a> {
        fn from(text: &'a ProjectedText) -> Self {
            Self {
                source_range: text.source_range.into(),
                text: &text.text,
            }
        }
    }

    #[derive(Serialize)]
    #[serde(rename_all = "camelCase")]
    pub(super) struct Doc<'a> {
        package_version: &'a str,
        source_id: Option<&'a str>,
        title: Option<TextW<'a>>,
        targets: Vec<TargetW<'a>>,
        external_links: Vec<LinkW<'a>>,
        reference_edges: Vec<EdgeW<'a>>,
        source_blocks: Vec<SourceBlockW<'a>>,
        formulas: Vec<FormulaW<'a>>,
        citations: Vec<CitationW<'a>>,
        ordered_lists: Vec<OrderedListW>,
        block_presentations: Vec<BlockPresentationW<'a>>,
        structure: StructureW<'a>,
        catalogs: CatalogsW<'a>,
        searchable_text: SearchableTextW<'a>,
    }

    #[derive(Serialize)]
    #[serde(rename_all = "camelCase")]
    struct TargetW<'a> {
        kind: &'static str,
        id: &'a str,
        label: &'a str,
        id_range: RangeW,
        target_range: RangeW,
    }

    #[derive(Serialize)]
    #[serde(rename_all = "camelCase")]
    struct LinkW<'a> {
        source_range: RangeW,
        target_range: RangeW,
        target: &'a str,
        label: &'a str,
    }

    #[derive(Serialize)]
    #[serde(rename_all = "camelCase")]
    struct EdgeW<'a> {
        source_id: Option<&'a str>,
        source_range: RangeW,
        target: KeyW<'a>,
        resolution: Option<ResolutionW<'a>>,
    }

    #[derive(Serialize)]
    #[serde(untagged)]
    enum KeyW<'a> {
        Local {
            kind: &'static str,
            anchor: &'a str,
        },
        Document {
            kind: &'static str,
            document: &'a str,
            anchor: Option<&'a str>,
        },
        Scheme {
            kind: &'static str,
            scheme: &'a str,
            locator: &'a str,
            anchor: Option<&'a str>,
        },
    }

    impl<'a> From<&'a ReferenceKey> for KeyW<'a> {
        fn from(key: &'a ReferenceKey) -> Self {
            match key {
                ReferenceKey::Local { anchor } => Self::Local {
                    kind: "local",
                    anchor,
                },
                ReferenceKey::Document { document, anchor } => Self::Document {
                    kind: "document",
                    document,
                    anchor: anchor.as_deref(),
                },
                ReferenceKey::Scheme {
                    scheme,
                    locator,
                    anchor,
                } => Self::Scheme {
                    kind: "scheme",
                    scheme,
                    locator,
                    anchor: anchor.as_deref(),
                },
            }
        }
    }

    #[derive(Serialize)]
    #[serde(untagged)]
    enum ResolutionW<'a> {
        #[serde(rename_all = "camelCase")]
        Resolved {
            status: &'static str,
            href: &'a str,
            display_text: Option<&'a str>,
            notices: Vec<&'static str>,
        },
        Failed {
            status: &'static str,
            kind: &'static str,
        },
    }

    impl<'a> From<&'a ResolutionOutcome> for ResolutionW<'a> {
        fn from(outcome: &'a ResolutionOutcome) -> Self {
            match outcome {
                ResolutionOutcome::Resolved {
                    href,
                    display_text,
                    notices,
                } => Self::Resolved {
                    status: "resolved",
                    href,
                    display_text: display_text.as_deref(),
                    notices: notices
                        .iter()
                        .map(|notice| notice.kind.diagnostic_code())
                        .collect(),
                },
                ResolutionOutcome::Failed(failure) => Self::Failed {
                    status: "failed",
                    kind: failure.kind.diagnostic_code(),
                },
            }
        }
    }

    #[derive(Serialize)]
    #[serde(rename_all = "camelCase")]
    struct SourceBlockW<'a> {
        source_range: RangeW,
        content_range: RangeW,
        title: Option<TextW<'a>>,
        language_range: Option<RangeW>,
        language: Option<&'a str>,
        line_numbers: bool,
        start_line: Option<u32>,
        source: &'a str,
    }

    #[derive(Serialize)]
    #[serde(rename_all = "camelCase")]
    struct FormulaW<'a> {
        kind: &'static str,
        language: &'static str,
        source_range: RangeW,
        content_range: RangeW,
        source: &'a str,
    }

    #[derive(Serialize)]
    #[serde(rename_all = "camelCase")]
    struct CitationW<'a> {
        order: u32,
        source_range: RangeW,
        keys: Vec<CitationKeyW<'a>>,
        attributes: Vec<CitationAttributeW<'a>>,
    }

    #[derive(Serialize)]
    #[serde(rename_all = "camelCase")]
    struct CitationKeyW<'a> {
        source_range: RangeW,
        key: &'a str,
    }

    #[derive(Serialize)]
    #[serde(rename_all = "camelCase")]
    struct CitationAttributeW<'a> {
        source_range: RangeW,
        name: Option<&'a str>,
        value: &'a str,
    }

    #[derive(Serialize)]
    #[serde(rename_all = "camelCase")]
    struct OrderedListW {
        source_range: RangeW,
        start: Option<u32>,
        reversed: bool,
        style: &'static str,
    }

    #[derive(Serialize)]
    #[serde(rename_all = "camelCase")]
    struct BlockPresentationW<'a> {
        kind: &'static str,
        source_range: RangeW,
        content_range: RangeW,
        title: Option<&'a str>,
        attribution: Option<&'a str>,
        citation: Option<&'a str>,
    }

    #[derive(Serialize)]
    #[serde(rename_all = "camelCase")]
    struct StructureW<'a> {
        headings: Vec<HeadingW<'a>>,
        toc: Vec<TocW<'a>>,
        manpage: Option<ManpageW<'a>>,
    }

    #[derive(Serialize)]
    #[serde(rename_all = "camelCase")]
    struct HeadingW<'a> {
        kind: &'static str,
        level: u8,
        id: &'a str,
        id_range: RangeW,
        title: &'a str,
        range: RangeW,
        title_range: RangeW,
        number: &'a [u32],
        toc_included: bool,
    }

    #[derive(Serialize)]
    #[serde(rename_all = "camelCase")]
    struct TocW<'a> {
        id: &'a str,
        title: &'a str,
        level: u8,
        number: &'a [u32],
        range: RangeW,
        children: Vec<TocW<'a>>,
    }

    impl<'a> From<&'a crate::structure::TocEntry> for TocW<'a> {
        fn from(entry: &'a crate::structure::TocEntry) -> Self {
            Self {
                id: &entry.id,
                title: &entry.title,
                level: entry.level,
                number: &entry.number,
                range: entry.range.into(),
                children: entry.children.iter().map(TocW::from).collect(),
            }
        }
    }

    #[derive(Serialize)]
    #[serde(rename_all = "camelCase")]
    struct ManpageW<'a> {
        name: &'a str,
        section: &'a str,
        purpose: &'a str,
        title_range: RangeW,
        name_range: RangeW,
        purpose_range: RangeW,
    }

    #[derive(Serialize)]
    #[serde(rename_all = "camelCase")]
    struct CatalogsW<'a> {
        footnotes: Vec<FootnoteW<'a>>,
        bibliography: Vec<BibliographyW<'a>>,
        index: Vec<IndexW<'a>>,
    }

    #[derive(Serialize)]
    #[serde(rename_all = "camelCase")]
    struct FootnoteW<'a> {
        number: u32,
        id: Option<&'a str>,
        definition_range: RangeW,
        content_range: RangeW,
        text: &'a str,
        occurrences: Vec<RangeW>,
    }

    #[derive(Serialize)]
    #[serde(rename_all = "camelCase")]
    struct BibliographyW<'a> {
        id: &'a str,
        label: Option<&'a str>,
        definition_range: RangeW,
        references: Vec<RangeW>,
    }

    #[derive(Serialize)]
    #[serde(rename_all = "camelCase")]
    struct IndexW<'a> {
        terms: &'a [String],
        display: &'a str,
        occurrences: Vec<RangeW>,
    }

    #[derive(Serialize)]
    #[serde(rename_all = "camelCase")]
    struct SearchableTextW<'a> {
        text: &'a str,
        segments: Vec<SegmentW<'a>>,
    }

    #[derive(Serialize)]
    #[serde(rename_all = "camelCase")]
    struct SegmentW<'a> {
        kind: &'static str,
        source_range: RangeW,
        text: &'a str,
    }

    impl<'a> Doc<'a> {
        pub(super) fn new(doc: &'a DocumentProjection) -> Self {
            Self {
                package_version: doc.package_version,
                source_id: doc.source_id.as_ref().map(SourceId::as_str),
                title: doc.title.as_ref().map(TextW::from),
                targets: doc
                    .targets
                    .iter()
                    .map(|target| TargetW {
                        kind: reference_target_kind(target.kind),
                        id: &target.id,
                        label: &target.label,
                        id_range: target.id_range.into(),
                        target_range: target.target_range.into(),
                    })
                    .collect(),
                external_links: doc
                    .external_links
                    .iter()
                    .map(|link| LinkW {
                        source_range: link.source_range.into(),
                        target_range: link.target_range.into(),
                        target: &link.target,
                        label: &link.label,
                    })
                    .collect(),
                reference_edges: doc
                    .reference_edges
                    .iter()
                    .map(|edge| EdgeW {
                        source_id: edge.source_id.as_ref().map(SourceId::as_str),
                        source_range: edge.source_range.into(),
                        target: KeyW::from(&edge.target),
                        resolution: edge.resolution.as_ref().map(ResolutionW::from),
                    })
                    .collect(),
                source_blocks: doc
                    .source_blocks
                    .iter()
                    .map(|source| SourceBlockW {
                        source_range: source.source_range.into(),
                        content_range: source.content_range.into(),
                        title: source.title.as_ref().map(TextW::from),
                        language_range: source.language_range.map(RangeW::from),
                        language: source.language.as_deref(),
                        line_numbers: source.line_numbers,
                        start_line: source.start_line,
                        source: &source.source,
                    })
                    .collect(),
                formulas: doc
                    .formulas
                    .iter()
                    .map(|formula| FormulaW {
                        kind: formula.kind.as_str(),
                        language: math_language(formula.language),
                        source_range: formula.source_range.into(),
                        content_range: formula.content_range.into(),
                        source: &formula.source,
                    })
                    .collect(),
                citations: doc
                    .citations
                    .iter()
                    .map(|citation| CitationW {
                        order: citation.order,
                        source_range: citation.range.into(),
                        keys: citation
                            .keys
                            .iter()
                            .map(|key| CitationKeyW {
                                source_range: key.range.into(),
                                key: &key.value,
                            })
                            .collect(),
                        attributes: citation
                            .attributes
                            .iter()
                            .map(|attribute| CitationAttributeW {
                                source_range: attribute.range.into(),
                                name: attribute.name.as_deref(),
                                value: &attribute.value,
                            })
                            .collect(),
                    })
                    .collect(),
                ordered_lists: doc
                    .ordered_lists
                    .iter()
                    .map(|list| OrderedListW {
                        source_range: list.source_range.into(),
                        start: list.start,
                        reversed: list.reversed,
                        style: list.style_name(),
                    })
                    .collect(),
                block_presentations: doc
                    .block_presentations
                    .iter()
                    .map(|block| BlockPresentationW {
                        kind: block.kind.as_str(),
                        source_range: block.source_range.into(),
                        content_range: block.content_range.into(),
                        title: block.title.as_deref(),
                        attribution: block.attribution.as_deref(),
                        citation: block.citation.as_deref(),
                    })
                    .collect(),
                structure: StructureW {
                    headings: doc
                        .structure
                        .headings()
                        .iter()
                        .map(|heading| {
                            let presentation = doc
                                .presentation
                                .heading_at(heading.range)
                                .expect("every projected heading has presentation facts");
                            HeadingW {
                                kind: structure_kind(heading.kind),
                                level: heading.level,
                                id: &heading.id,
                                id_range: heading.id_range.into(),
                                title: &heading.title,
                                range: heading.range.into(),
                                title_range: heading.title_range.into(),
                                number: &presentation.number,
                                toc_included: presentation.toc_included,
                            }
                        })
                        .collect(),
                    toc: doc.presentation.toc().iter().map(TocW::from).collect(),
                    manpage: doc.structure.manpage().map(|manpage| ManpageW {
                        name: &manpage.name,
                        section: &manpage.section,
                        purpose: &manpage.purpose,
                        title_range: manpage.title_range.into(),
                        name_range: manpage.name_range.into(),
                        purpose_range: manpage.purpose_range.into(),
                    }),
                },
                catalogs: CatalogsW {
                    footnotes: doc
                        .catalogs
                        .footnotes()
                        .iter()
                        .map(|footnote| FootnoteW {
                            number: footnote.number,
                            id: footnote.id.as_deref(),
                            definition_range: footnote.definition_range.into(),
                            content_range: footnote.content_range.into(),
                            text: &footnote.text,
                            occurrences: footnote
                                .occurrences
                                .iter()
                                .map(|occurrence| occurrence.range.into())
                                .collect(),
                        })
                        .collect(),
                    bibliography: doc
                        .catalogs
                        .bibliography()
                        .iter()
                        .map(|entry| BibliographyW {
                            id: &entry.id,
                            label: entry.label.as_deref(),
                            definition_range: entry.definition_range.into(),
                            references: entry
                                .references
                                .iter()
                                .map(|reference| reference.range.into())
                                .collect(),
                        })
                        .collect(),
                    index: doc
                        .catalogs
                        .index()
                        .iter()
                        .map(|entry| IndexW {
                            terms: &entry.terms,
                            display: &entry.display,
                            occurrences: entry
                                .occurrences
                                .iter()
                                .map(|range| RangeW::from(*range))
                                .collect(),
                        })
                        .collect(),
                },
                searchable_text: SearchableTextW {
                    text: &doc.searchable_text.text,
                    segments: doc
                        .searchable_text
                        .segments
                        .iter()
                        .map(|segment| SegmentW {
                            kind: segment.kind.as_str(),
                            source_range: segment.source_range.into(),
                            text: &segment.text,
                        })
                        .collect(),
                },
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::inline_model::MathLanguage;
    use crate::preprocessor::{
        PreprocessOptions, ResourceDocument, ResourceSnapshot, preprocess_and_analyze,
    };
    use crate::reference::ResolvedReference;
    use crate::{AnalysisOptions, Engine, SourceId};

    use super::*;
    use crate::core::AnalysisInputs;

    #[test]
    fn rendering_features_are_typed_unique_and_deterministically_sorted() {
        let source = include_str!("../../../fixtures/projection/rendering-features.adoc");
        let analysis = Engine::new(AnalysisOptions::default())
            .analyze(source)
            .expect("analysis");
        let mut projected = project(&analysis, &RenderInputs::default());
        assert_eq!(
            projected
                .formulas
                .iter()
                .map(|formula| formula.kind)
                .collect::<Vec<_>>(),
            [FormulaKind::Inline, FormulaKind::Inline, FormulaKind::Block]
        );
        let latex = projected.formulas[0].clone();
        let mut typst = latex.clone();
        typst.language = MathLanguage::Typst;
        projected.formulas.extend([typst, latex]);

        assert_eq!(
            projected.rendering_features(),
            RenderingFeatures {
                math_languages: vec!["latexmath".to_owned(), "typst".to_owned()],
                source_languages: vec![
                    "c--".to_owned(),
                    "javascript".to_owned(),
                    "rust".to_owned()
                ],
                table_of_contents: true,
            }
        );
        assert!(!projected.presentation.toc().is_empty());
    }

    #[test]
    fn rendering_features_reflect_preprocessed_includes() {
        let mut snapshot = ResourceSnapshot::default();
        snapshot.insert(
            "features.adoc",
            ResourceDocument {
                source_id: SourceId::new("included:features.adoc"),
                source: include_str!(
                    "../../../fixtures/projection/rendering-features-included.adoc"
                )
                .into(),
            },
        );
        let options = PreprocessOptions {
            enable_includes: true,
            ..PreprocessOptions::default()
        };
        let preprocessed = preprocess_and_analyze(
            &Engine::new(AnalysisOptions::default()),
            "include::features.adoc[]\n",
            &snapshot,
            &options,
        )
        .expect("preprocessed analysis");

        assert_eq!(
            project(&preprocessed.analysis, &RenderInputs::default()).rendering_features(),
            RenderingFeatures {
                math_languages: vec!["latexmath".to_owned()],
                source_languages: vec!["kotlin".to_owned()],
                table_of_contents: false,
            }
        );
    }

    #[test]
    fn rendering_features_are_empty_when_document_needs_no_optional_rendering() {
        let analysis = Engine::new(AnalysisOptions::default())
            .analyze("Plain paragraph.\n")
            .expect("analysis");

        assert_eq!(
            project(&analysis, &RenderInputs::default()).rendering_features(),
            RenderingFeatures::default()
        );

        let section_without_toc = Engine::new(AnalysisOptions::default())
            .analyze("= Title\n\n== Section\n")
            .expect("analysis");
        assert!(
            !project(&section_without_toc, &RenderInputs::default())
                .rendering_features()
                .table_of_contents
        );

        let toc_without_entries = Engine::new(AnalysisOptions::default())
            .analyze("= Title\n:toc:\n")
            .expect("analysis");
        assert!(
            !project(&toc_without_entries, &RenderInputs::default())
                .rendering_features()
                .table_of_contents
        );
    }

    #[test]
    fn projections_are_stable_and_keep_links_and_reference_kinds_distinct() {
        let source = "\
= Title

[[part]]
== Section

https://example.com[Site] <<part>> xref:other.adoc#x[] xref:note:42[]

[source,rust]
----
fn main() {}
----

stem:[x+y]
";
        let analysis = Engine::new(AnalysisOptions::default())
            .analyze_with(
                source,
                AnalysisInputs {
                    source_id: Some(&SourceId::new("host:document")),
                    ..AnalysisInputs::default()
                },
            )
            .expect("analysis");
        let projected = project(&analysis, &RenderInputs::default());
        let html = crate::html::render(analysis.document(), &crate::html::RenderPolicy::default());

        assert_eq!(projected.package_version, crate::VERSION);
        assert!(html.html.contains("<h1"));
        assert_eq!(projected.external_links.len(), 1);
        assert_eq!(projected.reference_edges.len(), 3);
        assert!(matches!(
            projected.reference_edges[0].target,
            ReferenceKey::Local { .. }
        ));
        assert!(matches!(
            projected.reference_edges[1].target,
            ReferenceKey::Document { .. }
        ));
        assert!(matches!(
            projected.reference_edges[2].target,
            ReferenceKey::Scheme { .. }
        ));
        assert!(projected.searchable_text.text.contains("fn main() {}"));
        assert!(!projected.searchable_text.text.contains("x+y"));
        assert_eq!(
            projected.render_json(),
            project(&analysis, &RenderInputs::default()).render_json()
        );
    }

    #[test]
    fn block_presentation_titles_use_resolved_inline_text() {
        let analysis = Engine::new(AnalysisOptions::default())
            .analyze(
                "\
= Title
:product: AdocWeave

.*Important* {product}
[NOTE]
====
body
====
",
            )
            .expect("analysis");
        let projection = project(&analysis, &RenderInputs::default());

        assert_eq!(
            projection.block_presentations[0].title.as_deref(),
            Some("Important AdocWeave")
        );
    }

    #[test]
    fn reference_graph_attaches_optional_resolution_by_exact_source_range() {
        let analysis = Engine::new(AnalysisOptions::default())
            .analyze("xref:other.adoc[Other]")
            .expect("analysis");
        let resolution =
            ResolvedReference::resolved(analysis.references()[0].range, "https://example/other")
                .with_display_text("Resolved document title");
        let projected = project(
            &analysis,
            &RenderInputs::default().with_references(vec![resolution]),
        );

        assert!(matches!(
            projected.reference_edges[0].resolution,
            Some(ResolutionOutcome::Resolved {
                ref href,
                ref display_text,
                ..
            }) if href == "https://example/other"
                && display_text.as_deref() == Some("Resolved document title")
        ));
        assert!(
            projected
                .render_json()
                .contains("\"displayText\":\"Resolved document title\"")
        );
    }

    #[test]
    fn formula_projection_preserves_inline_and_block_sources() {
        let analysis = Engine::new(AnalysisOptions::default())
            .analyze(
                "\
stem:[x + y]

[stem]
++++
a^2
++++
",
            )
            .expect("analysis");
        let projected = project(&analysis, &RenderInputs::default());

        assert_eq!(projected.formulas.len(), 2);
        assert_eq!(projected.formulas[0].kind, FormulaKind::Inline);
        assert_eq!(projected.formulas[0].display(), FormulaKind::Inline);
        assert_eq!(
            projected.formulas[0].language,
            crate::inline_model::MathLanguage::Latex
        );
        assert_eq!(
            projected.formulas[0].language.as_asciidoc_name(),
            "latexmath"
        );
        assert_eq!(projected.formulas[0].source, "x + y");
        assert_eq!(
            &analysis.source()[projected.formulas[0].content_range.start().to_usize()
                ..projected.formulas[0].content_range.end().to_usize()],
            projected.formulas[0].source
        );
        assert_eq!(projected.formulas[1].kind, FormulaKind::Block);
        assert_eq!(projected.formulas[1].display(), FormulaKind::Block);
        assert_eq!(
            projected.formulas[1].language,
            crate::inline_model::MathLanguage::Latex
        );
        assert_eq!(projected.formulas[1].source, "a^2\n");
        assert_eq!(
            &analysis.source()[projected.formulas[1].content_range.start().to_usize()
                ..projected.formulas[1].content_range.end().to_usize()],
            projected.formulas[1].source
        );
        let json = projected.render_json();
        assert!(json.contains("\"formulas\":["));
        // The wire enum remains `latex`; the AsciiDoc syntax name is `latexmath`.
        assert!(json.contains("\"language\":\"latex\""));
    }

    #[test]
    fn source_block_projection_separates_language_content_and_ranges() {
        let analysis = Engine::new(AnalysisOptions::default())
            .analyze(
                "\
.main.rs
[source,rust,linenums,start=7]
----
let x = 1;
----
",
            )
            .expect("analysis");
        let projected = project(&analysis, &RenderInputs::default());

        assert_eq!(projected.source_blocks.len(), 1);
        let source = &projected.source_blocks[0];
        assert_eq!(
            source.title.as_ref().map(|title| title.text.as_str()),
            Some("main.rs")
        );
        assert_eq!(source.language.as_deref(), Some("rust"));
        assert!(source.line_numbers);
        assert_eq!(source.start_line, Some(7));
        assert_eq!(source.source, "let x = 1;\n");
        assert!(source.language_range.is_some());
        assert!(source.source_range.start() <= source.content_range.start());
        assert!(source.content_range.end() <= source.source_range.end());
    }

    #[test]
    fn source_block_line_number_option_spellings_share_one_projection() {
        for attribute in [
            "[source,rust,linenums]",
            "[source,rust,%linenums]",
            "[source,rust,options=linenums]",
        ] {
            let analysis = Engine::new(AnalysisOptions::default())
                .analyze(&format!("{attribute}\n----\ncode\n----\n"))
                .expect("analysis");
            let projected = project(&analysis, &RenderInputs::default());
            let source = &projected.source_blocks[0];

            assert!(source.line_numbers, "{attribute}");
            assert_eq!(source.start_line, Some(1), "{attribute}");
        }
    }

    #[test]
    fn source_block_line_number_boundaries_and_duplicates_are_deterministic() {
        let cases = [
            ("[source,rust,start=8]", false, None),
            ("[source,rust,linenums,start=0]", true, Some(1)),
            ("[source,rust,linenums,start=4294967296]", true, Some(1)),
            ("[source,rust,linenums,start=7,start=9]", true, Some(7)),
            ("[source,rust,linenums,start=0,start=9]", true, Some(1)),
            ("[source,rust,start=8,%linenums]", true, Some(8)),
            ("[source,rust,start=8,options=linenums]", true, Some(8)),
            (
                "[source,rust,%linenums,options=linenums,start=8]",
                true,
                Some(8),
            ),
        ];

        for (attribute, line_numbers, start_line) in cases {
            let analysis = Engine::new(AnalysisOptions::default())
                .analyze(&format!("{attribute}\n----\ncode\n----\n"))
                .expect("analysis");
            let projected = project(&analysis, &RenderInputs::default());
            let source = &projected.source_blocks[0];

            assert_eq!(source.line_numbers, line_numbers, "{attribute}");
            assert_eq!(source.start_line, start_line, "{attribute}");
        }
    }

    #[test]
    fn ordered_list_projection_uses_lowered_presentation() {
        let analysis = Engine::new(AnalysisOptions::default())
            .analyze(
                "\
[start=4,%reversed,loweralpha]
. one
. two
",
            )
            .expect("analysis");
        let projected = project(&analysis, &RenderInputs::default());

        assert_eq!(projected.ordered_lists.len(), 1);
        assert_eq!(
            projected.ordered_lists[0],
            OrderedListProjection {
                source_range: analysis.ast().blocks()[0].range(),
                start: Some(4),
                reversed: true,
                style: crate::block_model::OrderedListStyle::LowerAlpha,
            }
        );
        assert!(
            projected
                .render_json()
                .contains("\"orderedLists\":[{\"sourceRange\":")
        );
    }

    #[test]
    fn duplicate_resolution_ranges_never_depend_on_input_order() {
        let analysis = Engine::new(AnalysisOptions::default())
            .analyze("xref:other.adoc[Other]")
            .expect("analysis");
        let range = analysis.references()[0].range;
        let first = ResolvedReference::resolved(range, "https://example/first");
        let second = ResolvedReference::resolved(range, "https://example/second");
        let forward = project(
            &analysis,
            &RenderInputs::default().with_references(vec![first.clone(), second.clone()]),
        );
        let reverse = project(
            &analysis,
            &RenderInputs::default().with_references(vec![second, first]),
        );

        assert_eq!(forward, reverse);
        assert!(forward.reference_edges[0].resolution.is_none());
    }

    #[test]
    fn citations_reach_the_projection_json_with_keys_attributes_and_order() {
        let source = "See cite:[smith2024, tanaka2025] and cite:[a, locator=\"p. 12\"].\n";
        let analysis = crate::Engine::new(crate::AnalysisOptions::default())
            .analyze(source)
            .expect("analysis");
        let json = project(&analysis, &RenderInputs::default()).render_json();

        // Keys keep their source order and their own ranges.
        assert!(json.contains(
            "\"citations\":[{\"order\":0,\"sourceRange\":{\"start\":4,\"end\":32},\"keys\":[\
             {\"sourceRange\":{\"start\":10,\"end\":19},\"key\":\"smith2024\"},\
             {\"sourceRange\":{\"start\":21,\"end\":31},\"key\":\"tanaka2025\"}],\"attributes\":[]}"
        ));
        // A named attribute is reported apart from the keys.
        assert!(json.contains("\"key\":\"a\"}],\"attributes\":[{\"sourceRange\":"));
        assert!(json.contains("\"name\":\"locator\",\"value\":\"p. 12\""));
        assert!(json.contains("\"order\":1,"));

        // The recorded ranges address the original source.
        let value = project(&analysis, &RenderInputs::default());
        for citation in &value.citations {
            for key in &citation.keys {
                assert_eq!(
                    &source[key.range.start().to_usize()..key.range.end().to_usize()],
                    key.value
                );
            }
        }
    }

    #[test]
    fn projections_keep_the_public_baseline_json_contract() {
        let analysis = Engine::new(AnalysisOptions::default())
            .analyze("= T")
            .expect("analysis");
        let rendered = project(&analysis, &RenderInputs::default()).render_json();
        assert_eq!(
            rendered.replacen(
                &format!("\"packageVersion\":\"{}\"", crate::VERSION),
                "\"packageVersion\":\"<package-version>\"",
                1,
            ),
            "{\"packageVersion\":\"<package-version>\",\"sourceId\":null,\"title\":{\"sourceRange\":{\"start\":2,\"end\":3},\"text\":\"T\"},\"targets\":[{\"kind\":\"document-title\",\"id\":\"_t\",\"label\":\"T\",\"idRange\":{\"start\":2,\"end\":3},\"targetRange\":{\"start\":0,\"end\":3}}],\"externalLinks\":[],\"referenceEdges\":[],\"sourceBlocks\":[],\"formulas\":[],\"citations\":[],\"orderedLists\":[],\"blockPresentations\":[],\"structure\":{\"headings\":[{\"kind\":\"document-title\",\"level\":0,\"id\":\"_t\",\"idRange\":{\"start\":2,\"end\":3},\"title\":\"T\",\"range\":{\"start\":0,\"end\":3},\"titleRange\":{\"start\":2,\"end\":3},\"number\":[],\"tocIncluded\":false}],\"toc\":[],\"manpage\":null},\"catalogs\":{\"footnotes\":[],\"bibliography\":[],\"index\":[]},\"searchableText\":{\"text\":\"T\",\"segments\":[{\"kind\":\"prose\",\"sourceRange\":{\"start\":2,\"end\":3},\"text\":\"T\"}]}}"
        );
    }

    #[test]
    fn bibliography_catalog_keeps_definition_and_all_reference_ranges() {
        let analysis = Engine::new(AnalysisOptions::default())
            .analyze("* bibanchor:ref[] Entry\n\nSee <<ref>> and <<ref,Entry>>.")
            .expect("analysis");
        let projection = project(&analysis, &RenderInputs::default());

        assert_eq!(projection.catalogs.bibliography().len(), 1);
        assert_eq!(projection.catalogs.bibliography()[0].references.len(), 2);
        assert!(
            projection
                .render_json()
                .contains("\"bibliography\":[{\"id\":\"ref\",\"label\":null,\"definitionRange\":")
        );
    }

    #[test]
    fn bibliography_catalog_keeps_the_anchor_display_text() {
        let analysis = Engine::new(AnalysisOptions::default())
            .analyze("* [[[smith2024,1]]] Smith, A. 2024.\n\nSee <<smith2024>>.")
            .expect("analysis");
        let projection = project(&analysis, &RenderInputs::default());

        let entry = &projection.catalogs.bibliography()[0];
        assert_eq!(entry.id, "smith2024");
        assert_eq!(entry.label.as_deref(), Some("1"));
        assert!(
            projection
                .render_json()
                .contains("\"bibliography\":[{\"id\":\"smith2024\",\"label\":\"1\",")
        );
    }

    #[test]
    fn searchable_text_excludes_attributes_math_and_invisible_anchor_syntax() {
        let source = "\
= Visible
:name: hidden

[[secret]]
== Section

stem:[hidden-math]

....
visible code
....
";
        let analysis = Engine::new(AnalysisOptions::default())
            .analyze(source)
            .expect("analysis");
        let searchable = searchable_text(&analysis);

        assert_eq!(searchable.text, "Visible\nSection\nvisible code");
        assert_eq!(
            searchable
                .segments
                .iter()
                .map(|segment| segment.kind)
                .collect::<Vec<_>>(),
            vec![
                SearchTextKind::Prose,
                SearchTextKind::Prose,
                SearchTextKind::Code
            ]
        );
    }
}
