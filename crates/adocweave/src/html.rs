//! HTML output backend.
//!
//! This module depends on the output-neutral semantic AST. The parser and AST
//! do not depend on this module, so additional output backends can consume the
//! same document without changing parsing behavior.

mod body;
mod generated_bibliography;
mod head;
mod plan;
mod safe;

use std::collections::{BTreeMap, BTreeSet};

use crate::diagnostic::{Diagnostic, DiagnosticCode, DiagnosticId, Severity};
use crate::document::HeadingId;
use crate::inline::Inline;
use crate::parser::{AstBlock, AstDocument, Heading, HeadingKind, Paragraph, Unsupported};
use crate::render::{RenderInputProblemKind, RenderInputUsage, RenderInputs};
use crate::url::{ActiveUrlPolicy, UrlProvenance};
use body::{BlockWriter, RenderScope, classes, passive, source_language_class};

pub const ALLOWED_ELEMENTS: &[&str] = &[
    "a",
    "audio",
    "body",
    "blockquote",
    "br",
    "caption",
    "cite",
    "code",
    "dd",
    "div",
    "dl",
    "dt",
    "em",
    "figcaption",
    "figure",
    "h1",
    "h2",
    "h3",
    "h4",
    "h5",
    "head",
    "hr",
    "html",
    "img",
    "kbd",
    "li",
    "link",
    "mark",
    "meta",
    "ol",
    "p",
    "pre",
    "span",
    "strong",
    "style",
    "sub",
    "sup",
    "table",
    "tbody",
    "td",
    "tfoot",
    "th",
    "thead",
    "title",
    "tr",
    "ul",
    "video",
];
pub const ALLOWED_ATTRIBUTES: &[&str] = &[
    "alt",
    "charset",
    "class",
    "colspan",
    "controls",
    "data-language",
    "data-line-numbers",
    "data-line-start",
    "data-math-display",
    "data-math-language",
    "height",
    "href",
    "id",
    "lang",
    "poster",
    "rel",
    "rowspan",
    "src",
    "target",
    "title",
    "width",
];
pub const ALLOWED_CLASSES: &[&str] = &[
    "author",
    "admonition",
    "admonition-caution",
    "admonition-important",
    "admonition-note",
    "admonition-tip",
    "admonition-warning",
    "attribution",
    "appendix",
    "bibliography-anchor",
    "bibliography-backref",
    "button",
    "callout-list",
    "callout-number",
    "checklist-marker",
    "citation",
    "document-title",
    "footnote",
    "footnote-backref",
    "footnote-ref",
    "footnotes",
    "index-term",
    "language-*",
    "lead",
    "math-latex",
    "math-typst",
    "menu",
    "page-break",
    "revision",
    "quote",
    "source-block",
    "table-align-center",
    "table-align-left",
    "table-align-right",
    "table-valign-bottom",
    "table-valign-middle",
    "table-valign-top",
    "table-frame-all",
    "table-frame-ends",
    "table-frame-none",
    "table-frame-sides",
    "table-grid-all",
    "table-grid-cols",
    "table-grid-none",
    "table-grid-rows",
    "table-stripes-all",
    "table-stripes-even",
    "table-stripes-hover",
    "table-stripes-none",
    "table-stripes-odd",
    "toc",
    "title",
    "verse",
];

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HtmlDocumentMode {
    Fragment,
    Complete,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum ExternalLinkPresentation {
    #[default]
    SameContext,
    NewContext {
        noreferrer: bool,
    },
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum UnknownSourceLanguage {
    #[default]
    PreserveSanitized,
    OmitClass,
    Diagnostic,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct SourceLanguagePolicy {
    /// `None` accepts every safely normalized language. `Some` is an allowlist.
    pub allowed: Option<BTreeSet<String>>,
    pub unknown: UnknownSourceLanguage,
}

impl SourceLanguagePolicy {
    pub fn allows(&self, language: &str) -> bool {
        self.allowed.as_ref().is_none_or(|allowed| {
            allowed
                .iter()
                .any(|candidate| candidate.eq_ignore_ascii_case(language))
        })
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MathLanguagePolicy {
    /// An empty set disables every math language.
    pub allowed: BTreeSet<crate::inline::MathLanguage>,
}

impl Default for MathLanguagePolicy {
    fn default() -> Self {
        Self {
            allowed: [
                crate::inline::MathLanguage::Latex,
                crate::inline::MathLanguage::Typst,
            ]
            .into_iter()
            .collect(),
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum UnresolvedReferencePresentation {
    #[default]
    Target,
    LabelOnly,
    Hidden,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ResourceCapabilities {
    pub images: bool,
    pub media: bool,
}

impl Default for ResourceCapabilities {
    fn default() -> Self {
        Self {
            images: true,
            media: true,
        }
    }
}

/// A host-supplied stylesheet emitted into the complete document `<head>`.
/// Stylesheets are output configuration, never document input.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum StylesheetSource {
    /// CSS text emitted inside a `<style>` element.
    Inline(String),
    /// Stylesheet URL emitted as `<link rel="stylesheet">` after the
    /// [`ActiveUrlPolicy`] revalidates it in the resolved-resource context.
    External(String),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StylesheetPolicy {
    /// Stylesheet sources emitted in host order. Duplicate sources are
    /// emitted once; rejected sources are skipped with a diagnostic.
    pub sources: Vec<StylesheetSource>,
    /// Upper bound in bytes for each inline CSS body.
    pub max_inline_bytes: u32,
    /// Upper bound in bytes for each stylesheet URL.
    pub max_url_bytes: u32,
    /// Upper bound on the number of emitted stylesheet sources.
    pub max_sources: u32,
}

impl Default for StylesheetPolicy {
    fn default() -> Self {
        Self {
            sources: Vec::new(),
            max_inline_bytes: 1_048_576,
            max_url_bytes: 2_048,
            max_sources: 16,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RenderPolicy {
    pub document_mode: HtmlDocumentMode,
    pub render_document_title: bool,
    /// Enables the optional `kbd`, `btn`, and `menu` presentation macros.
    pub render_ui_macros: bool,
    pub active_urls: ActiveUrlPolicy,
    pub external_links: ExternalLinkPresentation,
    pub source_languages: SourceLanguagePolicy,
    pub math_languages: MathLanguagePolicy,
    pub unresolved_references: UnresolvedReferencePresentation,
    pub resources: ResourceCapabilities,
    pub stylesheets: StylesheetPolicy,
}

impl Default for RenderPolicy {
    fn default() -> Self {
        Self {
            document_mode: HtmlDocumentMode::Fragment,
            render_document_title: true,
            render_ui_macros: false,
            active_urls: ActiveUrlPolicy::default(),
            external_links: ExternalLinkPresentation::default(),
            source_languages: SourceLanguagePolicy::default(),
            math_languages: MathLanguagePolicy::default(),
            unresolved_references: UnresolvedReferencePresentation::default(),
            resources: ResourceCapabilities::default(),
            stylesheets: StylesheetPolicy::default(),
        }
    }
}

impl RenderPolicy {
    pub fn allows_url(&self, value: &str, context: UrlProvenance) -> bool {
        self.active_urls.allows(value, context)
    }

    pub fn classify_url(&self, value: &str, context: UrlProvenance) -> crate::url::UrlDecision {
        self.active_urls.classify(value, context)
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct HtmlOutput {
    pub package_version: &'static str,
    pub html: String,
    pub diagnostics: Vec<Diagnostic>,
    pub document_attributes: BTreeMap<String, String>,
    pub heading_ids: Vec<HeadingId>,
}

pub fn render(document: &crate::document::Document, policy: &RenderPolicy) -> HtmlOutput {
    render_with_inputs(document, policy, &RenderInputs::default())
}

pub use crate::reference::ResolvedReference;

pub fn render_with_inputs(
    document: &crate::document::Document,
    policy: &RenderPolicy,
    inputs: &RenderInputs,
) -> HtmlOutput {
    render_with_inputs_ast(document.inner(), policy, inputs)
}

pub(crate) fn render_with_inputs_ast(
    document: &AstDocument,
    policy: &RenderPolicy,
    inputs: &RenderInputs,
) -> HtmlOutput {
    let mut fragment = String::new();
    let document_attributes = document
        .attribute_environment()
        .values_at(document.header().end);
    let heading_ids = crate::document::generate_heading_ids_ast(document);
    let mut diagnostics = Vec::new();
    let generated_bibliography = generated_bibliography::prepare(
        inputs.generated_bibliography(),
        document,
        &mut diagnostics,
    );
    let mut input_usage = inputs.track_usage();
    {
        let mut inline_context = InlineRenderContext {
            policy,
            input_usage: &mut input_usage,
            diagnostics: &mut diagnostics,
            catalogs: document.catalogs(),
            identifiers: document.identifiers(),
            structure: document.structure(),
            presentation: document.presentation(),
            generated_bibliography: generated_bibliography.as_ref(),
        };
        let body_plan = body::plan_body_traversal(document, policy);
        serialize_body_traversal(
            &mut fragment,
            document,
            &body_plan,
            policy,
            &mut inline_context,
        );
        if let Some(bibliography) = &generated_bibliography {
            generated_bibliography::render(&mut fragment, bibliography);
        }
    }
    for problem in input_usage.finish() {
        let domain = problem.domain.as_str();
        let (code, message) = match problem.kind {
            RenderInputProblemKind::Duplicate => (
                "duplicate-render-input",
                format!("multiple {domain} resolutions have the same source range"),
            ),
            RenderInputProblemKind::Unused => (
                "unused-render-input",
                format!("{domain} resolution does not match a renderable {domain}"),
            ),
        };
        diagnostics.push(render_input_diagnostic(
            code,
            domain,
            &message,
            problem.range,
        ));
    }
    let document_head = head::plan_document_head(document, policy, &mut diagnostics);
    crate::diagnostic::sort_diagnostics(&mut diagnostics);

    let html = match document_head {
        Some(document_head) => {
            let head = head::serialize_document_head(&document_head);
            let mut html = String::from("<!doctype html>\n");
            BlockWriter::start(&mut html, "html", &[passive("lang", "")]);
            BlockWriter::line_break(&mut html);
            html.push_str(&head);
            BlockWriter::start(&mut html, "body", &[]);
            BlockWriter::line_break(&mut html);
            html.push_str(&fragment);
            BlockWriter::end(&mut html, "body");
            BlockWriter::line_break(&mut html);
            BlockWriter::end(&mut html, "html");
            BlockWriter::line_break(&mut html);
            html
        }
        None => fragment,
    };

    HtmlOutput {
        package_version: crate::VERSION,
        html,
        diagnostics,
        document_attributes,
        heading_ids,
    }
}

fn serialize_body_traversal(
    output: &mut String,
    document: &AstDocument,
    plan: &body::BodyTraversalPlan<'_>,
    policy: &RenderPolicy,
    context: &mut InlineRenderContext<'_, '_>,
) {
    for step in &plan.steps {
        match step {
            body::BodyTraversalStep::TableOfContents => {
                render_toc(output, document.presentation());
            }
            body::BodyTraversalStep::FootnoteCatalog => {
                render_footnote_catalog(output, document.catalogs());
            }
            body::BodyTraversalStep::Block {
                block,
                scope,
                render_header_metadata: include_header_metadata,
            } => {
                render_block(output, block, policy, context, *scope);
                if *include_header_metadata {
                    render_header_metadata(output, document.header());
                }
            }
        }
    }
}

fn render_header_metadata(output: &mut String, header: &crate::parser::DocumentHeader) {
    for author in &header.authors {
        BlockWriter::start(output, "p", &[classes(&["author"])]);
        BlockWriter::text(output, &author.name);
        if let Some(email) = &author.email {
            BlockWriter::text(output, " <");
            BlockWriter::text(output, email);
            BlockWriter::text(output, ">");
        }
        BlockWriter::end(output, "p");
        BlockWriter::line_break(output);
    }
    if let Some(revision) = &header.revision {
        BlockWriter::start(output, "p", &[classes(&["revision"])]);
        let mut separator = "";
        for value in [
            revision.number.as_ref(),
            revision.date.as_ref(),
            revision.remark.as_ref(),
        ]
        .into_iter()
        .flatten()
        {
            BlockWriter::text(output, separator);
            BlockWriter::text(output, &value.value);
            separator = " — ";
        }
        BlockWriter::end(output, "p");
        BlockWriter::line_break(output);
    }
}

fn render_block(
    output: &mut String,
    block: &AstBlock,
    policy: &RenderPolicy,
    context: &mut InlineRenderContext<'_, '_>,
    scope: RenderScope,
) {
    let explicit_id = context
        .identifiers
        .target_at(block.range())
        .map(|target| target.id.as_str());
    match block {
        AstBlock::Heading(heading) => {
            let id = if let Some(id) = context
                .identifiers
                .heading_at(heading.text_range)
                .map(|heading| heading.id.as_str())
            {
                id
            } else if let Some(id) = explicit_id {
                id
            } else {
                unreachable!("lowering assigns every heading an identifier")
            };
            render_heading(output, heading, id, policy, context);
        }
        AstBlock::Paragraph(paragraph) => {
            if let Some(admonition) = &paragraph.admonition {
                render_admonition_start(
                    output,
                    admonition,
                    explicit_id,
                    &paragraph.metadata,
                    context,
                );
                render_paragraph(output, paragraph, None, context);
                BlockWriter::end(output, "div");
                BlockWriter::line_break(output);
            } else {
                render_paragraph(output, paragraph, explicit_id, context);
            }
        }
        AstBlock::LiteralParagraph(paragraph) => {
            render_preformatted(output, explicit_id, &paragraph.value);
        }
        AstBlock::Break(block) => render_break(output, block.kind, explicit_id),
        AstBlock::Source(block) => {
            BlockWriter::start(output, "pre", &optional_id(explicit_id));
            let mut attributes = Vec::new();
            if let Some(language) = &block.language {
                if policy.source_languages.allows(language) {
                    attributes.push(source_language_class(language));
                } else if policy.source_languages.unknown == UnknownSourceLanguage::Diagnostic {
                    context.diagnostics.push(render_diagnostic(
                        "source-language-not-allowed",
                        "source language is rejected by the render policy",
                        block.language_range.unwrap_or(block.attribute_range),
                    ));
                }
            }
            BlockWriter::start(output, "code", &attributes);
            BlockWriter::text(output, &block.value);
            BlockWriter::end(output, "code");
            BlockWriter::end(output, "pre");
            BlockWriter::line_break(output);
        }
        AstBlock::Verbatim(block) => match &block.kind {
            crate::parser::VerbatimKind::Source(source) => {
                let has_presentation = block.metadata.title.is_some() || source.line_numbers;
                if has_presentation {
                    let mut attributes = optional_id(explicit_id);
                    attributes.push(classes(&["source-block"]));
                    BlockWriter::start(output, "figure", &attributes);
                    BlockWriter::line_break(output);
                    if let Some(title) = &block.metadata.title {
                        BlockWriter::start(output, "figcaption", &[]);
                        BlockWriter::text(
                            output,
                            &crate::projection::resolved_inline_text(&title.inlines),
                        );
                        BlockWriter::end(output, "figcaption");
                        BlockWriter::line_break(output);
                    }
                }
                let mut pre_attributes = Vec::new();
                if !has_presentation {
                    pre_attributes.extend(optional_id(explicit_id));
                }
                if has_presentation
                    && let Some(language) = &source.language
                    && policy.source_languages.allows(language)
                {
                    pre_attributes.push(passive("data-language", language));
                }
                if source.line_numbers {
                    pre_attributes.push(passive("data-line-numbers", "true"));
                    pre_attributes.push(passive(
                        "data-line-start",
                        source.start_line.unwrap_or(1).to_string(),
                    ));
                }
                BlockWriter::start(output, "pre", &pre_attributes);
                let mut code_attributes = Vec::new();
                if let Some(language) = &source.language {
                    if policy.source_languages.allows(language) {
                        code_attributes.push(source_language_class(language));
                    } else if policy.source_languages.unknown == UnknownSourceLanguage::Diagnostic {
                        context.diagnostics.push(render_diagnostic(
                            "source-language-not-allowed",
                            "source language is rejected by the render policy",
                            source.language_range.unwrap_or(source.attribute_range),
                        ));
                    }
                }
                BlockWriter::start(output, "code", &code_attributes);
                BlockWriter::text(output, &block.value);
                BlockWriter::end(output, "code");
                BlockWriter::end(output, "pre");
                BlockWriter::line_break(output);
                if has_presentation {
                    BlockWriter::end(output, "figure");
                    BlockWriter::line_break(output);
                }
            }
            crate::parser::VerbatimKind::Listing | crate::parser::VerbatimKind::Literal => {
                render_preformatted(output, explicit_id, &block.value);
            }
        },
        AstBlock::List(list) => render_list(output, list, explicit_id, policy, context, scope),
        AstBlock::Math(block) => {
            if policy.math_languages.allowed.contains(&block.language) {
                let mut attributes = optional_id(explicit_id);
                attributes.extend(math_attributes(block.language, "block"));
                BlockWriter::start(output, "pre", &attributes);
                BlockWriter::start(output, "code", &[]);
                BlockWriter::text(output, &block.value);
                BlockWriter::end(output, "code");
                BlockWriter::end(output, "pre");
                BlockWriter::line_break(output);
            } else {
                render_preformatted(output, explicit_id, &block.value);
                context.diagnostics.push(render_diagnostic(
                    "math-language-not-allowed",
                    "math language is rejected by the render policy",
                    block.attribute_range,
                ));
            }
        }
        AstBlock::Delimited(block) => {
            render_delimited(output, block, explicit_id, policy, context, scope);
        }
        AstBlock::Unsupported(block) => render_unsupported(output, block, explicit_id),
    }
}

fn render_preformatted(output: &mut String, explicit_id: Option<&str>, value: &str) {
    BlockWriter::start(output, "pre", &optional_id(explicit_id));
    BlockWriter::text(output, value);
    BlockWriter::end(output, "pre");
    BlockWriter::line_break(output);
}

fn render_delimited(
    output: &mut String,
    block: &crate::parser::DelimitedBlock,
    explicit_id: Option<&str>,
    policy: &RenderPolicy,
    context: &mut InlineRenderContext<'_, '_>,
    scope: RenderScope,
) {
    if let Some(presentation) = &block.presentation {
        match presentation {
            crate::parser::DelimitedPresentation::Admonition(admonition) => {
                render_admonition_start(output, admonition, explicit_id, &block.metadata, context);
                render_delimited_children(output, block, policy, context, scope);
                BlockWriter::end(output, "div");
                BlockWriter::line_break(output);
                return;
            }
            crate::parser::DelimitedPresentation::Quote(quote) => {
                let quote_class = match quote.kind {
                    crate::parser::QuoteKind::Quote => "quote",
                    crate::parser::QuoteKind::Verse => "verse",
                };
                let mut attributes = optional_id(explicit_id);
                attributes.push(classes(&[quote_class]));
                BlockWriter::start(output, "div", &attributes);
                BlockWriter::line_break(output);
                if quote.kind == crate::parser::QuoteKind::Quote {
                    BlockWriter::start(output, "blockquote", &[]);
                    BlockWriter::line_break(output);
                }
                if quote.kind == crate::parser::QuoteKind::Verse {
                    render_verse_children(output, block, policy, context, scope);
                } else {
                    render_delimited_children(output, block, policy, context, scope);
                }
                if quote.kind == crate::parser::QuoteKind::Quote {
                    BlockWriter::end(output, "blockquote");
                    BlockWriter::line_break(output);
                }
                if quote.attribution.is_some() || quote.citation.is_some() {
                    BlockWriter::start(output, "div", &[classes(&["attribution"])]);
                    if let Some(attribution) = &quote.attribution {
                        BlockWriter::text(output, "— ");
                        BlockWriter::text(output, &attribution.value);
                    }
                    if let Some(citation) = &quote.citation {
                        BlockWriter::text(output, " ");
                        BlockWriter::start(output, "cite", &[]);
                        BlockWriter::text(output, &citation.value);
                        BlockWriter::end(output, "cite");
                    }
                    BlockWriter::end(output, "div");
                    BlockWriter::line_break(output);
                }
                BlockWriter::end(output, "div");
                BlockWriter::line_break(output);
                return;
            }
        }
    }
    match &block.content {
        crate::parser::DelimitedContent::Verbatim(value) => {
            if !matches!(block.kind, crate::parser::DelimitedBlockKind::Comment) {
                render_preformatted(output, explicit_id, value);
            }
        }
        crate::parser::DelimitedContent::Passthrough(value) => {
            render_preformatted(output, explicit_id, value);
        }
        crate::parser::DelimitedContent::Table(table) => {
            render_table(
                output,
                table,
                &block.metadata,
                explicit_id,
                policy,
                context,
                scope,
            );
        }
        crate::parser::DelimitedContent::Compound(_) => {
            render_delimited_children(output, block, policy, context, scope);
        }
    }
}

fn render_delimited_children(
    output: &mut String,
    block: &crate::parser::DelimitedBlock,
    policy: &RenderPolicy,
    context: &mut InlineRenderContext<'_, '_>,
    scope: RenderScope,
) {
    if let crate::parser::DelimitedContent::Compound(children) = &block.content {
        for child in children {
            render_block(output, child, policy, context, scope);
        }
    }
}

fn render_verse_children(
    output: &mut String,
    block: &crate::parser::DelimitedBlock,
    policy: &RenderPolicy,
    context: &mut InlineRenderContext<'_, '_>,
    scope: RenderScope,
) {
    let crate::parser::DelimitedContent::Compound(children) = &block.content else {
        return;
    };
    if children
        .iter()
        .all(|child| matches!(child, AstBlock::Paragraph(_)))
    {
        BlockWriter::start(output, "pre", &[]);
        for (index, child) in children.iter().enumerate() {
            let AstBlock::Paragraph(paragraph) = child else {
                unreachable!()
            };
            if index > 0 {
                BlockWriter::line_break(output);
                BlockWriter::line_break(output);
            }
            // Verse preserves source line boundaries. Rendering the stored source text
            // avoids the normal paragraph inline renderer's intentional newline folding.
            BlockWriter::text(output, &paragraph.value);
        }
        BlockWriter::end(output, "pre");
        BlockWriter::line_break(output);
    } else {
        render_delimited_children(output, block, policy, context, scope);
    }
}

fn render_admonition_start(
    output: &mut String,
    admonition: &crate::parser::AdmonitionPresentation,
    explicit_id: Option<&str>,
    metadata: &crate::parser::BlockMetadata,
    context: &mut InlineRenderContext<'_, '_>,
) {
    let kind_class = match admonition.kind.label() {
        "CAUTION" => "admonition-caution",
        "IMPORTANT" => "admonition-important",
        "NOTE" => "admonition-note",
        "TIP" => "admonition-tip",
        "WARNING" => "admonition-warning",
        _ => unreachable!("admonition kinds have fixed labels"),
    };
    let mut attributes = optional_id(explicit_id);
    attributes.push(classes(&["admonition", kind_class]));
    BlockWriter::start(output, "div", &attributes);
    BlockWriter::start(output, "div", &[classes(&["title"])]);
    if let Some(title) = &metadata.title {
        render_inlines(output, &title.inlines, context);
    } else {
        BlockWriter::text(output, admonition.kind.label());
    }
    BlockWriter::end(output, "div");
    BlockWriter::line_break(output);
}

fn render_table(
    output: &mut String,
    table: &crate::table::Table,
    metadata: &crate::parser::BlockMetadata,
    explicit_id: Option<&str>,
    policy: &RenderPolicy,
    context: &mut InlineRenderContext<'_, '_>,
    scope: RenderScope,
) {
    use crate::table::{
        HorizontalAlignment, TableCellStyle, TableFrame, TableGrid, TableSection, TableStripes,
    };
    let frame_class = match table.presentation.frame {
        TableFrame::All => "table-frame-all",
        TableFrame::Ends => "table-frame-ends",
        TableFrame::None => "table-frame-none",
        TableFrame::Sides => "table-frame-sides",
    };
    let grid_class = match table.presentation.grid {
        TableGrid::All => "table-grid-all",
        TableGrid::Columns => "table-grid-cols",
        TableGrid::None => "table-grid-none",
        TableGrid::Rows => "table-grid-rows",
    };
    let stripes_class = match table.presentation.stripes {
        TableStripes::All => "table-stripes-all",
        TableStripes::Even => "table-stripes-even",
        TableStripes::Hover => "table-stripes-hover",
        TableStripes::None => "table-stripes-none",
        TableStripes::Odd => "table-stripes-odd",
    };
    let mut attributes = optional_id(explicit_id);
    attributes.push(classes(&[frame_class, grid_class, stripes_class]));
    if let Some(width) = table.presentation.width {
        attributes.push(passive("width", format!("{width}%")));
    }
    BlockWriter::start(output, "table", &attributes);
    BlockWriter::line_break(output);
    if let Some(caption) = &metadata.title {
        BlockWriter::start(output, "caption", &[]);
        render_inlines(output, &caption.inlines, context);
        BlockWriter::end(output, "caption");
        BlockWriter::line_break(output);
    }
    let mut section = None;
    for row in &table.rows {
        if section != Some(row.section) {
            if let Some(previous) = section {
                BlockWriter::end(output, table_section_name(previous));
                BlockWriter::line_break(output);
            }
            BlockWriter::start(output, table_section_name(row.section), &[]);
            BlockWriter::line_break(output);
            section = Some(row.section);
        }
        BlockWriter::start(output, "tr", &[]);
        BlockWriter::line_break(output);
        for cell in &row.cells {
            let tag = if row.section == TableSection::Header || cell.style == TableCellStyle::Header
            {
                "th"
            } else {
                "td"
            };
            let mut cell_attributes = Vec::new();
            if cell.column_span > 1 {
                cell_attributes.push(passive("colspan", cell.column_span.to_string()));
            }
            if cell.row_span > 1 {
                cell_attributes.push(passive("rowspan", cell.row_span.to_string()));
            }
            let alignment = cell.horizontal_alignment.unwrap_or_else(|| {
                table
                    .columns
                    .get(cell.column_index as usize)
                    .map_or(HorizontalAlignment::Left, |column| {
                        column.horizontal_alignment
                    })
            });
            let vertical_alignment = cell.vertical_alignment.unwrap_or_else(|| {
                table
                    .columns
                    .get(cell.column_index as usize)
                    .map_or(crate::table::VerticalAlignment::Top, |column| {
                        column.vertical_alignment
                    })
            });
            let horizontal_class = match alignment {
                HorizontalAlignment::Left => "table-align-left",
                HorizontalAlignment::Center => "table-align-center",
                HorizontalAlignment::Right => "table-align-right",
            };
            let vertical_class = match vertical_alignment {
                crate::table::VerticalAlignment::Top => "table-valign-top",
                crate::table::VerticalAlignment::Middle => "table-valign-middle",
                crate::table::VerticalAlignment::Bottom => "table-valign-bottom",
            };
            cell_attributes.push(classes(&[horizontal_class, vertical_class]));
            BlockWriter::start(output, tag, &cell_attributes);
            render_table_cell(output, cell, policy, context, scope);
            BlockWriter::end(output, tag);
            BlockWriter::line_break(output);
        }
        BlockWriter::end(output, "tr");
        BlockWriter::line_break(output);
    }
    if let Some(section) = section {
        BlockWriter::end(output, table_section_name(section));
        BlockWriter::line_break(output);
    }
    BlockWriter::end(output, "table");
    BlockWriter::line_break(output);
}

fn table_section_name(section: crate::table::TableSection) -> &'static str {
    match section {
        crate::table::TableSection::Header => "thead",
        crate::table::TableSection::Body => "tbody",
        crate::table::TableSection::Footer => "tfoot",
    }
}

fn render_table_cell(
    output: &mut String,
    cell: &crate::table::TableCell,
    policy: &RenderPolicy,
    context: &mut InlineRenderContext<'_, '_>,
    scope: RenderScope,
) {
    use crate::table::{TableCellContent, TableCellStyle};
    match &cell.content {
        TableCellContent::Verbatim(value) => {
            BlockWriter::start(output, "pre", &[]);
            BlockWriter::text(output, value);
            BlockWriter::end(output, "pre");
        }
        TableCellContent::Inlines(inlines) => {
            let wrapper = match cell.style {
                TableCellStyle::Emphasis => Some("em"),
                TableCellStyle::Monospace => Some("code"),
                TableCellStyle::Strong => Some("strong"),
                _ => None,
            };
            if let Some(wrapper) = wrapper {
                BlockWriter::start(output, wrapper, &[]);
            }
            render_inlines(output, inlines, context);
            if let Some(wrapper) = wrapper {
                BlockWriter::end(output, wrapper);
            }
        }
        TableCellContent::AsciiDoc(blocks) => {
            for block in blocks {
                render_block(output, block, policy, context, scope);
            }
        }
    }
}

fn render_break(output: &mut String, kind: crate::parser::BreakKind, id: Option<&str>) {
    let mut attributes = optional_id(id);
    if kind == crate::parser::BreakKind::Page {
        attributes.push(classes(&["page-break"]));
    }
    BlockWriter::void(output, "hr", &attributes);
    BlockWriter::line_break(output);
}

fn render_list(
    output: &mut String,
    list: &crate::parser::ListBlock,
    explicit_id: Option<&str>,
    policy: &RenderPolicy,
    context: &mut InlineRenderContext<'_, '_>,
    scope: RenderScope,
) {
    let tag = match list.kind {
        crate::parser::ListKind::Unordered => "ul",
        crate::parser::ListKind::Ordered => "ol",
        crate::parser::ListKind::Description => "dl",
        crate::parser::ListKind::Callout => "ol",
    };
    let mut attributes = optional_id(explicit_id);
    if list.kind == crate::parser::ListKind::Callout {
        attributes.push(classes(&["callout-list"]));
    }
    BlockWriter::start(output, tag, &attributes);
    BlockWriter::line_break(output);
    for item in &list.items {
        if list.kind == crate::parser::ListKind::Description {
            for term in &item.terms {
                BlockWriter::start(output, "dt", &[]);
                render_inlines(output, &term.inlines, context);
                BlockWriter::end(output, "dt");
                BlockWriter::line_break(output);
            }
            BlockWriter::start(output, "dd", &[]);
        } else {
            BlockWriter::start(output, "li", &[]);
        }
        if let Some(state) = item.checklist {
            BlockWriter::start(output, "span", &[classes(&["checklist-marker"])]);
            BlockWriter::text(
                output,
                if state == crate::parser::ChecklistState::Checked {
                    "☑"
                } else {
                    "☐"
                },
            );
            BlockWriter::end(output, "span");
            BlockWriter::text(output, " ");
        }
        if let Some(id) = item.callout_id {
            BlockWriter::start(output, "span", &[classes(&["callout-number"])]);
            BlockWriter::text(output, &id.to_string());
            BlockWriter::end(output, "span");
            BlockWriter::text(output, " ");
        }
        render_inlines(output, &item.inlines, context);
        if scope.bibliography_section
            && list.kind == crate::parser::ListKind::Unordered
            && let Some(entry) = bibliography_entry_for_item(&item.inlines, context.catalogs)
        {
            render_bibliography_backrefs(output, entry);
        }
        for child in &item.children {
            BlockWriter::line_break(output);
            render_list(output, child, None, policy, context, scope);
        }
        for continuation in &item.continuations {
            if !output.ends_with('\n') {
                BlockWriter::line_break(output);
            }
            render_block(output, continuation, policy, context, scope);
        }
        BlockWriter::end(
            output,
            if list.kind == crate::parser::ListKind::Description {
                "dd"
            } else {
                "li"
            },
        );
        BlockWriter::line_break(output);
    }
    BlockWriter::end(output, tag);
    BlockWriter::line_break(output);
}

fn bibliography_entry_for_item<'a>(
    inlines: &[Inline],
    catalogs: &'a crate::catalog::DocumentCatalogs,
) -> Option<&'a crate::catalog::BibliographyEntry> {
    inlines.iter().find_map(|inline| {
        let Inline::Macro(node) = inline else {
            return None;
        };
        (node.kind == crate::inline::StandardMacroKind::BibliographyAnchor)
            .then(|| {
                catalogs
                    .bibliography()
                    .iter()
                    .find(|entry| entry.definition_range == node.range)
            })
            .flatten()
    })
}

fn bibliography_reference_id(range: crate::source::TextRange) -> String {
    format!("_bibliography_ref_{}", range.start().to_u32())
}

fn render_bibliography_backrefs(output: &mut String, entry: &crate::catalog::BibliographyEntry) {
    for (index, reference) in entry.references.iter().enumerate() {
        BlockWriter::text(output, " ");
        let target = bibliography_reference_id(reference.range);
        let href = safe::SafeFragmentUrl::new(&target)
            .expect("generated bibliography reference IDs are control-free")
            .into_owned();
        BlockWriter::start(
            output,
            "a",
            &[
                classes(&["bibliography-backref"]),
                body::fragment_url("href", href),
            ],
        );
        BlockWriter::text(output, &format!("↩{}", index + 1));
        BlockWriter::end(output, "a");
    }
}

fn render_heading(
    output: &mut String,
    heading: &Heading,
    id: &str,
    policy: &RenderPolicy,
    context: &mut InlineRenderContext<'_, '_>,
) {
    if !heading.well_formed {
        BlockWriter::start(output, "p", &[]);
        render_inlines(output, &heading.inlines, context);
        BlockWriter::end(output, "p");
        BlockWriter::line_break(output);
        return;
    }

    match heading.kind {
        HeadingKind::DocumentTitle if policy.render_document_title => {
            BlockWriter::start(
                output,
                "h1",
                &[classes(&["document-title"]), passive("id", id)],
            );
            render_inlines(output, &heading.inlines, context);
            BlockWriter::end(output, "h1");
            BlockWriter::line_break(output);
        }
        HeadingKind::DocumentTitle => {}
        HeadingKind::Part => render_heading_level(output, heading, id, 1, context),
        HeadingKind::Section { level } | HeadingKind::Discrete { level } => {
            render_heading_level(output, heading, id, level, context);
        }
    }
}

fn render_heading_level(
    output: &mut String,
    heading: &Heading,
    id: &str,
    level: u8,
    context: &mut InlineRenderContext<'_, '_>,
) {
    let name = match level {
        1 => "h1",
        2 => "h2",
        3 => "h3",
        4 => "h4",
        5 => "h5",
        _ => unreachable!("parser only produces supported heading levels"),
    };
    let mut attributes = Vec::new();
    if context
        .structure
        .heading_at(heading.range)
        .is_some_and(|item| item.kind == crate::structure::SectionKind::Appendix)
    {
        attributes.push(classes(&["appendix"]));
    }
    attributes.push(passive("id", id));
    BlockWriter::start(output, name, &attributes);
    if let Some(presentation) = context.presentation.heading_at(heading.range)
        && presentation.numbered
    {
        render_section_number(output, &presentation.number);
    }
    render_inlines(output, &heading.inlines, context);
    BlockWriter::end(output, name);
    BlockWriter::line_break(output);
}

fn render_section_number(output: &mut String, number: &[u32]) {
    if number.is_empty() {
        return;
    }
    for (index, value) in number.iter().enumerate() {
        if index > 0 {
            BlockWriter::text(output, ".");
        }
        BlockWriter::text(output, &value.to_string());
    }
    BlockWriter::text(output, ". ");
}

fn render_paragraph(
    output: &mut String,
    paragraph: &Paragraph,
    id: Option<&str>,
    context: &mut InlineRenderContext<'_, '_>,
) {
    let mut attributes = optional_id(id);
    if paragraph
        .metadata
        .roles
        .iter()
        .any(|role| role.value == "lead")
        || paragraph
            .metadata
            .attributes
            .iter()
            .any(|attribute| attribute.name.is_none() && attribute.value == "lead")
    {
        attributes.push(classes(&["lead"]));
    }
    BlockWriter::start(output, "p", &attributes);
    render_inlines(output, &paragraph.inlines, context);
    BlockWriter::end(output, "p");
    BlockWriter::line_break(output);
}

fn render_inlines(
    output: &mut String,
    inlines: &[Inline],
    context: &mut InlineRenderContext<'_, '_>,
) {
    let plan = body::plan_inlines(inlines, context);
    body::serialize_inlines(output, &plan);
}

const fn math_class(language: crate::inline::MathLanguage) -> &'static str {
    match language {
        crate::inline::MathLanguage::Latex => "math-latex",
        crate::inline::MathLanguage::Typst => "math-typst",
    }
}

fn math_attributes(
    language: crate::inline::MathLanguage,
    display: &str,
) -> Vec<body::PlannedAttribute> {
    vec![
        classes(&[math_class(language)]),
        passive("data-math-language", language.as_asciidoc_name()),
        passive("data-math-display", display),
    ]
}

struct InlineRenderContext<'inputs, 'render> {
    policy: &'inputs RenderPolicy,
    input_usage: &'render mut RenderInputUsage<'inputs>,
    diagnostics: &'render mut Vec<Diagnostic>,
    catalogs: &'inputs crate::catalog::DocumentCatalogs,
    identifiers: &'inputs crate::document::DocumentIdentifiers,
    structure: &'inputs crate::structure::DocumentStructure,
    presentation: &'inputs crate::presentation::DocumentPresentation,
    generated_bibliography:
        Option<&'render generated_bibliography::PreparedGeneratedBibliography<'inputs>>,
}

fn render_toc(output: &mut String, presentation: &crate::presentation::DocumentPresentation) {
    fn render_entries(
        output: &mut String,
        entries: &[crate::structure::TocEntry],
        presentation: &crate::presentation::DocumentPresentation,
    ) {
        if entries.is_empty() {
            return;
        }
        BlockWriter::start(output, "ul", &[]);
        BlockWriter::line_break(output);
        for entry in entries {
            BlockWriter::start(output, "li", &[]);
            let href = safe::SafeFragmentUrl::new(&entry.id)
                .expect("TOC identifiers are nonempty and control-free")
                .into_owned();
            BlockWriter::start(output, "a", &[body::fragment_url("href", href)]);
            if presentation
                .heading_at(entry.range)
                .is_some_and(|heading| heading.numbered)
            {
                render_section_number(output, &entry.number);
            }
            BlockWriter::text(output, &entry.title);
            BlockWriter::end(output, "a");
            render_entries(output, &entry.children, presentation);
            BlockWriter::end(output, "li");
            BlockWriter::line_break(output);
        }
        BlockWriter::end(output, "ul");
        BlockWriter::line_break(output);
    }

    if presentation.toc().is_empty() {
        return;
    }
    BlockWriter::start(output, "div", &[classes(&["toc"])]);
    BlockWriter::line_break(output);
    render_entries(output, presentation.toc(), presentation);
    BlockWriter::end(output, "div");
    BlockWriter::line_break(output);
}

fn render_footnote_catalog(output: &mut String, catalogs: &crate::catalog::DocumentCatalogs) {
    if catalogs.footnotes().is_empty() {
        return;
    }
    BlockWriter::start(output, "div", &[classes(&["footnotes"])]);
    BlockWriter::line_break(output);
    BlockWriter::start(output, "ol", &[]);
    BlockWriter::line_break(output);
    for footnote in catalogs.footnotes() {
        let footnote_id = format!("_footnote_{}", footnote.number);
        BlockWriter::start(output, "li", &[passive("id", footnote_id)]);
        BlockWriter::inline_text(output, &footnote.text);
        for (index, _) in footnote.occurrences.iter().enumerate() {
            BlockWriter::text(output, " ");
            let target = format!("_footnoteref_{}_{}", footnote.number, index + 1);
            let href = safe::SafeFragmentUrl::new(&target)
                .expect("generated footnote reference IDs are control-free")
                .into_owned();
            BlockWriter::start(
                output,
                "a",
                &[
                    classes(&["footnote-backref"]),
                    body::fragment_url("href", href),
                ],
            );
            BlockWriter::text(output, "↩");
            BlockWriter::end(output, "a");
        }
        BlockWriter::end(output, "li");
        BlockWriter::line_break(output);
    }
    BlockWriter::end(output, "ol");
    BlockWriter::line_break(output);
    BlockWriter::end(output, "div");
    BlockWriter::line_break(output);
}

fn render_diagnostic(code: &str, message: &str, range: crate::source::TextRange) -> Diagnostic {
    Diagnostic {
        id: DiagnosticId::new(format!(
            "{code}@{}:{}",
            range.start().to_u32(),
            range.end().to_u32()
        )),
        code: DiagnosticCode::new(code),
        severity: Severity::Warning,
        message: message.to_owned(),
        range,
        related: Vec::new(),
        fixes: Vec::new(),
    }
}

fn append_plan_diagnostics(
    diagnostics: &mut Vec<Diagnostic>,
    planned: impl IntoIterator<Item = plan::PlanDiagnostic>,
) {
    diagnostics.extend(planned.into_iter().map(|diagnostic| {
        render_diagnostic(diagnostic.code, diagnostic.message, diagnostic.range)
    }));
}

fn render_input_diagnostic(
    code: &str,
    domain: &str,
    message: &str,
    range: crate::source::TextRange,
) -> Diagnostic {
    let mut diagnostic = render_diagnostic(code, message, range);
    diagnostic.id = DiagnosticId::new(format!(
        "{code}:{domain}@{}:{}",
        range.start().to_u32(),
        range.end().to_u32()
    ));
    diagnostic
}

fn render_unsupported(output: &mut String, unsupported: &Unsupported, id: Option<&str>) {
    BlockWriter::start(output, "p", &optional_id(id));
    BlockWriter::text(output, &unsupported.raw);
    BlockWriter::end(output, "p");
    BlockWriter::line_break(output);
}

fn optional_id(id: Option<&str>) -> Vec<body::PlannedAttribute> {
    id.map(|id| vec![passive("id", id)]).unwrap_or_default()
}

#[cfg(test)]
mod tests;
