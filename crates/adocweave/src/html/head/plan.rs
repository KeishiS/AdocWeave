use crate::block_model::AstDocument;
use crate::diagnostic::{Diagnostic, DiagnosticId};
use crate::structure::SectionKind;
use crate::url::UrlProvenance;

use super::super::safe::{SafeStyleBody, SafeUrl, TextValue};
use super::super::{HtmlDocumentMode, RenderPolicy, StylesheetSource, render_diagnostic};

pub(in crate::html) struct DocumentHeadPlan<'a> {
    pub(super) title: TextValue<'a>,
    pub(super) stylesheets: Vec<PlannedStylesheet<'a>>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum PlannedStylesheet<'a> {
    Inline(SafeStyleBody<'a>),
    External(SafeUrl<'a>),
}

/// Resolves document metadata and host stylesheet configuration into values
/// that are safe for the head serializer. Rejected sources produce diagnostics
/// without exposing their contents.
pub(in crate::html) fn plan_document_head<'a>(
    document: &'a AstDocument,
    policy: &'a RenderPolicy,
    diagnostics: &mut Vec<Diagnostic>,
) -> Option<DocumentHeadPlan<'a>> {
    if policy.document_mode != HtmlDocumentMode::Complete {
        if !policy.stylesheets.sources.is_empty() {
            diagnostics.push(stylesheet_diagnostic(
                "stylesheet-not-applicable",
                0,
                "stylesheets apply only to complete document output",
            ));
        }
        return None;
    }

    let title = document
        .structure()
        .headings()
        .iter()
        .find(|heading| heading.kind == SectionKind::DocumentTitle)
        .map(|heading| heading.title.as_str())
        .filter(|title| !title.trim().is_empty())
        .unwrap_or("AdocWeave document");

    Some(DocumentHeadPlan {
        title: TextValue::new(title),
        stylesheets: plan_stylesheets(policy, diagnostics),
    })
}

fn plan_stylesheets<'a>(
    policy: &'a RenderPolicy,
    diagnostics: &mut Vec<Diagnostic>,
) -> Vec<PlannedStylesheet<'a>> {
    let config = &policy.stylesheets;
    let mut planned = Vec::new();
    let mut emitted: Vec<&StylesheetSource> = Vec::new();

    for (index, source) in config.sources.iter().enumerate() {
        if emitted.contains(&source) {
            continue;
        }
        if emitted.len() == usize::try_from(config.max_sources).unwrap_or(usize::MAX) {
            diagnostics.push(stylesheet_diagnostic(
                "stylesheet-limit-exceeded",
                index,
                &format!(
                    "stylesheet count exceeds the limit of {}",
                    config.max_sources
                ),
            ));
            break;
        }

        let stylesheet = match source {
            StylesheetSource::Inline(css) => {
                if css.len() > usize::try_from(config.max_inline_bytes).unwrap_or(usize::MAX) {
                    diagnostics.push(stylesheet_diagnostic(
                        "stylesheet-limit-exceeded",
                        index,
                        &format!(
                            "inline stylesheet {index} exceeds the limit of {} bytes",
                            config.max_inline_bytes
                        ),
                    ));
                    continue;
                }
                let Some(css) = SafeStyleBody::new(css) else {
                    diagnostics.push(stylesheet_diagnostic(
                        "invalid-stylesheet-content",
                        index,
                        &format!(
                            "inline stylesheet {index} contains a forbidden sequence or control character"
                        ),
                    ));
                    continue;
                };
                PlannedStylesheet::Inline(css)
            }
            StylesheetSource::External(url) => {
                if url.len() > usize::try_from(config.max_url_bytes).unwrap_or(usize::MAX) {
                    diagnostics.push(stylesheet_diagnostic(
                        "stylesheet-limit-exceeded",
                        index,
                        &format!(
                            "stylesheet URL {index} exceeds the limit of {} bytes",
                            config.max_url_bytes
                        ),
                    ));
                    continue;
                }
                let Some(url) =
                    SafeUrl::from_policy(url, &policy.active_urls, UrlProvenance::ResolvedResource)
                else {
                    diagnostics.push(stylesheet_diagnostic(
                        "invalid-stylesheet-url",
                        index,
                        &format!("stylesheet URL {index} is not allowed by the URL policy"),
                    ));
                    continue;
                };
                PlannedStylesheet::External(url)
            }
        };
        planned.push(stylesheet);
        emitted.push(source);
    }

    planned
}

fn stylesheet_diagnostic(code: &str, index: usize, message: &str) -> Diagnostic {
    let range =
        crate::source::TextRange::new(crate::source::TextSize::ZERO, crate::source::TextSize::ZERO)
            .expect("the empty range at the document start is always valid");
    let mut diagnostic = render_diagnostic(code, message, range);
    diagnostic.id = DiagnosticId::new(format!("{code}:stylesheet-{index}@0:0"));
    diagnostic
}

#[cfg(test)]
mod tests {
    use crate::html::{HtmlDocumentMode, RenderPolicy, StylesheetPolicy, StylesheetSource};
    use crate::parser::parse;

    use super::*;

    #[test]
    fn plan_contains_only_valid_deduplicated_stylesheets() {
        let document = parse("= Title").expect("valid source");
        let policy = RenderPolicy {
            document_mode: HtmlDocumentMode::Complete,
            stylesheets: StylesheetPolicy {
                sources: vec![
                    StylesheetSource::Inline("p {}".to_owned()),
                    StylesheetSource::Inline("</style>".to_owned()),
                    StylesheetSource::External("javascript:alert(1)".to_owned()),
                    StylesheetSource::Inline("p {}".to_owned()),
                    StylesheetSource::External("https://example.com/theme.css".to_owned()),
                ],
                ..StylesheetPolicy::default()
            },
            ..RenderPolicy::default()
        };
        let mut diagnostics = Vec::new();

        let plan = plan_document_head(&document.ast, &policy, &mut diagnostics)
            .expect("complete document head");

        assert_eq!(plan.title, TextValue::new("Title"));
        assert_eq!(plan.stylesheets.len(), 2);
        assert!(matches!(
            plan.stylesheets.as_slice(),
            [PlannedStylesheet::Inline(_), PlannedStylesheet::External(_)]
        ));
        assert_eq!(diagnostics.len(), 2);
    }
}
