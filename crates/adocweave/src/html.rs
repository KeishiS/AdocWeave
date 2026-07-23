//! HTML output backend.
//!
//! This module depends on the output-neutral semantic AST. The parser and AST
//! do not depend on this module, so additional output backends can consume the
//! same document without changing parsing behavior.

use std::collections::{BTreeMap, BTreeSet};

use crate::diagnostic::{Diagnostic, DiagnosticCode, DiagnosticId, Severity};
use crate::document::{HeadingId, ReferenceTarget, generate_heading_ids, reference_targets};
use crate::inline::{
    Inline, InlineLiteralKind, InlineStyle, Link, Reference, ReferenceDestination,
};
use crate::parser::{AstBlock, AstDocument, Heading, HeadingKind, Paragraph, Unsupported};
use crate::render::{RenderInputProblemKind, RenderInputUsage, RenderInputs, ResolutionMatch};
use crate::resource::{ResolvedResource, ResourceOutcome};
use crate::url::{UrlContext, UrlPolicy};

pub const HTML_CONTRACT_VERSION: u16 = 8;
pub const ALLOWED_ELEMENTS: &[&str] = &[
    "a", "audio", "body", "br", "code", "dd", "div", "dl", "dt", "em", "h1", "h2", "h3", "h4",
    "h5", "hr", "html", "img", "kbd", "li", "mark", "ol", "p", "pre", "span", "strong", "sub",
    "sup", "table", "tbody", "td", "tfoot", "th", "thead", "tr", "ul", "video",
];
pub const ALLOWED_ATTRIBUTES: &[&str] = &[
    "alt", "class", "colspan", "controls", "height", "href", "id", "rel", "rowspan", "src",
    "target", "title", "width",
];
pub const ALLOWED_CLASSES: &[&str] = &[
    "author",
    "appendix",
    "bibliography-anchor",
    "bibliography-backref",
    "button",
    "callout-list",
    "callout-number",
    "checklist-marker",
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
    "table-align-center",
    "table-align-left",
    "table-align-right",
    "table-valign-bottom",
    "table-valign-middle",
    "table-valign-top",
    "toc",
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

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RenderPolicy {
    pub document_mode: HtmlDocumentMode,
    pub render_document_title: bool,
    /// Enables the optional `kbd`, `btn`, and `menu` presentation macros.
    pub render_ui_macros: bool,
    pub url_policy: UrlPolicy,
    pub external_links: ExternalLinkPresentation,
    pub source_languages: SourceLanguagePolicy,
    pub math_languages: MathLanguagePolicy,
    pub unresolved_references: UnresolvedReferencePresentation,
    pub resources: ResourceCapabilities,
}

impl Default for RenderPolicy {
    fn default() -> Self {
        Self {
            document_mode: HtmlDocumentMode::Fragment,
            render_document_title: true,
            render_ui_macros: false,
            url_policy: UrlPolicy::default(),
            external_links: ExternalLinkPresentation::default(),
            source_languages: SourceLanguagePolicy::default(),
            math_languages: MathLanguagePolicy::default(),
            unresolved_references: UnresolvedReferencePresentation::default(),
            resources: ResourceCapabilities::default(),
        }
    }
}

impl RenderPolicy {
    pub fn allows_url(&self, value: &str, context: UrlContext) -> bool {
        self.url_policy.allows(value, context)
    }

    pub fn classify_url(&self, value: &str, context: UrlContext) -> crate::url::UrlDecision {
        self.url_policy.classify(value, context)
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct HtmlOutput {
    pub contract_version: u16,
    pub html: String,
    pub diagnostics: Vec<Diagnostic>,
    pub document_attributes: BTreeMap<String, String>,
    pub heading_ids: Vec<HeadingId>,
}

pub fn render(document: &AstDocument, policy: &RenderPolicy) -> HtmlOutput {
    render_with_inputs(document, policy, &RenderInputs::default())
}

pub use crate::reference::ResolvedReference;

pub fn render_with_inputs(
    document: &AstDocument,
    policy: &RenderPolicy,
    inputs: &RenderInputs,
) -> HtmlOutput {
    let mut fragment = String::new();
    let document_attributes = document.presentation().attributes().values().clone();
    let heading_ids = generate_heading_ids(document);
    let targets = reference_targets(document);
    let mut diagnostics = Vec::new();
    let mut input_usage = inputs.track_usage();
    {
        let mut inline_context = InlineRenderContext {
            policy,
            targets: &targets,
            input_usage: &mut input_usage,
            diagnostics: &mut diagnostics,
            catalogs: document.catalogs(),
            structure: document.structure(),
            presentation: document.presentation(),
            bibliography_section: false,
        };
        for node in document.layout().nodes() {
            let crate::presentation::LayoutNode::Block(block_id) = node else {
                match node {
                    crate::presentation::LayoutNode::Generated(
                        crate::presentation::GeneratedLayoutNode::TableOfContents,
                    ) => render_toc(&mut fragment, document.presentation()),
                    crate::presentation::LayoutNode::Generated(
                        crate::presentation::GeneratedLayoutNode::FootnoteCatalog,
                    ) => render_footnote_catalog(&mut fragment, document.catalogs()),
                    crate::presentation::LayoutNode::Generated(
                        crate::presentation::GeneratedLayoutNode::BibliographySectionStart(_),
                    ) => inline_context.bibliography_section = true,
                    crate::presentation::LayoutNode::Generated(
                        crate::presentation::GeneratedLayoutNode::BibliographySectionEnd,
                    ) => inline_context.bibliography_section = false,
                    crate::presentation::LayoutNode::Block(_) => unreachable!(),
                }
                continue;
            };
            let range = document
                .index()
                .block_range(*block_id)
                .expect("layout only contains indexed blocks");
            let block = document
                .blocks()
                .iter()
                .find(|block| block.range() == range)
                .expect("layout only contains top-level blocks");
            let explicit_id = targets
                .iter()
                .find(|target| target.target_range == block.range())
                .map(|target| target.id.as_str());
            let heading_id = heading_ids
                .iter()
                .find(|heading| {
                    matches!(block, AstBlock::Heading(value) if value.text_range == heading.range)
                })
                .map(|heading| heading.id.as_str());
            render_block(
                &mut fragment,
                block,
                explicit_id,
                heading_id,
                policy,
                &mut inline_context,
            );
            if matches!(
                block,
                AstBlock::Heading(Heading {
                    kind: HeadingKind::DocumentTitle,
                    ..
                })
            ) && policy.render_document_title
            {
                render_header_metadata(&mut fragment, document.header());
            }
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
    crate::diagnostic::sort_diagnostics(&mut diagnostics);

    let html = if policy.document_mode == HtmlDocumentMode::Complete {
        format!("<!doctype html>\n<html>\n<body>\n{fragment}</body>\n</html>\n")
    } else {
        fragment
    };

    HtmlOutput {
        contract_version: HTML_CONTRACT_VERSION,
        html,
        diagnostics,
        document_attributes,
        heading_ids,
    }
}

fn render_header_metadata(output: &mut String, header: &crate::parser::DocumentHeader) {
    for author in &header.authors {
        output.push_str("<p class=\"author\">");
        escape_html_into(output, &author.name);
        if let Some(email) = &author.email {
            output.push_str(" &lt;");
            escape_html_into(output, email);
            output.push_str("&gt;");
        }
        output.push_str("</p>\n");
    }
    if let Some(revision) = &header.revision {
        output.push_str("<p class=\"revision\">");
        let mut separator = "";
        for value in [
            revision.number.as_ref(),
            revision.date.as_ref(),
            revision.remark.as_ref(),
        ]
        .into_iter()
        .flatten()
        {
            output.push_str(separator);
            escape_html_into(output, &value.value);
            separator = " — ";
        }
        output.push_str("</p>\n");
    }
}

fn render_block(
    output: &mut String,
    block: &AstBlock,
    explicit_id: Option<&str>,
    heading_id: Option<&str>,
    policy: &RenderPolicy,
    context: &mut InlineRenderContext<'_, '_>,
) {
    let explicit_id = explicit_id.or_else(|| {
        context
            .targets
            .iter()
            .find(|target| target.target_range == block.range())
            .map(|target| target.id.as_str())
    });
    match block {
        AstBlock::Heading(heading) => {
            let id = if let Some(id) = heading_id {
                id
            } else if let Some(id) = explicit_id {
                id
            } else {
                unreachable!("lowering assigns every heading an identifier")
            };
            render_heading(output, heading, id, policy, context);
        }
        AstBlock::Paragraph(paragraph) => {
            render_paragraph(output, paragraph, explicit_id, context);
        }
        AstBlock::LiteralParagraph(paragraph) => {
            render_preformatted(output, explicit_id, None, &paragraph.value);
        }
        AstBlock::Break(block) => render_break(output, block.kind, explicit_id),
        AstBlock::Source(block) => {
            output.push_str("<pre");
            render_optional_id(output, explicit_id);
            output.push_str("><code");
            if let Some(language) = &block.language {
                if policy.source_languages.allows(language) {
                    output.push_str(" class=\"language-");
                    escape_html_into(output, &safe_language_class(language));
                    output.push('"');
                } else if policy.source_languages.unknown == UnknownSourceLanguage::Diagnostic {
                    context.diagnostics.push(render_diagnostic(
                        "source-language-not-allowed",
                        "source language is rejected by the render policy",
                        block.language_range.unwrap_or(block.attribute_range),
                    ));
                }
            }
            output.push('>');
            escape_html_into(output, &block.value);
            output.push_str("</code></pre>\n");
        }
        AstBlock::Verbatim(block) => match &block.kind {
            crate::parser::VerbatimKind::Source(source) => {
                output.push_str("<pre");
                render_optional_id(output, explicit_id);
                output.push_str("><code");
                if let Some(language) = &source.language {
                    if policy.source_languages.allows(language) {
                        output.push_str(" class=\"language-");
                        escape_html_into(output, &safe_language_class(language));
                        output.push('"');
                    } else if policy.source_languages.unknown == UnknownSourceLanguage::Diagnostic {
                        context.diagnostics.push(render_diagnostic(
                            "source-language-not-allowed",
                            "source language is rejected by the render policy",
                            source.language_range.unwrap_or(source.attribute_range),
                        ));
                    }
                }
                output.push('>');
                escape_html_into(output, &block.value);
                output.push_str("</code></pre>\n");
            }
            crate::parser::VerbatimKind::Listing | crate::parser::VerbatimKind::Literal => {
                render_preformatted(output, explicit_id, None, &block.value);
            }
        },
        AstBlock::List(list) => render_list(output, list, explicit_id, policy, context),
        AstBlock::Math(block) => {
            if policy.math_languages.allowed.contains(&block.language) {
                render_preformatted(
                    output,
                    explicit_id,
                    Some(math_class(block.language)),
                    &block.value,
                );
            } else {
                render_preformatted(output, explicit_id, None, &block.value);
                context.diagnostics.push(render_diagnostic(
                    "math-language-not-allowed",
                    "math language is rejected by the render policy",
                    block.attribute_range,
                ));
            }
        }
        AstBlock::Delimited(block) => {
            render_delimited(output, block, explicit_id, policy, context);
        }
        AstBlock::Unsupported(block) => render_unsupported(output, block, explicit_id),
    }
}

fn render_preformatted(
    output: &mut String,
    explicit_id: Option<&str>,
    class: Option<&str>,
    value: &str,
) {
    output.push_str("<pre");
    render_optional_id(output, explicit_id);
    if let Some(class) = class {
        output.push_str(" class=\"");
        escape_html_into(output, class);
        output.push('"');
    }
    output.push('>');
    if class.is_some() {
        output.push_str("<code>");
    }
    escape_html_into(output, value);
    if class.is_some() {
        output.push_str("</code>");
    }
    output.push_str("</pre>\n");
}

fn render_delimited(
    output: &mut String,
    block: &crate::parser::DelimitedBlock,
    explicit_id: Option<&str>,
    policy: &RenderPolicy,
    context: &mut InlineRenderContext<'_, '_>,
) {
    match &block.content {
        crate::parser::DelimitedContent::Verbatim(value) => {
            if !matches!(block.kind, crate::parser::DelimitedBlockKind::Comment) {
                output.push_str("<pre");
                render_optional_id(output, explicit_id);
                output.push('>');
                escape_html_into(output, value);
                output.push_str("</pre>\n");
            }
        }
        crate::parser::DelimitedContent::Passthrough(value) => {
            output.push_str("<pre");
            render_optional_id(output, explicit_id);
            output.push('>');
            escape_html_into(output, value);
            output.push_str("</pre>\n");
        }
        crate::parser::DelimitedContent::Table(table) => {
            render_table(output, table, explicit_id, policy, context);
        }
        crate::parser::DelimitedContent::Compound(children) => {
            for child in children {
                render_block(output, child, None, None, policy, context);
            }
        }
    }
}

fn render_table(
    output: &mut String,
    table: &crate::table::Table,
    explicit_id: Option<&str>,
    policy: &RenderPolicy,
    context: &mut InlineRenderContext<'_, '_>,
) {
    use crate::table::{HorizontalAlignment, TableCellStyle, TableSection};
    output.push_str("<table");
    render_optional_id(output, explicit_id);
    output.push_str(">\n");
    let mut section = None;
    for row in &table.rows {
        if section != Some(row.section) {
            if let Some(previous) = section {
                output.push_str(table_section_close(previous));
            }
            output.push_str(match row.section {
                TableSection::Header => "<thead>\n",
                TableSection::Body => "<tbody>\n",
                TableSection::Footer => "<tfoot>\n",
            });
            section = Some(row.section);
        }
        output.push_str("<tr>\n");
        for cell in &row.cells {
            let tag = if row.section == TableSection::Header || cell.style == TableCellStyle::Header
            {
                "th"
            } else {
                "td"
            };
            output.push('<');
            output.push_str(tag);
            if cell.column_span > 1 {
                output.push_str(" colspan=\"");
                output.push_str(&cell.column_span.to_string());
                output.push('"');
            }
            if cell.row_span > 1 {
                output.push_str(" rowspan=\"");
                output.push_str(&cell.row_span.to_string());
                output.push('"');
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
            output.push_str(" class=\"");
            output.push_str(match alignment {
                HorizontalAlignment::Left => "table-align-left",
                HorizontalAlignment::Center => "table-align-center",
                HorizontalAlignment::Right => "table-align-right",
            });
            output.push(' ');
            output.push_str(match vertical_alignment {
                crate::table::VerticalAlignment::Top => "table-valign-top",
                crate::table::VerticalAlignment::Middle => "table-valign-middle",
                crate::table::VerticalAlignment::Bottom => "table-valign-bottom",
            });
            output.push_str("\">");
            render_table_cell(output, cell, policy, context);
            output.push_str("</");
            output.push_str(tag);
            output.push_str(">\n");
        }
        output.push_str("</tr>\n");
    }
    if let Some(section) = section {
        output.push_str(table_section_close(section));
    }
    output.push_str("</table>\n");
}

fn table_section_close(section: crate::table::TableSection) -> &'static str {
    match section {
        crate::table::TableSection::Header => "</thead>\n",
        crate::table::TableSection::Body => "</tbody>\n",
        crate::table::TableSection::Footer => "</tfoot>\n",
    }
}

fn render_table_cell(
    output: &mut String,
    cell: &crate::table::TableCell,
    policy: &RenderPolicy,
    context: &mut InlineRenderContext<'_, '_>,
) {
    use crate::table::{TableCellContent, TableCellStyle};
    match &cell.content {
        TableCellContent::Verbatim(value) => {
            output.push_str("<pre>");
            escape_html_into(output, value);
            output.push_str("</pre>");
        }
        TableCellContent::Inlines(inlines) => {
            let wrapper = match cell.style {
                TableCellStyle::Emphasis => Some("em"),
                TableCellStyle::Monospace => Some("code"),
                TableCellStyle::Strong => Some("strong"),
                _ => None,
            };
            if let Some(wrapper) = wrapper {
                output.push('<');
                output.push_str(wrapper);
                output.push('>');
            }
            render_inlines(output, inlines, context);
            if let Some(wrapper) = wrapper {
                output.push_str("</");
                output.push_str(wrapper);
                output.push('>');
            }
        }
        TableCellContent::AsciiDoc(blocks) => {
            for block in blocks {
                render_block(output, block, None, None, policy, context);
            }
        }
    }
}

fn render_break(output: &mut String, kind: crate::parser::BreakKind, id: Option<&str>) {
    output.push_str("<hr");
    render_optional_id(output, id);
    if kind == crate::parser::BreakKind::Page {
        output.push_str(" class=\"page-break\"");
    }
    output.push_str(">\n");
}

fn render_list(
    output: &mut String,
    list: &crate::parser::ListBlock,
    explicit_id: Option<&str>,
    policy: &RenderPolicy,
    context: &mut InlineRenderContext<'_, '_>,
) {
    let tag = match list.kind {
        crate::parser::ListKind::Unordered => "ul",
        crate::parser::ListKind::Ordered => "ol",
        crate::parser::ListKind::Description => "dl",
        crate::parser::ListKind::Callout => "ol",
    };
    output.push('<');
    output.push_str(tag);
    render_optional_id(output, explicit_id);
    if list.kind == crate::parser::ListKind::Callout {
        output.push_str(" class=\"callout-list\"");
    }
    if list.kind == crate::parser::ListKind::Ordered {
        if let Some(start) = list.presentation.start {
            output.push_str(" start=\"");
            output.push_str(&start.to_string());
            output.push('\"');
        }
        if list.presentation.reversed {
            output.push_str(" reversed");
        }
        match list.presentation.style {
            crate::parser::OrderedListStyle::Arabic | crate::parser::OrderedListStyle::Decimal => {}
            crate::parser::OrderedListStyle::LowerAlpha => output.push_str(" type=\"a\""),
            crate::parser::OrderedListStyle::UpperAlpha => output.push_str(" type=\"A\""),
            crate::parser::OrderedListStyle::LowerRoman => output.push_str(" type=\"i\""),
            crate::parser::OrderedListStyle::UpperRoman => output.push_str(" type=\"I\""),
            crate::parser::OrderedListStyle::LowerGreek => {
                output.push_str(" style=\"list-style-type:lower-greek\"")
            }
        }
    }
    output.push_str(">\n");
    for item in &list.items {
        if list.kind == crate::parser::ListKind::Description {
            for term in &item.terms {
                output.push_str("<dt>");
                render_inlines(output, &term.inlines, context);
                output.push_str("</dt>\n");
            }
            output.push_str("<dd>");
        } else {
            output.push_str("<li>");
        }
        if let Some(state) = item.checklist {
            output.push_str("<span class=\"checklist-marker\">");
            output.push_str(if state == crate::parser::ChecklistState::Checked {
                "☑"
            } else {
                "☐"
            });
            output.push_str("</span> ");
        }
        if let Some(id) = item.callout_id {
            output.push_str("<span class=\"callout-number\">");
            output.push_str(&id.to_string());
            output.push_str("</span> ");
        }
        render_inlines(output, &item.inlines, context);
        if context.bibliography_section && list.kind == crate::parser::ListKind::Unordered {
            if let Some(entry) = bibliography_entry_for_item(&item.inlines, context.catalogs) {
                render_bibliography_backrefs(output, entry);
            }
        }
        for child in &item.children {
            output.push('\n');
            render_list(output, child, None, policy, context);
        }
        for continuation in &item.continuations {
            if !output.ends_with('\n') {
                output.push('\n');
            }
            render_block(output, continuation, None, None, policy, context);
        }
        output.push_str(if list.kind == crate::parser::ListKind::Description {
            "</dd>\n"
        } else {
            "</li>\n"
        });
    }
    output.push_str("</");
    output.push_str(tag);
    output.push_str(">\n");
}

fn safe_language_class(language: &str) -> String {
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
        output.push_str(" <a class=\"bibliography-backref\" href=\"#");
        output.push_str(&bibliography_reference_id(reference.range));
        output.push_str("\">↩");
        output.push_str(&(index + 1).to_string());
        output.push_str("</a>");
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
        output.push_str("<p>");
        render_inlines(output, &heading.inlines, context);
        output.push_str("</p>\n");
        return;
    }

    match heading.kind {
        HeadingKind::DocumentTitle if policy.render_document_title => {
            output.push_str("<h1 class=\"document-title\" id=\"");
            output.push_str(id);
            output.push_str("\">");
            render_inlines(output, &heading.inlines, context);
            output.push_str("</h1>\n");
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
    let level = char::from(b'0' + level);
    output.push_str("<h");
    output.push(level);
    if context
        .structure
        .heading_at(heading.range)
        .is_some_and(|item| item.kind == crate::structure::SectionKind::Appendix)
    {
        output.push_str(" class=\"appendix\"");
    }
    output.push_str(" id=\"");
    output.push_str(id);
    output.push_str("\">");
    if context.presentation.section_numbers_enabled() {
        if let Some(presentation) = context.presentation.heading_at(heading.range) {
            render_section_number(output, &presentation.number);
        }
    }
    render_inlines(output, &heading.inlines, context);
    output.push_str("</h");
    output.push(level);
    output.push_str(">\n");
}

fn render_section_number(output: &mut String, number: &[u32]) {
    if number.is_empty() {
        return;
    }
    for (index, value) in number.iter().enumerate() {
        if index > 0 {
            output.push('.');
        }
        output.push_str(&value.to_string());
    }
    output.push_str(". ");
}

fn render_paragraph(
    output: &mut String,
    paragraph: &Paragraph,
    id: Option<&str>,
    context: &mut InlineRenderContext<'_, '_>,
) {
    output.push_str("<p");
    render_optional_id(output, id);
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
        output.push_str(" class=\"lead\"");
    }
    output.push('>');
    render_inlines(output, &paragraph.inlines, context);
    output.push_str("</p>\n");
}

fn render_inlines(
    output: &mut String,
    inlines: &[Inline],
    context: &mut InlineRenderContext<'_, '_>,
) {
    for inline in inlines {
        match inline {
            Inline::Text(text) => escape_inline_text(output, &text.value),
            Inline::Literal { kind, value, .. } => match kind {
                InlineLiteralKind::Monospace => {
                    output.push_str("<code>");
                    escape_inline_text(output, value);
                    output.push_str("</code>");
                }
            },
            Inline::Styled {
                style, children, ..
            } => {
                if matches!(
                    style,
                    InlineStyle::CurvedDoubleQuote | InlineStyle::CurvedSingleQuote
                ) {
                    output.push_str(if *style == InlineStyle::CurvedDoubleQuote {
                        "“"
                    } else {
                        "‘"
                    });
                    render_inlines(output, children, context);
                    output.push_str(if *style == InlineStyle::CurvedDoubleQuote {
                        "”"
                    } else {
                        "’"
                    });
                    continue;
                }
                let tag = match style {
                    InlineStyle::Strong => "strong",
                    InlineStyle::Emphasis => "em",
                    InlineStyle::Highlight => "mark",
                    InlineStyle::Subscript => "sub",
                    InlineStyle::Superscript => "sup",
                    InlineStyle::CurvedDoubleQuote | InlineStyle::CurvedSingleQuote => {
                        unreachable!()
                    }
                };
                output.push('<');
                output.push_str(tag);
                output.push('>');
                render_inlines(output, children, context);
                output.push_str("</");
                output.push_str(tag);
                output.push('>');
            }
            Inline::AttributeReference { name, value, .. } => {
                if let Some(value) = value {
                    escape_html_into(output, value);
                } else {
                    output.push('{');
                    escape_html_into(output, name);
                    output.push('}');
                }
            }
            Inline::Link(link) => render_link(output, link, context),
            Inline::Reference(reference) => render_reference(output, reference, context),
            Inline::Macro(node) => render_standard_macro(output, node, context),
            Inline::HardBreak { .. } => output.push_str("<br>\n"),
            Inline::Passthrough { value, .. } => escape_inline_text(output, value),
            Inline::Formula(formula) => {
                output.push_str("<code");
                if context
                    .policy
                    .math_languages
                    .allowed
                    .contains(&formula.language)
                {
                    output.push_str(" class=\"");
                    output.push_str(math_class(formula.language));
                    output.push('"');
                } else {
                    context.diagnostics.push(render_diagnostic(
                        "math-language-not-allowed",
                        "math language is rejected by the render policy",
                        formula.range,
                    ));
                }
                output.push('>');
                escape_inline_text(output, &formula.value);
                output.push_str("</code>");
            }
        }
    }
}

fn render_standard_macro(
    output: &mut String,
    node: &crate::inline::StandardMacro,
    context: &mut InlineRenderContext<'_, '_>,
) {
    use crate::inline::StandardMacroKind as Kind;
    let first = node
        .attributes
        .first()
        .map(|attribute| attribute.value.as_str());
    match node.kind {
        Kind::Email => {
            let href = format!("mailto:{}", node.target);
            if !context.policy.allows_url(&href, UrlContext::AuthoredLink) {
                escape_inline_text(output, &node.target);
                return;
            }
            output.push_str("<a href=\"");
            escape_html_into(output, &href);
            output.push_str("\">");
            escape_inline_text(output, &node.target);
            output.push_str("</a>");
        }
        Kind::Footnote => {
            let Some((footnote, occurrence)) = context.catalogs.footnote_occurrence(node.range)
            else {
                escape_inline_text(output, first.unwrap_or(&node.target));
                return;
            };
            output.push_str("<sup class=\"footnote\"><a class=\"footnote-ref\" id=\"_footnoteref_");
            output.push_str(&footnote.number.to_string());
            output.push('_');
            output.push_str(&(occurrence + 1).to_string());
            output.push_str("\" href=\"#_footnote_");
            output.push_str(&footnote.number.to_string());
            output.push_str("\">");
            output.push_str(&footnote.number.to_string());
            output.push_str("</a></sup>");
        }
        Kind::Anchor | Kind::BibliographyAnchor => {
            output.push_str("<span id=\"");
            escape_html_into(output, &node.target);
            if node.kind == Kind::BibliographyAnchor {
                output.push_str("\" class=\"bibliography-anchor");
            }
            output.push_str("\"></span>");
        }
        Kind::IndexTerm => {
            output.push_str("<span class=\"index-term\"></span>");
        }
        Kind::Keyboard => {
            if !context.policy.render_ui_macros {
                escape_inline_text(output, first.unwrap_or(&node.target));
                return;
            }
            output.push_str("<kbd>");
            escape_inline_text(output, first.unwrap_or(&node.target));
            output.push_str("</kbd>");
        }
        Kind::Button => {
            if !context.policy.render_ui_macros {
                escape_inline_text(output, first.unwrap_or(&node.target));
                return;
            }
            output.push_str("<span class=\"button\">");
            escape_inline_text(output, first.unwrap_or(&node.target));
            output.push_str("</span>");
        }
        Kind::Menu => {
            if !context.policy.render_ui_macros {
                escape_inline_text(output, first.unwrap_or(&node.target));
                return;
            }
            output.push_str("<span class=\"menu\">");
            escape_inline_text(output, &node.target);
            for attribute in &node.attributes {
                output.push_str(" › ");
                escape_inline_text(output, &attribute.value);
            }
            output.push_str("</span>");
        }
        Kind::Image | Kind::Icon => render_image_macro(output, node, context),
        Kind::Audio | Kind::Video => render_media_macro(output, node, context),
    }
}

fn render_image_macro(
    output: &mut String,
    node: &crate::inline::StandardMacro,
    context: &mut InlineRenderContext<'_, '_>,
) {
    let alt = macro_attribute(node, "alt", 0).unwrap_or("");
    if !context.policy.resources.images {
        escape_inline_text(output, alt);
        context.diagnostics.push(render_diagnostic(
            "resource-capability-disabled",
            "image rendering is disabled by the host capability profile",
            node.range,
        ));
        return;
    }
    let Some(href) = resolved_resource_href(node, context) else {
        escape_inline_text(output, alt);
        return;
    };
    output.push_str("<img src=\"");
    escape_html_into(output, &href);
    output.push_str("\" alt=\"");
    escape_html_into(output, alt);
    output.push('"');
    render_dimension(output, node, "width", 1);
    render_dimension(output, node, "height", 2);
    if let Some(title) = macro_attribute(node, "title", usize::MAX) {
        output.push_str(" title=\"");
        escape_html_into(output, title);
        output.push('"');
    }
    output.push('>');
}

fn render_media_macro(
    output: &mut String,
    node: &crate::inline::StandardMacro,
    context: &mut InlineRenderContext<'_, '_>,
) {
    if !context.policy.resources.media {
        escape_inline_text(output, &node.target);
        context.diagnostics.push(render_diagnostic(
            "resource-capability-disabled",
            "media rendering is disabled by the host capability profile",
            node.range,
        ));
        return;
    }
    let Some(href) = resolved_resource_href(node, context) else {
        escape_inline_text(output, &node.target);
        return;
    };
    let tag = if node.kind == crate::inline::StandardMacroKind::Audio {
        "audio"
    } else {
        "video"
    };
    output.push('<');
    output.push_str(tag);
    output.push_str(" src=\"");
    escape_html_into(output, &href);
    output.push_str("\" controls>");
    output.push_str("</");
    output.push_str(tag);
    output.push('>');
}

fn resolved_resource_href(
    node: &crate::inline::StandardMacro,
    context: &mut InlineRenderContext<'_, '_>,
) -> Option<String> {
    let resolution = context.input_usage.resource_at(node.range);
    match resolution {
        ResolutionMatch::Unique(ResolvedResource {
            outcome: ResourceOutcome::Resolved(value),
            ..
        }) if context
            .policy
            .allows_url(&value.href, UrlContext::ResolvedResource) =>
        {
            Some(value.href.clone())
        }
        ResolutionMatch::Unique(ResolvedResource {
            outcome: ResourceOutcome::Resolved(_),
            ..
        }) => {
            context.diagnostics.push(render_diagnostic(
                "invalid-url-scheme",
                "resolved resource URL is rejected by the render policy",
                node.target_range,
            ));
            None
        }
        ResolutionMatch::Unique(ResolvedResource {
            outcome: ResourceOutcome::Failed(failure),
            ..
        }) => {
            context.diagnostics.push(render_diagnostic(
                failure.kind.diagnostic_code(),
                "resource resolution failed",
                node.target_range,
            ));
            None
        }
        ResolutionMatch::Missing => {
            context.diagnostics.push(render_diagnostic(
                "unresolved-resource",
                "resource requires host resolution",
                node.target_range,
            ));
            None
        }
        ResolutionMatch::Duplicate => None,
    }
}

fn macro_attribute<'a>(
    node: &'a crate::inline::StandardMacro,
    name: &str,
    position: usize,
) -> Option<&'a str> {
    node.attributes
        .iter()
        .find(|attribute| attribute.name.as_deref() == Some(name))
        .or_else(|| {
            node.attributes
                .get(position)
                .filter(|attribute| attribute.name.is_none())
        })
        .map(|attribute| attribute.value.as_str())
}

fn render_dimension(
    output: &mut String,
    node: &crate::inline::StandardMacro,
    name: &str,
    position: usize,
) {
    if let Some(value) = macro_attribute(node, name, position) {
        if !value.is_empty() && value.bytes().all(|byte| byte.is_ascii_digit()) {
            output.push(' ');
            output.push_str(name);
            output.push_str("=\"");
            output.push_str(value);
            output.push('"');
        }
    }
}

const fn math_class(language: crate::inline::MathLanguage) -> &'static str {
    match language {
        crate::inline::MathLanguage::Latex => "math-latex",
        crate::inline::MathLanguage::Typst => "math-typst",
    }
}

struct InlineRenderContext<'inputs, 'render> {
    policy: &'inputs RenderPolicy,
    targets: &'inputs [ReferenceTarget],
    input_usage: &'render mut RenderInputUsage<'inputs>,
    diagnostics: &'render mut Vec<Diagnostic>,
    catalogs: &'inputs crate::catalog::DocumentCatalogs,
    structure: &'inputs crate::structure::DocumentStructure,
    presentation: &'inputs crate::presentation::DocumentPresentation,
    bibliography_section: bool,
}

fn render_toc(output: &mut String, presentation: &crate::presentation::DocumentPresentation) {
    fn render_entries(
        output: &mut String,
        entries: &[crate::structure::TocEntry],
        section_numbers: bool,
    ) {
        if entries.is_empty() {
            return;
        }
        output.push_str("<ul>\n");
        for entry in entries {
            output.push_str("<li><a href=\"#");
            escape_html_into(output, &entry.id);
            output.push_str("\">");
            if section_numbers {
                render_section_number(output, &entry.number);
            }
            escape_html_into(output, &entry.title);
            output.push_str("</a>");
            render_entries(output, &entry.children, section_numbers);
            output.push_str("</li>\n");
        }
        output.push_str("</ul>\n");
    }

    if presentation.toc().is_empty() {
        return;
    }
    output.push_str("<div class=\"toc\">\n");
    render_entries(
        output,
        presentation.toc(),
        presentation.section_numbers_enabled(),
    );
    output.push_str("</div>\n");
}

fn render_footnote_catalog(output: &mut String, catalogs: &crate::catalog::DocumentCatalogs) {
    if catalogs.footnotes().is_empty() {
        return;
    }
    output.push_str("<div class=\"footnotes\">\n<ol>\n");
    for footnote in catalogs.footnotes() {
        output.push_str("<li id=\"_footnote_");
        output.push_str(&footnote.number.to_string());
        output.push_str("\">");
        escape_inline_text(output, &footnote.text);
        for (index, _) in footnote.occurrences.iter().enumerate() {
            output.push_str(" <a class=\"footnote-backref\" href=\"#_footnoteref_");
            output.push_str(&footnote.number.to_string());
            output.push('_');
            output.push_str(&(index + 1).to_string());
            output.push_str("\">↩</a>");
        }
        output.push_str("</li>\n");
    }
    output.push_str("</ol>\n</div>\n");
}

fn render_link(output: &mut String, link: &Link, context: &mut InlineRenderContext<'_, '_>) {
    if context
        .policy
        .allows_url(&link.target, UrlContext::AuthoredLink)
    {
        output.push_str("<a href=\"");
        escape_html_into(output, &link.target);
        output.push('"');
        if matches!(
            context.policy.external_links,
            ExternalLinkPresentation::NewContext { .. }
        ) && matches!(
            context
                .policy
                .classify_url(&link.target, UrlContext::AuthoredLink),
            crate::url::UrlDecision::Allowed
        ) && link.target.split_once(':').is_some_and(|(scheme, _)| {
            scheme.eq_ignore_ascii_case("http") || scheme.eq_ignore_ascii_case("https")
        }) {
            output.push_str(" target=\"_blank\" rel=\"noopener");
            if matches!(
                context.policy.external_links,
                ExternalLinkPresentation::NewContext { noreferrer: true }
            ) {
                output.push_str(" noreferrer");
            }
            output.push('"');
        }
        output.push('>');
        render_label_or_text(output, &link.label, &link.target_source, context);
        output.push_str("</a>");
    } else {
        render_label_or_text(output, &link.label, &link.target_source, context);
        context.diagnostics.push(render_diagnostic(
            "invalid-url-scheme",
            "URL is rejected by the render policy",
            link.target_range,
        ));
    }
}

fn render_reference(
    output: &mut String,
    reference: &Reference,
    context: &mut InlineRenderContext<'_, '_>,
) {
    let (href, fallback, diagnostic) = match &reference.destination {
        ReferenceDestination::Local { anchor, .. } => {
            if let Some(target) = context.targets.iter().find(|target| target.id == *anchor) {
                (Some(format!("#{anchor}")), target.label.clone(), None)
            } else {
                (
                    None,
                    anchor.clone(),
                    Some(("unresolved-cross-reference", "local anchor does not exist")),
                )
            }
        }
        ReferenceDestination::Invalid => (
            None,
            reference_text(reference),
            Some(("invalid-cross-reference", "invalid cross reference target")),
        ),
        ReferenceDestination::Document { .. } | ReferenceDestination::Scheme { .. } => {
            let resolution = context.input_usage.reference_at(reference.range);
            if let ResolutionMatch::Unique(resolution) = resolution {
                match &resolution.outcome {
                    crate::reference::ResolutionOutcome::Resolved {
                        href,
                        display_text,
                        notices,
                    } if context
                        .policy
                        .allows_url(href, UrlContext::ResolvedReference) =>
                    {
                        for notice in notices {
                            context.diagnostics.push(render_diagnostic(
                                notice.kind.diagnostic_code(),
                                "reference resolution used a fallback",
                                reference.target_range,
                            ));
                        }
                        (
                            Some(href.clone()),
                            display_text
                                .clone()
                                .unwrap_or_else(|| reference_text(reference)),
                            None,
                        )
                    }
                    crate::reference::ResolutionOutcome::Resolved { .. } => (
                        None,
                        reference_text(reference),
                        Some((
                            "invalid-url-scheme",
                            "resolved reference URL is rejected by the render policy",
                        )),
                    ),
                    crate::reference::ResolutionOutcome::Failed(failure) => (
                        None,
                        reference_text(reference),
                        Some((
                            failure.kind.diagnostic_code(),
                            "reference resolution failed",
                        )),
                    ),
                }
            } else if resolution == ResolutionMatch::Duplicate {
                (None, reference_text(reference), None)
            } else {
                (
                    None,
                    reference_text(reference),
                    Some((
                        "unresolved-cross-reference",
                        "cross reference requires host resolution",
                    )),
                )
            }
        }
    };
    if let Some(href) = href {
        output.push_str("<a href=\"");
        escape_html_into(output, &href);
        if context.catalogs.bibliography().iter().any(|entry| {
            entry
                .references
                .iter()
                .any(|candidate| candidate.range == reference.range)
        }) {
            output.push_str("\" id=\"");
            output.push_str(&bibliography_reference_id(reference.range));
        }
        output.push_str("\">");
        render_label_or_text(output, &reference.label, &fallback, context);
        output.push_str("</a>");
    } else {
        match context.policy.unresolved_references {
            UnresolvedReferencePresentation::Target => {
                render_label_or_text(output, &reference.label, &fallback, context);
            }
            UnresolvedReferencePresentation::LabelOnly => {
                render_inlines(output, &reference.label, context);
            }
            UnresolvedReferencePresentation::Hidden => {}
        }
    }
    if let Some((code, message)) = diagnostic {
        context
            .diagnostics
            .push(render_diagnostic(code, message, reference.target_range));
    }
}

fn render_label_or_text(
    output: &mut String,
    label: &[Inline],
    fallback: &str,
    context: &mut InlineRenderContext<'_, '_>,
) {
    if label.is_empty() {
        escape_html_into(output, fallback);
    } else {
        render_inlines(output, label, context);
    }
}

fn reference_text(reference: &Reference) -> String {
    reference.target_source.clone()
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
    output.push_str("<p");
    render_optional_id(output, id);
    output.push('>');
    escape_html_into(output, &unsupported.raw);
    output.push_str("</p>\n");
}

fn render_optional_id(output: &mut String, id: Option<&str>) {
    if let Some(id) = id {
        output.push_str(" id=\"");
        escape_html_into(output, id);
        output.push('"');
    }
}

fn escape_html_into(output: &mut String, text: &str) {
    for character in text.chars() {
        match character {
            '&' => output.push_str("&amp;"),
            '<' => output.push_str("&lt;"),
            '>' => output.push_str("&gt;"),
            '"' => output.push_str("&#34;"),
            '\'' => output.push_str("&#39;"),
            _ => output.push(character),
        }
    }
}

fn escape_inline_text(output: &mut String, text: &str) {
    let mut characters = text.chars().peekable();
    while let Some(character) = characters.next() {
        if character == '\r' {
            if characters.peek() == Some(&'\n') {
                characters.next();
            }
            output.push(' ');
        } else if character == '\n' {
            output.push(' ');
        } else {
            let mut encoded = [0; 4];
            escape_html_into(output, character.encode_utf8(&mut encoded));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        ALLOWED_ATTRIBUTES, ALLOWED_CLASSES, ALLOWED_ELEMENTS, ExternalLinkPresentation,
        HTML_CONTRACT_VERSION, HtmlDocumentMode, MathLanguagePolicy, RenderPolicy,
        ResolvedReference, ResourceCapabilities, SourceLanguagePolicy, UnknownSourceLanguage,
        UnresolvedReferencePresentation, render, render_with_inputs,
    };
    use crate::inline::{Inline, ReferenceDestination};
    use crate::parser::AstBlock;
    use crate::parser::parse;
    use crate::render::RenderInputs;
    use crate::resource::ResolvedResource;
    use crate::url::{UrlContext, UrlDecision};

    fn echo_resource_inputs(document: &crate::parser::AstDocument) -> RenderInputs {
        let mut resources = Vec::new();
        crate::walker::walk(document, |node| {
            if let crate::walker::SemanticNode::Inline(Inline::Macro(node)) = node {
                if crate::resource::ResourceReference::from_macro(node).is_some() {
                    resources.push(ResolvedResource::resolved(
                        node.range,
                        node.target.clone(),
                        None,
                        None,
                    ));
                }
            }
        });
        RenderInputs::new(Vec::new(), resources)
    }

    #[test]
    fn html_renderer_renders_paragraphs_and_folds_source_lines() {
        let parsed = parse("first line\nsecond line\n\nlast").expect("valid source");

        assert_eq!(
            render(&parsed.ast, &RenderPolicy::default()).html,
            "<p>first line second line</p>\n<p>last</p>\n"
        );
    }

    #[test]
    fn appendix_class_comes_from_the_shared_document_structure() {
        let parsed = parse("= Book\n:doctype: book\n\n[appendix]\n== Reference\n").expect("parse");

        assert_eq!(
            render(&parsed.ast, &RenderPolicy::default()).html,
            "<h1 class=\"document-title\" id=\"_book\">Book</h1>\n<h1 class=\"appendix\" id=\"_reference\">Reference</h1>\n"
        );
    }

    #[test]
    fn toc_and_section_numbers_render_from_document_presentation_layout() {
        let parsed = parse(
            "= Book\n:toc:\n:toclevels: 1\n:sectnums:\n\n== First\n=== Hidden child\n\n== Second\n",
        )
        .expect("parse");

        assert_eq!(
            render(&parsed.ast, &RenderPolicy::default()).html,
            "<h1 class=\"document-title\" id=\"_book\">Book</h1>\n<div class=\"toc\">\n<ul>\n<li><a href=\"#_first\">1. First</a></li>\n<li><a href=\"#_second\">2. Second</a></li>\n</ul>\n</div>\n<h1 id=\"_first\">1. First</h1>\n<h2 id=\"_hidden_child\">1.1. Hidden child</h2>\n<h1 id=\"_second\">2. Second</h1>\n"
        );
    }

    #[test]
    fn book_parts_and_appendices_keep_presentation_numbers_without_changing_ids() {
        let parsed = parse(
            "= Book\n:doctype: book\n:toc:\n:sectnums:\n\n= Part\n\n== Chapter\n\n[appendix]\n== Reference\n",
        )
        .expect("parse");

        assert_eq!(
            render(&parsed.ast, &RenderPolicy::default()).html,
            "<h1 class=\"document-title\" id=\"_book\">Book</h1>\n<div class=\"toc\">\n<ul>\n<li><a href=\"#_part\">1. Part</a><ul>\n<li><a href=\"#_chapter\">1.1. Chapter</a></li>\n<li><a href=\"#_reference\">1.2. Reference</a></li>\n</ul>\n</li>\n</ul>\n</div>\n<h1 id=\"_part\">1. Part</h1>\n<h1 id=\"_chapter\">1.1. Chapter</h1>\n<h1 class=\"appendix\" id=\"_reference\">1.2. Reference</h1>\n"
        );
    }

    #[test]
    fn bibliography_section_uses_catalog_entries_for_citation_back_references() {
        let parsed = parse(
            "= References\n\n[bibliography]\n== Sources\n\n* bibanchor:ref[] Entry\n\nSee <<ref,Entry>>.\n",
        )
        .expect("parse");
        let output = render(&parsed.ast, &RenderPolicy::default()).html;

        assert!(output.contains("<span id=\"ref\" class=\"bibliography-anchor\"></span>"));
        assert!(output.contains("class=\"bibliography-backref\""));
        assert!(output.contains("id=\"_bibliography_ref_"));
        assert!(output.contains("href=\"#_bibliography_ref_"));
    }

    #[test]
    fn bibliography_back_references_are_scoped_to_bibliography_sections() {
        let parsed = parse(
            "* bibanchor:outside[] Outside\n\nSee <<outside>>.\n\n[bibliography]\n== Sources\n\n* bibanchor:inside[] Inside\n\nSee <<inside>>.\n\n== After\n\n* bibanchor:after[] After\n\nSee <<after>>.\n",
        )
        .expect("parse");
        let output = render(&parsed.ast, &RenderPolicy::default()).html;

        assert_eq!(output.matches("class=\"bibliography-backref\"").count(), 1);
        assert!(output.contains("href=\"#_bibliography_ref_"));
    }

    #[test]
    fn inline_regression_keeps_plain_text_html_output_unchanged() {
        let parsed = parse("plain <text>\nnext").expect("valid source");

        assert_eq!(
            render(&parsed.ast, &RenderPolicy::default()).html,
            "<p>plain &lt;text&gt; next</p>\n"
        );
    }

    #[test]
    fn multiline_inline_spans_fold_source_endings_without_losing_markup() {
        let source =
            "before *strong\n日本語* and ``mono\r\ncode`` https://example.org[label\n続き]";
        let parsed = parse(source).expect("valid source");

        assert_eq!(
            render(&parsed.ast, &RenderPolicy::default()).html,
            "<p>before <strong>strong 日本語</strong> and <code>mono code</code> <a href=\"https://example.org\">label 続き</a></p>\n"
        );
    }

    #[test]
    fn monospace_html_escapes_code_content() {
        let parsed = parse("use `<tag>` now").expect("valid source");

        assert_eq!(
            render(&parsed.ast, &RenderPolicy::default()).html,
            "<p>use <code>&lt;tag&gt;</code> now</p>\n"
        );
    }

    #[test]
    fn strong_html_renders_nested_inlines() {
        let parsed = parse("*bold and `code`*").expect("valid source");

        assert_eq!(
            render(&parsed.ast, &RenderPolicy::default()).html,
            "<p><strong>bold and <code>code</code></strong></p>\n"
        );
    }

    #[test]
    fn emphasis_html_renders_nested_inlines() {
        let parsed = parse("_italic and *bold*_").expect("valid source");

        assert_eq!(
            render(&parsed.ast, &RenderPolicy::default()).html,
            "<p><em>italic and <strong>bold</strong></em></p>\n"
        );
    }

    #[test]
    fn literal_block_html_escapes_content_without_inline_parsing() {
        let parsed = parse("....\n<tag> & *strong*\n....\n").expect("valid source");

        assert_eq!(
            render(&parsed.ast, &RenderPolicy::default()).html,
            "<pre>&lt;tag&gt; &amp; *strong*\n</pre>\n"
        );
    }

    #[test]
    fn source_block_html_escapes_code_and_sanitizes_language_class() {
        let parsed = parse("[source, Rust<script>]\n----\n<&>\n----\n").expect("valid source");

        assert_eq!(
            render(&parsed.ast, &RenderPolicy::default()).html,
            "<pre><code class=\"language-rust-script-\">&lt;&amp;&gt;\n</code></pre>\n"
        );
    }

    #[test]
    fn html_renderer_escapes_all_special_characters_and_raw_html() {
        let source = include_str!("../../../fixtures/plain/escaping.adoc");
        let parsed = parse(source).expect("valid source");

        assert_eq!(
            render(&parsed.ast, &RenderPolicy::default()).html,
            include_str!("../../../fixtures/plain/escaping.html")
        );
    }

    #[test]
    fn html_renderer_is_deterministic() {
        let parsed = parse("same input").expect("valid source");
        let options = RenderPolicy::default();

        assert_eq!(render(&parsed.ast, &options), render(&parsed.ast, &options));
    }

    #[test]
    fn html_renderer_can_wrap_a_complete_document() {
        let parsed = parse("paragraph").expect("valid source");

        assert_eq!(
            render(
                &parsed.ast,
                &RenderPolicy {
                    document_mode: HtmlDocumentMode::Complete,
                    ..RenderPolicy::default()
                }
            )
            .html,
            concat!(
                "<!doctype html>\n",
                "<html>\n",
                "<body>\n",
                "<p>paragraph</p>\n",
                "</body>\n",
                "</html>\n"
            )
        );
    }

    #[test]
    fn html_contract_golden_covers_fragment_and_complete_document() {
        let parsed =
            parse(include_str!("../../../fixtures/html/contract.adoc")).expect("valid source");
        assert_eq!(
            render(&parsed.ast, &RenderPolicy::default()).html,
            include_str!("../../../fixtures/html/contract.fragment.html")
        );
        assert_eq!(
            render(
                &parsed.ast,
                &RenderPolicy {
                    document_mode: HtmlDocumentMode::Complete,
                    ..RenderPolicy::default()
                }
            )
            .html,
            include_str!("../../../fixtures/html/contract.complete.html")
        );
    }

    #[test]
    fn render_policy_allows_only_configured_safe_schemes() {
        let mut policy = RenderPolicy::default();
        assert_eq!(
            policy.classify_url("https://example.com", UrlContext::AuthoredLink),
            UrlDecision::Allowed
        );
        assert_eq!(
            policy.classify_url("HTTP://example.com", UrlContext::AuthoredLink),
            UrlDecision::Allowed
        );
        assert_eq!(
            policy.classify_url("javascript:alert(1)", UrlContext::AuthoredLink),
            UrlDecision::Rejected
        );
        assert_eq!(
            policy.classify_url("java%0ascript:alert(1)", UrlContext::AuthoredLink),
            UrlDecision::Rejected
        );
        assert_eq!(
            policy.classify_url("relative.adoc", UrlContext::AuthoredLink),
            UrlDecision::Rejected
        );
        assert_eq!(
            policy.classify_url("/absolute", UrlContext::AuthoredLink),
            UrlDecision::Rejected
        );
        assert_eq!(
            policy.classify_url("data:text/html,x", UrlContext::AuthoredLink),
            UrlDecision::Rejected
        );

        policy
            .url_policy
            .allowed_schemes
            .insert("mailto".to_owned());
        policy.url_policy.allow_relative = true;
        assert!(policy.allows_url("mailto:user@example.com", UrlContext::AuthoredLink));
        assert!(policy.allows_url("relative.adoc", UrlContext::AuthoredLink));
        assert!(!policy.allows_url("../outside.adoc", UrlContext::AuthoredLink));

        let parsed = parse("link:relative.adoc[relative]").expect("parse");
        assert_eq!(
            render(&parsed.ast, &policy).html,
            "<p><a href=\"relative.adoc\">relative</a></p>\n"
        );
    }

    #[test]
    fn external_link_attributes_are_fixed_and_do_not_apply_to_xrefs() {
        let analysis = crate::core::Engine::new(crate::core::ParseOptions::default())
            .analyze("https://example.com/[External] xref:note:123[Internal]")
            .expect("analysis");
        let policy = RenderPolicy {
            external_links: ExternalLinkPresentation::NewContext { noreferrer: true },
            ..RenderPolicy::default()
        };
        let output = render_with_inputs(
            analysis.ast(),
            &policy,
            &RenderInputs::new(
                vec![ResolvedReference::resolved(
                    analysis.references()[0].range,
                    "https://app.example/notes/123",
                )],
                Vec::new(),
            ),
        );

        assert!(output.html.contains(
            "href=\"https://example.com/\" target=\"_blank\" rel=\"noopener noreferrer\""
        ));
        assert!(
            output
                .html
                .contains("<a href=\"https://app.example/notes/123\">Internal</a>")
        );
    }

    #[test]
    fn source_math_reference_and_resource_policies_fail_closed() {
        let source = "[source,python]\n----\nprint(1)\n----\n\nstem:[x] xref:note:secret[] image:https://example/x.png[alt]";
        let analysis = crate::core::Engine::new(crate::core::ParseOptions::default())
            .analyze(source)
            .expect("analysis");
        let image = analysis.resource_queries()[0].reference.range;
        let policy = RenderPolicy {
            source_languages: SourceLanguagePolicy {
                allowed: Some(["rust".to_owned()].into_iter().collect()),
                unknown: UnknownSourceLanguage::Diagnostic,
            },
            math_languages: MathLanguagePolicy {
                allowed: std::collections::BTreeSet::new(),
            },
            unresolved_references: UnresolvedReferencePresentation::LabelOnly,
            resources: ResourceCapabilities {
                images: false,
                media: false,
            },
            ..RenderPolicy::default()
        };
        let output = render_with_inputs(
            analysis.ast(),
            &policy,
            &RenderInputs::new(
                Vec::new(),
                vec![ResolvedResource::resolved(
                    image,
                    "https://cdn.example/x.png",
                    None,
                    None,
                )],
            ),
        );

        assert!(!output.html.contains("language-python"));
        assert!(!output.html.contains("math-latex"));
        assert!(!output.html.contains("note:secret"));
        assert!(!output.html.contains("<img"));
        let codes = output
            .diagnostics
            .iter()
            .map(|diagnostic| diagnostic.code.as_str())
            .collect::<Vec<_>>();
        assert!(codes.contains(&"source-language-not-allowed"));
        assert!(codes.contains(&"math-language-not-allowed"));
        assert!(codes.contains(&"resource-capability-disabled"));
    }

    #[test]
    fn resolved_reference_notices_are_projected_as_render_diagnostics() {
        let analysis = crate::core::Engine::new(crate::core::ParseOptions::default())
            .analyze("xref:note:123#missing[Note]")
            .expect("analysis");
        let output = render_with_inputs(
            analysis.ast(),
            &RenderPolicy::default(),
            &RenderInputs::new(
                vec![
                    ResolvedReference::resolved(
                        analysis.references()[0].range,
                        "https://app.example/notes/123",
                    )
                    .with_notices(vec![crate::reference::ResolutionNotice {
                        kind: crate::reference::ResolutionNoticeKind::Fallback,
                    }]),
                ],
                Vec::new(),
            ),
        );

        assert_eq!(
            output.diagnostics[0].code.as_str(),
            "reference-resolution-fallback"
        );
    }

    #[test]
    fn kind_only_reference_failure_uses_a_fixed_diagnostic() {
        let analysis = crate::core::Engine::new(crate::core::ParseOptions::default())
            .analyze("xref:record:private[Public label]")
            .expect("analysis");
        let output = render_with_inputs(
            analysis.ast(),
            &RenderPolicy::default(),
            &RenderInputs::new(
                vec![ResolvedReference::failed(
                    analysis.references()[0].range,
                    crate::reference::ResolverFailure {
                        kind: crate::reference::ResolutionFailureKind::MissingTarget,
                    },
                )],
                Vec::new(),
            ),
        );

        assert_eq!(output.html, "<p>Public label</p>\n");
        assert_eq!(
            output.diagnostics[0].code.as_str(),
            "missing-reference-target"
        );
        assert_eq!(output.diagnostics[0].message, "reference resolution failed");
    }

    #[test]
    fn resolved_display_text_is_plain_text_and_only_fills_an_empty_label() {
        let analysis = crate::core::Engine::new(crate::core::ParseOptions::default())
            .analyze(
                "xref:note:01800000-0000-7000-8000-000000000001[]\n\n\
                 xref:note:01800000-0000-7000-8000-000000000002[Authored *label*]",
            )
            .expect("analysis");
        let inputs = RenderInputs::new(
            vec![
                ResolvedReference::resolved(
                    analysis.references()[0].range,
                    "/notes/01800000-0000-7000-8000-000000000001",
                )
                .with_display_text("公開 <タイトル> & *not markup*"),
                ResolvedReference::resolved(
                    analysis.references()[1].range,
                    "/notes/01800000-0000-7000-8000-000000000002",
                )
                .with_display_text("Resolver title must not replace the authored label"),
            ],
            Vec::new(),
        );

        let output = render_with_inputs(
            analysis.ast(),
            &RenderPolicy {
                url_policy: crate::url::UrlPolicy {
                    allow_resolved_root_relative: true,
                    ..crate::url::UrlPolicy::default()
                },
                ..RenderPolicy::default()
            },
            &inputs,
        );

        assert_eq!(
            output.html,
            "<p><a href=\"/notes/01800000-0000-7000-8000-000000000001\">公開 &lt;タイトル&gt; &amp; *not markup*</a></p>\n\
             <p><a href=\"/notes/01800000-0000-7000-8000-000000000002\">Authored <strong>label</strong></a></p>\n"
        );
    }

    #[test]
    fn failed_empty_label_hides_the_target_in_label_only_mode() {
        let analysis = crate::core::Engine::new(crate::core::ParseOptions::default())
            .analyze("xref:note:private[]")
            .expect("analysis");
        let inputs = RenderInputs::new(
            vec![ResolvedReference::failed(
                analysis.references()[0].range,
                crate::reference::ResolverFailure {
                    kind: crate::reference::ResolutionFailureKind::MissingTarget,
                },
            )],
            Vec::new(),
        );

        let output = render_with_inputs(
            analysis.ast(),
            &RenderPolicy {
                unresolved_references: UnresolvedReferencePresentation::LabelOnly,
                ..RenderPolicy::default()
            },
            &inputs,
        );

        assert_eq!(output.html, "<p></p>\n");
        assert!(!output.html.contains("private"));
    }

    #[test]
    fn html_contract_has_explicit_allowlists() {
        assert_eq!(HTML_CONTRACT_VERSION, 8);
        assert_eq!(
            ALLOWED_ELEMENTS,
            [
                "a", "audio", "body", "br", "code", "dd", "div", "dl", "dt", "em", "h1", "h2",
                "h3", "h4", "h5", "hr", "html", "img", "kbd", "li", "mark", "ol", "p", "pre",
                "span", "strong", "sub", "sup", "table", "tbody", "td", "tfoot", "th", "thead",
                "tr", "ul", "video"
            ]
        );
        assert_eq!(
            ALLOWED_ATTRIBUTES,
            [
                "alt", "class", "colspan", "controls", "height", "href", "id", "rel", "rowspan",
                "src", "target", "title", "width"
            ]
        );
        assert_eq!(
            ALLOWED_CLASSES,
            [
                "author",
                "appendix",
                "bibliography-anchor",
                "bibliography-backref",
                "button",
                "callout-list",
                "callout-number",
                "checklist-marker",
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
                "table-align-center",
                "table-align-left",
                "table-align-right",
                "table-valign-bottom",
                "table-valign-middle",
                "table-valign-top",
                "toc"
            ]
        );
        let parsed = parse("paragraph").expect("parse");
        assert_eq!(
            render(&parsed.ast, &RenderPolicy::default()).contract_version,
            HTML_CONTRACT_VERSION
        );
    }

    #[test]
    fn html_security_never_passes_input_elements_or_attributes_through() {
        let parsed = parse(
            "<script>alert(1)</script>\n\
             <svg onload=\"alert(1)\"></svg>\n\
             <p style=\"color:red\">unsafe</p>\n",
        )
        .expect("valid source");
        let html = render(&parsed.ast, &RenderPolicy::default()).html;

        assert!(!html.contains("<script"));
        assert!(!html.contains("<svg"));
        assert!(!html.contains("<svg onload="));
        assert!(!html.contains("<p style="));
        assert!(html.contains("&lt;script&gt;"));
        assert!(html.contains("&lt;svg onload=&#34;alert(1)&#34;&gt;"));

        let parsed =
            parse("[#safe.evil%interactive,onclick=\"alert(1)\",style=\"display:none\"]\nText\n")
                .expect("metadata source");
        let html = render(&parsed.ast, &RenderPolicy::default()).html;
        assert_eq!(html, "<p id=\"safe\">Text</p>\n");
        assert!(!html.contains("onclick"));
        assert!(!html.contains("display:none"));
        assert!(!html.contains("evil"));

        let parsed = parse(
            "++++\n<script>alert(1)</script>\n++++\n\n////\n<script>hidden</script>\n////\n\n====\ninside *safe*\n====\n",
        )
        .expect("delimited source");
        let html = render(&parsed.ast, &RenderPolicy::default()).html;
        assert!(html.contains("&lt;script&gt;alert(1)&lt;/script&gt;"));
        assert!(!html.contains("<script>"));
        assert!(!html.contains("hidden"));
        assert!(html.contains("<p>inside <strong>safe</strong></p>"));
    }

    #[test]
    fn document_attributes_are_substituted_once_and_exposed_as_metadata() {
        let parsed = parse("= Note\n:name: <Alice>\n\nHello {name}; {missing}.\n").expect("parse");
        let output = render(&parsed.ast, &RenderPolicy::default());

        assert_eq!(
            output.html,
            "<h1 class=\"document-title\" id=\"_note\">Note</h1>\n\
             <p>Hello &lt;Alice&gt;; {missing}.</p>\n"
        );
        assert_eq!(
            output.document_attributes.get("name"),
            Some(&"<Alice>".to_owned())
        );
    }

    #[test]
    fn links_apply_attributes_labels_and_url_policy() {
        let parsed = parse(
            "= Links\n:host: example.com\n\n\
             https://{host}[*safe*] javascript:alert(1)[unsafe]\n",
        )
        .expect("parse");
        let output = render(&parsed.ast, &RenderPolicy::default());

        assert!(
            output
                .html
                .contains("<a href=\"https://example.com\"><strong>safe</strong></a>")
        );
        assert!(output.html.contains(" unsafe</p>"));
        assert!(!output.html.contains("javascript:"));
        assert!(
            output
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code.as_str() == "invalid-url-scheme")
        );
    }

    #[test]
    fn link_target_attributes_expand_recursively() {
        let parsed = parse("= Links\n:a: {b}\n:b: expanded\n\nhttps://example.com/{a}[target]\n")
            .expect("parse");
        let AstBlock::Paragraph(paragraph) = &parsed.ast.blocks()[1] else {
            panic!("paragraph");
        };
        let Inline::Link(link) = &paragraph.inlines[0] else {
            panic!("link");
        };

        assert_eq!(link.target, "https://example.com/expanded");
    }

    #[test]
    fn ordered_substitutions_render_styles_replacements_and_safe_passthroughs() {
        let parsed = parse("= Pipeline\n:a: {b}\n:b: value\n\n{a} #mark# H~2~O E=mc^2^ \"`double`\" (C) ... +<b>*raw*</b>+\n\n++++\n<script>alert(1)</script>\n++++\n").expect("parse");
        let html = render(&parsed.ast, &RenderPolicy::default()).html;
        assert!(html.contains("<p>value <mark>mark</mark> H<sub>2</sub>O E=mc<sup>2</sup> “double” © … &lt;b&gt;*raw*&lt;/b&gt;</p>"));
        assert!(html.contains("&lt;script&gt;alert(1)&lt;/script&gt;"));
        assert!(!html.contains("<script>"));
    }

    #[test]
    fn cross_references_resolve_locally_or_from_safe_host_results() {
        let source = "[[local]]\n== Local\n\n<<local,Here>> xref:other.adoc#part[There]";
        let parsed = parse(source).expect("parse");
        let external = parsed
            .ast
            .blocks()
            .iter()
            .find_map(|block| match block {
                AstBlock::Paragraph(paragraph) => {
                    paragraph.inlines.iter().find_map(|inline| match inline {
                        Inline::Reference(reference)
                            if matches!(
                                reference.destination,
                                ReferenceDestination::Document { .. }
                            ) =>
                        {
                            Some(reference.range)
                        }
                        _ => None,
                    })
                }
                _ => None,
            })
            .expect("external reference");
        let output = render_with_inputs(
            &parsed.ast,
            &RenderPolicy::default(),
            &crate::render::RenderInputs::new(
                vec![ResolvedReference::resolved(
                    external,
                    "https://notes.example/part",
                )],
                vec![],
            ),
        );

        assert!(output.html.contains("<a href=\"#local\">Here</a>"));
        assert!(
            output
                .html
                .contains("<a href=\"https://notes.example/part\">There</a>")
        );
        assert!(output.diagnostics.is_empty());
    }

    #[test]
    fn unresolved_cross_references_render_as_safe_non_links() {
        let parsed = parse("xref:#missing[<Missing>] xref:other.adoc[Other]").expect("parse");
        let output = render(&parsed.ast, &RenderPolicy::default());

        assert_eq!(output.html, "<p>&lt;Missing&gt; Other</p>\n");
        assert_eq!(
            output
                .diagnostics
                .iter()
                .map(|diagnostic| diagnostic.code.as_str())
                .collect::<Vec<_>>(),
            ["unresolved-cross-reference", "unresolved-cross-reference"]
        );
    }

    #[test]
    fn heading_html_and_ids_match_fixture() {
        let source = include_str!("../../../fixtures/heading/basic.adoc");
        let parsed = parse(source).expect("valid source");
        let output = render(&parsed.ast, &RenderPolicy::default());

        assert_eq!(
            output.html,
            include_str!("../../../fixtures/heading/basic.html")
        );
        assert_eq!(
            output
                .heading_ids
                .iter()
                .map(|heading| heading.id.as_str())
                .collect::<Vec<_>>(),
            [
                "_document_title",
                "_hello_world",
                "_日本語",
                "_hello_world_2"
            ]
        );
    }

    #[test]
    fn heading_html_can_omit_document_title() {
        let parsed = parse("= Title\n\n== Section").expect("valid source");
        let output = render(
            &parsed.ast,
            &RenderPolicy {
                render_document_title: false,
                ..RenderPolicy::default()
            },
        );

        assert_eq!(output.html, "<h1 id=\"_section\">Section</h1>\n");
    }

    #[test]
    fn heading_id_has_a_deterministic_empty_fallback() {
        let parsed = parse("== !!!").expect("valid source");
        let output = render(&parsed.ast, &RenderPolicy::default());

        assert_eq!(output.heading_ids[0].id, "_section");
    }

    #[test]
    fn anchors_use_the_same_ids_in_html_and_reference_index() {
        let parsed =
            parse("[[heading-id]]\n== Heading\n\n[#paragraph-id]\nParagraph\n").expect("parse");
        let output = render(&parsed.ast, &RenderPolicy::default());

        assert_eq!(
            output.html,
            "<h1 id=\"heading-id\">Heading</h1>\n\
             <p id=\"paragraph-id\">Paragraph</p>\n"
        );
        let target_ids = crate::document::reference_targets(&parsed.ast)
            .into_iter()
            .map(|target| target.id)
            .collect::<Vec<_>>();
        assert_eq!(target_ids, ["heading-id", "paragraph-id"]);
    }

    #[test]
    fn lists_render_nested_and_continued_blocks() {
        let parsed = parse("* one\n** nested\n* code\n+\n....\n<raw>\n....\n").expect("parse");
        let output = render(&parsed.ast, &RenderPolicy::default());

        assert!(output.html.contains("<li>one\n<ul>"));
        assert!(output.html.contains("<pre>&lt;raw&gt;"));
    }

    #[test]
    fn lists_match_the_supported_asciidoctor_fixture() {
        let parsed = parse(include_str!(
            "../../../fixtures/lists/asciidoctor-compatible.adoc"
        ))
        .expect("parse");
        let output = render(&parsed.ast, &RenderPolicy::default());

        assert_eq!(
            output.html,
            include_str!("../../../fixtures/lists/asciidoctor-compatible.html")
        );
    }

    #[test]
    fn standard_list_forms_render_semantic_html() {
        let parsed =
            parse(include_str!("../../../fixtures/lists/standard-forms.adoc")).expect("parse");
        let output = render(&parsed.ast, &RenderPolicy::default());

        assert_eq!(
            output.html,
            include_str!("../../../fixtures/lists/standard-forms.html")
        );
    }

    #[test]
    fn ordered_list_html_uses_resolved_presentation() {
        let parsed = parse("[start=3,%reversed,upperroman]\n. one\n. two\n").expect("parse");
        let output = render(&parsed.ast, &RenderPolicy::default());

        assert_eq!(
            output.html,
            "<ol start=\"3\" reversed type=\"I\">\n<li>one</li>\n<li>two</li>\n</ol>\n"
        );
    }

    #[test]
    fn standard_table_forms_render_allowlisted_semantic_html() {
        let parsed =
            parse(include_str!("../../../fixtures/tables/standard-forms.adoc")).expect("parse");
        let output = render(&parsed.ast, &RenderPolicy::default());

        assert_eq!(
            output.html,
            include_str!("../../../fixtures/tables/standard-forms.html")
        );
    }

    #[test]
    fn advanced_table_formats_and_asciidoc_cells_render_from_typed_content() {
        let source = "[format=csv,options=header]\n|===\nname,value\nalpha,\"one, two\"\n|===\n\n[cols=a]\n|===\n|Paragraph.\n\n* one\n* two\n|===\n";
        let parsed = parse(source).expect("parse");
        let output = render(&parsed.ast, &RenderPolicy::default());
        assert!(output.html.contains("<thead>"));
        assert!(
            output
                .html
                .contains("<td class=\"table-align-left table-valign-top\">one, two</td>")
        );
        assert!(
            output.html.contains(
                "<td class=\"table-align-left table-valign-top\"><p>Paragraph.</p>\n<ul>"
            )
        );
        assert!(output.html.contains("<li>one</li>"));
    }

    #[test]
    fn standard_macros_render_resources_through_the_html_policy() {
        let parsed = parse(include_str!("../../../fixtures/macros/standard.adoc")).expect("parse");
        let output = render_with_inputs(
            &parsed.ast,
            &RenderPolicy::default(),
            &echo_resource_inputs(&parsed.ast),
        );
        assert_eq!(
            output.html,
            include_str!("../../../fixtures/macros/standard.html")
        );

        let parsed = parse("kbd:[Ctrl+C] btn:[Save] menu:File[Open]").expect("parse");
        let output = render(
            &parsed.ast,
            &RenderPolicy {
                render_ui_macros: true,
                ..RenderPolicy::default()
            },
        );
        assert_eq!(
            output.html,
            "<p><kbd>Ctrl+C</kbd> <span class=\"button\">Save</span> <span class=\"menu\">File › Open</span></p>\n"
        );

        let unsafe_resource = parse("image:javascript:alert(1)[safe fallback]").expect("parse");
        let output = render_with_inputs(
            &unsafe_resource.ast,
            &RenderPolicy::default(),
            &echo_resource_inputs(&unsafe_resource.ast),
        );
        assert_eq!(output.html, "<p>safe fallback</p>\n");
        assert!(!output.html.contains("<img"));
        assert_eq!(output.diagnostics[0].code.as_str(), "invalid-url-scheme");
    }

    #[test]
    fn render_inputs_handle_missing_failed_duplicate_and_unused_resources_deterministically() {
        let parsed = parse("image:https://source.example/image.png[alt]").expect("parse");
        let resolved = echo_resource_inputs(&parsed.ast).resources()[0].clone();

        let missing = render(&parsed.ast, &RenderPolicy::default());
        assert_eq!(missing.html, "<p>alt</p>\n");
        assert_eq!(missing.diagnostics[0].code.as_str(), "unresolved-resource");

        let failed = ResolvedResource::failed(
            resolved.source_range,
            crate::resource::ResourceFailure {
                kind: crate::resource::ResourceFailureKind::PermissionDenied,
            },
        );
        let failed = render_with_inputs(
            &parsed.ast,
            &RenderPolicy::default(),
            &RenderInputs::new(vec![], vec![failed]),
        );
        assert_eq!(
            failed.diagnostics[0].code.as_str(),
            "resource-permission-denied"
        );

        let duplicate = render_with_inputs(
            &parsed.ast,
            &RenderPolicy::default(),
            &RenderInputs::new(vec![], vec![resolved.clone(), resolved.clone()]),
        );
        assert_eq!(
            duplicate.diagnostics[0].code.as_str(),
            "duplicate-render-input"
        );

        let unused_range = crate::source::TextRange::new(
            crate::source::TextSize::ZERO,
            crate::source::TextSize::ZERO,
        )
        .expect("range");
        let unused = render_with_inputs(
            &parsed.ast,
            &RenderPolicy::default(),
            &RenderInputs::new(
                vec![],
                vec![
                    resolved,
                    ResolvedResource::resolved(
                        unused_range,
                        "https://unused.example/image.png",
                        None,
                        None,
                    ),
                ],
            ),
        );
        assert!(unused.html.contains("<img"));
        assert_eq!(unused.diagnostics[0].code.as_str(), "unused-render-input");
    }

    #[test]
    fn reference_fallback_preserves_the_source_scheme_spelling() {
        let parsed = parse("xref:Note:123[]").expect("parse");
        let output = render(&parsed.ast, &RenderPolicy::default());

        assert!(output.html.contains("Note:123"));
    }

    #[test]
    fn stem_html_is_escaped_and_matches_the_substitution_fixture() {
        let parsed =
            parse(include_str!("../../../fixtures/stem/substitutions.adoc")).expect("parse");
        let output = render(&parsed.ast, &RenderPolicy::default());

        assert_eq!(
            output.html,
            include_str!("../../../fixtures/stem/substitutions.html")
        );
        assert!(!output.html.contains("<z>"));
    }
}
