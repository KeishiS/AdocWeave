//! Deterministic, host-independent projections derived from one [`Analysis`].

use std::fmt::Write as _;

use crate::core::{Analysis, SourceId};
use crate::document::{ReferenceTarget, ReferenceTargetKind};
use crate::inline::{Inline, Link};
use crate::parser::{AstBlock, ListBlock};
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
    pub searchable_text: SearchableText,
    pub catalogs: crate::catalog::DocumentCatalogs,
    pub structure: crate::structure::DocumentStructure,
    pub presentation: crate::presentation::DocumentPresentation,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SourceBlockProjection {
    pub source_range: TextRange,
    pub content_range: TextRange,
    pub language_range: Option<TextRange>,
    pub language: Option<String>,
    pub source: String,
}

/// Presentation facts for an ordered list, resolved once during lowering.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct OrderedListProjection {
    pub source_range: TextRange,
    pub start: Option<u32>,
    pub reversed: bool,
    pub style: crate::parser::OrderedListStyle,
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
            crate::parser::OrderedListStyle::Arabic => "arabic",
            crate::parser::OrderedListStyle::Decimal => "decimal",
            crate::parser::OrderedListStyle::LowerAlpha => "loweralpha",
            crate::parser::OrderedListStyle::UpperAlpha => "upperalpha",
            crate::parser::OrderedListStyle::LowerRoman => "lowerroman",
            crate::parser::OrderedListStyle::UpperRoman => "upperroman",
            crate::parser::OrderedListStyle::LowerGreek => "lowergreek",
        }
    }
}

impl FormulaKind {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Inline => "inline",
            Self::Block => "block",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FormulaProjection {
    pub kind: FormulaKind,
    pub language: crate::inline::MathLanguage,
    pub source_range: TextRange,
    pub content_range: TextRange,
    pub source: String,
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
                if matches!(heading.kind, crate::parser::HeadingKind::DocumentTitle) =>
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
        .into_iter()
        .filter_map(|reference| {
            let target = ReferenceKey::from_destination(&reference.destination)?;
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
                language_range: source.language_range,
                language: source.language.clone(),
                source: source.value.clone(),
            });
        }
        crate::walker::SemanticNode::Block(AstBlock::Verbatim(block))
            if matches!(block.kind, crate::parser::VerbatimKind::Source(_)) =>
        {
            let crate::parser::VerbatimKind::Source(source) = &block.kind else {
                unreachable!("match guard ensures source verbatim block")
            };
            source_blocks.push(SourceBlockProjection {
                source_range: block.range,
                content_range: block.content_range,
                language_range: source.language_range,
                language: source.language.clone(),
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
            if list.kind == crate::parser::ListKind::Ordered =>
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
                    crate::parser::DelimitedPresentation::Admonition(_) => block_presentations
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
                    crate::parser::DelimitedPresentation::Quote(quote) => {
                        block_presentations.push(BlockPresentationProjection {
                            kind: match quote.kind {
                                crate::parser::QuoteKind::Quote => BlockPresentationKind::Quote,
                                crate::parser::QuoteKind::Verse => BlockPresentationKind::Verse,
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
                        })
                    }
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

fn collect_search_blocks(blocks: &[AstBlock], output: &mut Vec<SearchTextSegment>) {
    for block in blocks {
        match block {
            AstBlock::Heading(heading) => push_search(
                output,
                SearchTextKind::Prose,
                heading.text_range,
                inline_text(&heading.inlines),
            ),
            AstBlock::Paragraph(paragraph) => {
                push_search(
                    output,
                    SearchTextKind::Prose,
                    paragraph.content_range,
                    fold_line_endings(&inline_text(&paragraph.inlines)),
                );
            }
            AstBlock::LiteralParagraph(literal) => push_search(
                output,
                SearchTextKind::Code,
                literal.content_range,
                literal.value.clone(),
            ),
            AstBlock::Break(_) => {}
            AstBlock::Source(source) => push_search(
                output,
                SearchTextKind::Code,
                source.content_range,
                source.value.clone(),
            ),
            AstBlock::Verbatim(source) => push_search(
                output,
                SearchTextKind::Code,
                source.content_range,
                source.value.clone(),
            ),
            AstBlock::List(list) => collect_search_list(list, output),
            AstBlock::Delimited(block) => match &block.content {
                crate::parser::DelimitedContent::Compound(children) => {
                    collect_search_blocks(children, output);
                }
                crate::parser::DelimitedContent::Verbatim(value)
                    if !matches!(block.kind, crate::parser::DelimitedBlockKind::Comment) =>
                {
                    push_search(
                        output,
                        SearchTextKind::Code,
                        block.content_range,
                        value.clone(),
                    );
                }
                crate::parser::DelimitedContent::Verbatim(_)
                | crate::parser::DelimitedContent::Passthrough(_) => {}
                crate::parser::DelimitedContent::Table(table) => {
                    for row in &table.rows {
                        for cell in &row.cells {
                            match &cell.content {
                                crate::table::TableCellContent::Inlines(inlines) => push_search(
                                    output,
                                    SearchTextKind::Prose,
                                    cell.content_range,
                                    inline_text(inlines),
                                ),
                                crate::table::TableCellContent::AsciiDoc(blocks) => {
                                    collect_search_blocks(blocks, output)
                                }
                                crate::table::TableCellContent::Verbatim(value) => push_search(
                                    output,
                                    SearchTextKind::Code,
                                    cell.content_range,
                                    value.clone(),
                                ),
                            }
                        }
                    }
                }
            },
            AstBlock::Math(_) | AstBlock::Unsupported(_) => {}
        }
    }
}

fn collect_search_list(list: &ListBlock, output: &mut Vec<SearchTextSegment>) {
    for item in &list.items {
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
        for child in &item.children {
            collect_search_list(child, output);
        }
        collect_search_blocks(&item.continuations, output);
    }
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

fn resolved_inline_text(inlines: &[Inline]) -> String {
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
                    output.push_str(value.as_deref().unwrap_or_default());
                }
            }
            Inline::Formula(_) => {}
            Inline::Macro(node) => {
                use crate::inline::StandardMacroKind as Kind;
                match node.kind {
                    Kind::Anchor | Kind::BibliographyAnchor | Kind::IndexTerm => {}
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

fn fold_line_endings(value: &str) -> String {
    value
        .lines()
        .map(|line| line.trim_end_matches([' ', '\t']))
        .collect::<Vec<_>>()
        .join(" ")
}

impl DocumentProjection {
    /// Stable JSON without relying on a host serialization framework.
    pub fn render_json(&self) -> String {
        let mut output = String::new();
        write!(
            output,
            "{{\"packageVersion\":\"{}\",\"sourceId\":",
            self.package_version
        )
        .expect("writing to String cannot fail");
        write_optional_string(&mut output, self.source_id.as_ref().map(SourceId::as_str));
        output.push_str(",\"title\":");
        match &self.title {
            Some(title) => write_projected_text(&mut output, title),
            None => output.push_str("null"),
        }
        output.push_str(",\"targets\":[");
        for (index, target) in self.targets.iter().enumerate() {
            if index > 0 {
                output.push(',');
            }
            write!(
                output,
                "{{\"kind\":\"{}\",\"id\":{},\"label\":{},\"idRange\":{},\"targetRange\":{}}}",
                reference_target_kind(target.kind),
                json_string(&target.id),
                json_string(&target.label),
                json_range(target.id_range),
                json_range(target.target_range)
            )
            .expect("writing to String cannot fail");
        }
        output.push_str("],\"externalLinks\":[");
        for (index, link) in self.external_links.iter().enumerate() {
            if index > 0 {
                output.push(',');
            }
            write!(
                output,
                "{{\"sourceRange\":{},\"targetRange\":{},\"target\":{},\"label\":{}}}",
                json_range(link.source_range),
                json_range(link.target_range),
                json_string(&link.target),
                json_string(&link.label)
            )
            .expect("writing to String cannot fail");
        }
        output.push_str("],\"referenceEdges\":[");
        for (index, edge) in self.reference_edges.iter().enumerate() {
            if index > 0 {
                output.push(',');
            }
            write_reference_edge(&mut output, edge);
        }
        output.push_str("],\"sourceBlocks\":[");
        for (index, source) in self.source_blocks.iter().enumerate() {
            if index > 0 {
                output.push(',');
            }
            write!(
                output,
                "{{\"sourceRange\":{},\"contentRange\":{},\"languageRange\":{},\"language\":{},\"source\":{}}}",
                json_range(source.source_range),
                json_range(source.content_range),
                source
                    .language_range
                    .map_or_else(|| "null".to_owned(), json_range),
                source
                    .language
                    .as_deref()
                    .map_or_else(|| "null".to_owned(), json_string),
                json_string(&source.source),
            )
            .expect("writing to String cannot fail");
        }
        output.push_str("],\"formulas\":[");
        for (index, formula) in self.formulas.iter().enumerate() {
            if index > 0 {
                output.push(',');
            }
            write!(
                output,
                "{{\"kind\":\"{}\",\"language\":\"{}\",\"sourceRange\":{},\"contentRange\":{},\"source\":{}}}",
                formula.kind.as_str(),
                math_language(formula.language),
                json_range(formula.source_range),
                json_range(formula.content_range),
                json_string(&formula.source),
            )
            .expect("writing to String cannot fail");
        }
        output.push_str("],\"orderedLists\":[");
        for (index, list) in self.ordered_lists.iter().enumerate() {
            if index > 0 {
                output.push(',');
            }
            write!(
                output,
                "{{\"sourceRange\":{},\"start\":{},\"reversed\":{},\"style\":\"{}\"}}",
                json_range(list.source_range),
                list.start
                    .map_or_else(|| "null".to_owned(), |value| value.to_string()),
                list.reversed,
                list.style_name(),
            )
            .expect("writing to String cannot fail");
        }
        output.push_str("],\"blockPresentations\":[");
        for (index, block) in self.block_presentations.iter().enumerate() {
            if index > 0 {
                output.push(',');
            }
            write!(
                output,
                "{{\"kind\":\"{}\",\"sourceRange\":{},\"contentRange\":{},\"title\":{},\"attribution\":{},\"citation\":{}}}",
                block.kind.as_str(),
                json_range(block.source_range),
                json_range(block.content_range),
                block.title.as_deref().map_or_else(|| "null".to_owned(), json_string),
                block.attribution.as_deref().map_or_else(|| "null".to_owned(), json_string),
                block.citation.as_deref().map_or_else(|| "null".to_owned(), json_string),
            ).expect("writing to String cannot fail");
        }
        output.push_str("],\"structure\":");
        write_structure(&mut output, &self.structure, &self.presentation);
        output.push_str(",\"catalogs\":");
        write_catalogs(&mut output, &self.catalogs);
        output.push_str(",\"searchableText\":{\"text\":");
        output.push_str(&json_string(&self.searchable_text.text));
        output.push_str(",\"segments\":[");
        for (index, segment) in self.searchable_text.segments.iter().enumerate() {
            if index > 0 {
                output.push(',');
            }
            write!(
                output,
                "{{\"kind\":\"{}\",\"sourceRange\":{},\"text\":{}}}",
                segment.kind.as_str(),
                json_range(segment.source_range),
                json_string(&segment.text)
            )
            .expect("writing to String cannot fail");
        }
        output.push_str("]}}");
        output
    }
}

const fn math_language(language: crate::inline::MathLanguage) -> &'static str {
    match language {
        crate::inline::MathLanguage::Latex => "latex",
        crate::inline::MathLanguage::Typst => "typst",
    }
}

fn write_structure(
    output: &mut String,
    structure: &crate::structure::DocumentStructure,
    presentation: &crate::presentation::DocumentPresentation,
) {
    output.push_str("{\"headings\":[");
    for (index, heading) in structure.headings().iter().enumerate() {
        if index > 0 {
            output.push(',');
        }
        write!(
            output,
            "{{\"kind\":\"{}\",\"level\":{},\"id\":{},\"idRange\":{},\"title\":{},\"range\":{},\"titleRange\":{},\"number\":[",
            structure_kind(heading.kind),
            heading.level,
            json_string(&heading.id),
            json_range(heading.id_range),
            json_string(&heading.title),
            json_range(heading.range),
            json_range(heading.title_range),
        )
        .expect("writing to String cannot fail");
        let presentation = presentation
            .heading_at(heading.range)
            .expect("every projected heading has presentation facts");
        write_numbers(output, &presentation.number);
        write!(output, "],\"tocIncluded\":{}}}", presentation.toc_included)
            .expect("writing to String cannot fail");
    }
    output.push_str("],\"toc\":");
    write_toc(output, presentation.toc());
    output.push_str(",\"manpage\":");
    if let Some(manpage) = structure.manpage() {
        write!(
            output,
            "{{\"name\":{},\"section\":{},\"purpose\":{},\"titleRange\":{},\"nameRange\":{},\"purposeRange\":{}}}",
            json_string(&manpage.name),
            json_string(&manpage.section),
            json_string(&manpage.purpose),
            json_range(manpage.title_range),
            json_range(manpage.name_range),
            json_range(manpage.purpose_range),
        )
        .expect("writing to String cannot fail");
    } else {
        output.push_str("null");
    }
    output.push('}');
}

fn write_toc(output: &mut String, entries: &[crate::structure::TocEntry]) {
    output.push('[');
    for (index, entry) in entries.iter().enumerate() {
        if index > 0 {
            output.push(',');
        }
        write!(
            output,
            "{{\"id\":{},\"title\":{},\"level\":{},\"number\":[",
            json_string(&entry.id),
            json_string(&entry.title),
            entry.level,
        )
        .expect("writing to String cannot fail");
        write_numbers(output, &entry.number);
        write!(
            output,
            "],\"range\":{},\"children\":",
            json_range(entry.range)
        )
        .expect("writing to String cannot fail");
        write_toc(output, &entry.children);
        output.push('}');
    }
    output.push(']');
}

fn write_numbers(output: &mut String, numbers: &[u32]) {
    for (index, number) in numbers.iter().enumerate() {
        if index > 0 {
            output.push(',');
        }
        write!(output, "{number}").expect("writing to String cannot fail");
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

fn write_catalogs(output: &mut String, catalogs: &crate::catalog::DocumentCatalogs) {
    output.push_str("{\"footnotes\":[");
    for (index, footnote) in catalogs.footnotes().iter().enumerate() {
        if index > 0 {
            output.push(',');
        }
        write!(
            output,
            "{{\"number\":{},\"id\":{},\"definitionRange\":{},\"contentRange\":{},\"text\":{},\"occurrences\":[",
            footnote.number,
            footnote.id.as_ref().map_or_else(|| "null".to_owned(), |id| json_string(id)),
            json_range(footnote.definition_range),
            json_range(footnote.content_range),
            json_string(&footnote.text),
        )
        .expect("writing to String cannot fail");
        for (occurrence_index, occurrence) in footnote.occurrences.iter().enumerate() {
            if occurrence_index > 0 {
                output.push(',');
            }
            output.push_str(&json_range(occurrence.range));
        }
        output.push_str("]}");
    }
    output.push_str("],\"bibliography\":[");
    for (index, entry) in catalogs.bibliography().iter().enumerate() {
        if index > 0 {
            output.push(',');
        }
        write!(
            output,
            "{{\"id\":{},\"definitionRange\":{},\"references\":[",
            json_string(&entry.id),
            json_range(entry.definition_range),
        )
        .expect("writing to String cannot fail");
        for (reference_index, reference) in entry.references.iter().enumerate() {
            if reference_index > 0 {
                output.push(',');
            }
            output.push_str(&json_range(reference.range));
        }
        output.push_str("]}");
    }
    output.push_str("],\"index\":[");
    for (index, entry) in catalogs.index().iter().enumerate() {
        if index > 0 {
            output.push(',');
        }
        output.push_str("{\"terms\":[");
        for (term_index, term) in entry.terms.iter().enumerate() {
            if term_index > 0 {
                output.push(',');
            }
            output.push_str(&json_string(term));
        }
        write!(
            output,
            "],\"display\":{},\"occurrences\":[",
            json_string(&entry.display)
        )
        .expect("writing to String cannot fail");
        for (occurrence_index, range) in entry.occurrences.iter().enumerate() {
            if occurrence_index > 0 {
                output.push(',');
            }
            output.push_str(&json_range(*range));
        }
        output.push_str("]}");
    }
    output.push_str("]}");
}

fn write_projected_text(output: &mut String, text: &ProjectedText) {
    write!(
        output,
        "{{\"sourceRange\":{},\"text\":{}}}",
        json_range(text.source_range),
        json_string(&text.text)
    )
    .expect("writing to String cannot fail");
}

fn write_reference_edge(output: &mut String, edge: &ReferenceEdge) {
    output.push_str("{\"sourceId\":");
    write_optional_string(output, edge.source_id.as_ref().map(SourceId::as_str));
    write!(
        output,
        ",\"sourceRange\":{},\"target\":{}",
        json_range(edge.source_range),
        reference_key_json(&edge.target)
    )
    .expect("writing to String cannot fail");
    output.push_str(",\"resolution\":");
    match &edge.resolution {
        Some(ResolutionOutcome::Resolved {
            href,
            display_text,
            notices,
        }) => {
            write!(
                output,
                "{{\"status\":\"resolved\",\"href\":{},\"displayText\":{},\"notices\":[",
                json_string(href),
                display_text
                    .as_ref()
                    .map_or_else(|| "null".to_owned(), |text| json_string(text))
            )
            .expect("writing to String cannot fail");
            for (index, notice) in notices.iter().enumerate() {
                if index > 0 {
                    output.push(',');
                }
                output.push_str(&json_string(notice.kind.diagnostic_code()));
            }
            output.push_str("]}");
        }
        Some(ResolutionOutcome::Failed(failure)) => {
            write!(
                output,
                "{{\"status\":\"failed\",\"kind\":\"{}\"}}",
                failure.kind.diagnostic_code()
            )
            .expect("writing to String cannot fail");
        }
        None => output.push_str("null"),
    }
    output.push('}');
}

fn reference_key_json(key: &ReferenceKey) -> String {
    match key {
        ReferenceKey::Local { anchor } => {
            format!("{{\"kind\":\"local\",\"anchor\":{}}}", json_string(anchor))
        }
        ReferenceKey::Document { document, anchor } => format!(
            "{{\"kind\":\"document\",\"document\":{},\"anchor\":{}}}",
            json_string(document),
            optional_string_json(anchor.as_deref())
        ),
        ReferenceKey::Scheme {
            scheme,
            locator,
            anchor,
        } => format!(
            "{{\"kind\":\"scheme\",\"scheme\":{},\"locator\":{},\"anchor\":{}}}",
            json_string(scheme),
            json_string(locator),
            optional_string_json(anchor.as_deref())
        ),
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

fn write_optional_string(output: &mut String, value: Option<&str>) {
    output.push_str(&optional_string_json(value));
}

fn optional_string_json(value: Option<&str>) -> String {
    value.map_or_else(|| "null".to_owned(), json_string)
}

fn json_range(range: TextRange) -> String {
    format!(
        "{{\"start\":{},\"end\":{}}}",
        range.start().to_u32(),
        range.end().to_u32()
    )
}

fn json_string(value: &str) -> String {
    crate::json::string(value)
}

#[cfg(test)]
mod tests {
    use crate::reference::ResolvedReference;
    use crate::{Engine, ParseOptions, SourceId};

    use super::*;

    #[test]
    fn projections_are_stable_and_keep_links_and_reference_kinds_distinct() {
        let source = "= Title\n\n[[part]]\n== Section\n\nhttps://example.com[Site] <<part>> xref:other.adoc#x[] xref:note:42[]\n\n[source,rust]\n----\nfn main() {}\n----\n\nstem:[x+y]\n";
        let analysis = Engine::new(ParseOptions {
            source_id: Some(SourceId::new("host:document")),
            ..ParseOptions::default()
        })
        .analyze(source)
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
        let analysis = Engine::new(ParseOptions::default())
            .analyze("= Title\n:product: AdocWeave\n\n.*Important* {product}\n[NOTE]\n====\nbody\n====\n")
            .expect("analysis");
        let projection = project(&analysis, &RenderInputs::default());

        assert_eq!(
            projection.block_presentations[0].title.as_deref(),
            Some("Important AdocWeave")
        );
    }

    #[test]
    fn reference_graph_attaches_optional_resolution_by_exact_source_range() {
        let analysis = Engine::new(ParseOptions::default())
            .analyze("xref:other.adoc[Other]")
            .expect("analysis");
        let resolution =
            ResolvedReference::resolved(analysis.references()[0].range, "https://example/other")
                .with_display_text("Resolved document title");
        let projected = project(&analysis, &RenderInputs::new(vec![resolution], Vec::new()));

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
        let analysis = Engine::new(ParseOptions::default())
            .analyze("stem:[x + y]\n\n[stem]\n++++\na^2\n++++\n")
            .expect("analysis");
        let projected = project(&analysis, &RenderInputs::default());

        assert_eq!(projected.formulas.len(), 2);
        assert_eq!(projected.formulas[0].kind, FormulaKind::Inline);
        assert_eq!(projected.formulas[0].source, "x + y");
        assert_eq!(projected.formulas[1].kind, FormulaKind::Block);
        assert_eq!(projected.formulas[1].source, "a^2\n");
        let json = projected.render_json();
        assert!(json.contains("\"formulas\":["));
        assert!(json.contains("\"language\":\"latex\""));
    }

    #[test]
    fn source_block_projection_separates_language_content_and_ranges() {
        let analysis = Engine::new(ParseOptions::default())
            .analyze("[source,rust]\n----\nlet x = 1;\n----\n")
            .expect("analysis");
        let projected = project(&analysis, &RenderInputs::default());

        assert_eq!(projected.source_blocks.len(), 1);
        let source = &projected.source_blocks[0];
        assert_eq!(source.language.as_deref(), Some("rust"));
        assert_eq!(source.source, "let x = 1;\n");
        assert!(source.language_range.is_some());
        assert!(source.source_range.start() <= source.content_range.start());
        assert!(source.content_range.end() <= source.source_range.end());
    }

    #[test]
    fn ordered_list_projection_uses_lowered_presentation() {
        let analysis = Engine::new(ParseOptions::default())
            .analyze("[start=4,%reversed,loweralpha]\n. one\n. two\n")
            .expect("analysis");
        let projected = project(&analysis, &RenderInputs::default());

        assert_eq!(projected.ordered_lists.len(), 1);
        assert_eq!(
            projected.ordered_lists[0],
            OrderedListProjection {
                source_range: analysis.ast().blocks()[0].range(),
                start: Some(4),
                reversed: true,
                style: crate::parser::OrderedListStyle::LowerAlpha,
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
        let analysis = Engine::new(ParseOptions::default())
            .analyze("xref:other.adoc[Other]")
            .expect("analysis");
        let range = analysis.references()[0].range;
        let first = ResolvedReference::resolved(range, "https://example/first");
        let second = ResolvedReference::resolved(range, "https://example/second");
        let forward = project(
            &analysis,
            &RenderInputs::new(vec![first.clone(), second.clone()], Vec::new()),
        );
        let reverse = project(
            &analysis,
            &RenderInputs::new(vec![second, first], Vec::new()),
        );

        assert_eq!(forward, reverse);
        assert!(forward.reference_edges[0].resolution.is_none());
    }

    #[test]
    fn projections_keep_the_public_baseline_json_contract() {
        let analysis = Engine::new(ParseOptions::default())
            .analyze("= T")
            .expect("analysis");
        assert_eq!(
            project(&analysis, &RenderInputs::default()).render_json(),
            "{\"packageVersion\":\"0.6.0\",\"sourceId\":null,\"title\":{\"sourceRange\":{\"start\":2,\"end\":3},\"text\":\"T\"},\"targets\":[{\"kind\":\"document-title\",\"id\":\"_t\",\"label\":\"T\",\"idRange\":{\"start\":2,\"end\":3},\"targetRange\":{\"start\":0,\"end\":3}}],\"externalLinks\":[],\"referenceEdges\":[],\"sourceBlocks\":[],\"formulas\":[],\"orderedLists\":[],\"blockPresentations\":[],\"structure\":{\"headings\":[{\"kind\":\"document-title\",\"level\":0,\"id\":\"_t\",\"idRange\":{\"start\":2,\"end\":3},\"title\":\"T\",\"range\":{\"start\":0,\"end\":3},\"titleRange\":{\"start\":2,\"end\":3},\"number\":[],\"tocIncluded\":false}],\"toc\":[],\"manpage\":null},\"catalogs\":{\"footnotes\":[],\"bibliography\":[],\"index\":[]},\"searchableText\":{\"text\":\"T\",\"segments\":[{\"kind\":\"prose\",\"sourceRange\":{\"start\":2,\"end\":3},\"text\":\"T\"}]}}"
        );
    }

    #[test]
    fn bibliography_catalog_keeps_definition_and_all_reference_ranges() {
        let analysis = Engine::new(ParseOptions::default())
            .analyze("* bibanchor:ref[] Entry\n\nSee <<ref>> and <<ref,Entry>>.")
            .expect("analysis");
        let projection = project(&analysis, &RenderInputs::default());

        assert_eq!(projection.catalogs.bibliography().len(), 1);
        assert_eq!(projection.catalogs.bibliography()[0].references.len(), 2);
        assert!(
            projection
                .render_json()
                .contains("\"bibliography\":[{\"id\":\"ref\",\"definitionRange\":")
        );
    }

    #[test]
    fn searchable_text_excludes_attributes_math_and_invisible_anchor_syntax() {
        let source = "= Visible\n:name: hidden\n\n[[secret]]\n== Section\n\nstem:[hidden-math]\n\n....\nvisible code\n....\n";
        let analysis = Engine::new(ParseOptions::default())
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
