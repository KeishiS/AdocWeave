//! HTML output backend.
//!
//! This module depends on the output-neutral semantic AST. The parser and AST
//! do not depend on this module, so additional output backends can consume the
//! same document without changing parsing behavior.

mod blocks;
mod body;
mod generated_bibliography;
mod head;
mod plan;
mod safe;

use std::collections::{BTreeMap, BTreeSet};

use crate::block_model::{AstBlock, AstDocument, Heading, HeadingKind, Paragraph, Unsupported};
use crate::diagnostic::{Diagnostic, DiagnosticCode, DiagnosticId, Severity};
use crate::document::HeadingId;
use crate::inline_model::Inline;
use crate::render::{RenderInputProblemKind, RenderInputUsage, RenderInputs};
use crate::url::{ActiveUrlPolicy, UrlProvenance};
use blocks::*;
use body::{BlockWriter, RenderScope, classes, passive, source_language_class};

pub use safe::{ALLOWED_ATTRIBUTES, ALLOWED_CLASSES, ALLOWED_ELEMENTS};

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
    pub allowed: BTreeSet<crate::inline_model::MathLanguage>,
}

impl Default for MathLanguagePolicy {
    fn default() -> Self {
        Self {
            allowed: [
                crate::inline_model::MathLanguage::Latex,
                crate::inline_model::MathLanguage::Typst,
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

#[cfg(test)]
mod tests;
