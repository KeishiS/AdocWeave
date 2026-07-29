//! Safe render planning for links, references, and resolved resources.

use crate::document::DocumentIdentifiers;
use crate::inline::{Link, Reference};
use crate::reference::{ReferenceKey, ResolutionOutcome};
use crate::render::{RenderInputUsage, ResolutionMatch};
use crate::resource::{MediaFamily, ResolvedResource, ResourceOutcome};
use crate::source::TextRange;
use crate::url::UrlProvenance;

use super::safe::{SafeFragmentUrl, SafeUrl};
use super::{ExternalLinkPresentation, RenderPolicy};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct PlanDiagnostic {
    pub(super) code: &'static str,
    pub(super) message: &'static str,
    pub(super) range: TextRange,
}

impl PlanDiagnostic {
    const fn new(code: &'static str, message: &'static str, range: TextRange) -> Self {
        Self {
            code,
            message,
            range,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum PlannedLink<'a> {
    Active {
        href: SafeUrl<'a>,
        new_context: bool,
        noreferrer: bool,
    },
    Fallback {
        diagnostic: PlanDiagnostic,
    },
}

pub(super) fn plan_link<'a>(link: &'a Link, policy: &'a RenderPolicy) -> PlannedLink<'a> {
    let Some(href) =
        SafeUrl::from_policy(&link.target, &policy.active_urls, UrlProvenance::Authored)
    else {
        return PlannedLink::Fallback {
            diagnostic: PlanDiagnostic::new(
                "invalid-url-scheme",
                "URL is rejected by the render policy",
                link.target_range,
            ),
        };
    };
    let external_http = link.target.split_once(':').is_some_and(|(scheme, _)| {
        scheme.eq_ignore_ascii_case("http") || scheme.eq_ignore_ascii_case("https")
    });
    let (new_context, noreferrer) = match policy.external_links {
        ExternalLinkPresentation::NewContext { noreferrer } if external_http => (true, noreferrer),
        _ => (false, false),
    };
    PlannedLink::Active {
        href,
        new_context,
        noreferrer,
    }
}

pub(super) struct PlannedValue<T> {
    pub(super) value: Option<T>,
    pub(super) diagnostics: Vec<PlanDiagnostic>,
}

pub(super) fn plan_resource<'inputs>(
    range: TextRange,
    target_range: TextRange,
    expected_family: MediaFamily,
    policy: &RenderPolicy,
    input_usage: &mut RenderInputUsage<'inputs>,
) -> PlannedValue<SafeUrl<'inputs>> {
    let mut diagnostics = Vec::new();
    let value = match input_usage.resource_at(range) {
        ResolutionMatch::Unique(ResolvedResource {
            outcome: ResourceOutcome::Resolved(value),
            ..
        }) => {
            if let Some(href) = SafeUrl::from_policy(
                &value.href,
                &policy.active_urls,
                UrlProvenance::ResolvedResource,
            ) {
                if value.media_type.family() == expected_family {
                    Some(href)
                } else {
                    diagnostics.push(PlanDiagnostic::new(
                        "resource-media-type-mismatch",
                        "resolved resource media type does not match the macro",
                        target_range,
                    ));
                    None
                }
            } else {
                diagnostics.push(PlanDiagnostic::new(
                    "invalid-url-scheme",
                    "resolved resource URL is rejected by the render policy",
                    target_range,
                ));
                None
            }
        }
        ResolutionMatch::Unique(ResolvedResource {
            outcome: ResourceOutcome::Failed(failure),
            ..
        }) => {
            diagnostics.push(PlanDiagnostic::new(
                failure.kind.diagnostic_code(),
                "resource resolution failed",
                target_range,
            ));
            None
        }
        ResolutionMatch::Missing => {
            diagnostics.push(PlanDiagnostic::new(
                "unresolved-resource",
                "resource requires host resolution",
                target_range,
            ));
            None
        }
        ResolutionMatch::Duplicate => None,
    };
    PlannedValue { value, diagnostics }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum PlannedReferenceHref<'document, 'inputs> {
    Local(SafeFragmentUrl<'document>),
    Resolved(SafeUrl<'inputs>),
}

pub(super) struct PlannedReference<'document, 'inputs> {
    pub(super) href: Option<PlannedReferenceHref<'document, 'inputs>>,
    pub(super) fallback: String,
    pub(super) diagnostics: Vec<PlanDiagnostic>,
}

pub(super) fn plan_reference<'document, 'inputs>(
    reference: &'document Reference,
    identifiers: &'document DocumentIdentifiers,
    policy: &RenderPolicy,
    input_usage: &mut RenderInputUsage<'inputs>,
) -> PlannedReference<'document, 'inputs> {
    let fallback = || reference.target_source.clone();
    match &reference.target {
        Some(ReferenceKey::Local { anchor }) => {
            if let Some(target) = identifiers.target_by_id(anchor)
                && let Some(href) = SafeFragmentUrl::new(anchor)
            {
                PlannedReference {
                    href: Some(PlannedReferenceHref::Local(href)),
                    fallback: target.label.clone(),
                    diagnostics: Vec::new(),
                }
            } else {
                PlannedReference {
                    href: None,
                    fallback: anchor.clone(),
                    diagnostics: vec![PlanDiagnostic::new(
                        "unresolved-cross-reference",
                        "local anchor does not exist",
                        reference.target_range,
                    )],
                }
            }
        }
        None => PlannedReference {
            href: None,
            fallback: fallback(),
            diagnostics: vec![PlanDiagnostic::new(
                "invalid-cross-reference",
                "invalid cross reference target",
                reference.target_range,
            )],
        },
        Some(ReferenceKey::Document { .. }) | Some(ReferenceKey::Scheme { .. }) => {
            match input_usage.reference_at(reference.range) {
                ResolutionMatch::Unique(resolution) => match &resolution.outcome {
                    ResolutionOutcome::Resolved {
                        href,
                        display_text,
                        notices,
                    } => {
                        let diagnostics = notices
                            .iter()
                            .map(|notice| {
                                PlanDiagnostic::new(
                                    notice.kind.diagnostic_code(),
                                    "reference resolution used a fallback",
                                    reference.target_range,
                                )
                            })
                            .collect();
                        if let Some(href) = SafeUrl::from_policy(
                            href,
                            &policy.active_urls,
                            UrlProvenance::ResolvedReference,
                        ) {
                            PlannedReference {
                                href: Some(PlannedReferenceHref::Resolved(href)),
                                fallback: display_text.clone().unwrap_or_else(fallback),
                                diagnostics,
                            }
                        } else {
                            PlannedReference {
                                href: None,
                                fallback: fallback(),
                                diagnostics: vec![PlanDiagnostic::new(
                                    "invalid-url-scheme",
                                    "resolved reference URL is rejected by the render policy",
                                    reference.target_range,
                                )],
                            }
                        }
                    }
                    ResolutionOutcome::Failed(failure) => PlannedReference {
                        href: None,
                        fallback: fallback(),
                        diagnostics: vec![PlanDiagnostic::new(
                            failure.kind.diagnostic_code(),
                            "reference resolution failed",
                            reference.target_range,
                        )],
                    },
                },
                ResolutionMatch::Missing => PlannedReference {
                    href: None,
                    fallback: fallback(),
                    diagnostics: vec![PlanDiagnostic::new(
                        "unresolved-cross-reference",
                        "cross reference requires host resolution",
                        reference.target_range,
                    )],
                },
                ResolutionMatch::Duplicate => PlannedReference {
                    href: None,
                    fallback: fallback(),
                    diagnostics: Vec::new(),
                },
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::inline::Inline;
    use crate::parser::parse;
    use crate::render::RenderInputs;
    use crate::resource::ResolvedResource;
    use crate::source::{TextRange, TextSize};

    use super::*;

    fn range(start: usize, end: usize) -> TextRange {
        TextRange::new(
            TextSize::new(start).expect("valid start"),
            TextSize::new(end).expect("valid end"),
        )
        .expect("ordered range")
    }

    #[test]
    fn link_plan_owns_policy_decisions_without_serializing_html() {
        let link = Link {
            range: range(0, 24),
            macro_name_range: None,
            target_range: range(0, 19),
            target_source: "https://example.test".to_owned(),
            target: "https://example.test".to_owned(),
            target_attributes: Vec::new(),
            target_expansion_error: None,
            label_range: None,
            label: Vec::new(),
        };
        let policy = RenderPolicy {
            external_links: ExternalLinkPresentation::NewContext { noreferrer: true },
            ..RenderPolicy::default()
        };
        assert!(matches!(
            plan_link(&link, &policy),
            PlannedLink::Active {
                new_context: true,
                noreferrer: true,
                ..
            }
        ));

        let mut rejected = link;
        rejected.target = "javascript:alert(1)".to_owned();
        assert!(matches!(
            plan_link(&rejected, &policy),
            PlannedLink::Fallback {
                diagnostic: PlanDiagnostic {
                    code: "invalid-url-scheme",
                    ..
                }
            }
        ));
    }

    #[test]
    fn resource_plan_returns_only_policy_checked_family_matched_urls() {
        let source_range = range(2, 8);
        let resolved = ResolvedResource::resolved(
            source_range,
            "https://example.test/image.png",
            "image/png".parse().expect("media type"),
            Some(42),
        );
        let inputs = RenderInputs::new(Vec::new(), vec![resolved]);
        let mut usage = inputs.track_usage();
        let planned = plan_resource(
            source_range,
            source_range,
            MediaFamily::Image,
            &RenderPolicy::default(),
            &mut usage,
        );
        assert!(planned.value.is_some());
        assert!(planned.diagnostics.is_empty());

        let mut usage = inputs.track_usage();
        let mismatched = plan_resource(
            source_range,
            source_range,
            MediaFamily::Video,
            &RenderPolicy::default(),
            &mut usage,
        );
        assert!(mismatched.value.is_none());
        assert_eq!(
            mismatched.diagnostics[0].code,
            "resource-media-type-mismatch"
        );
    }

    #[test]
    fn local_reference_plan_uses_the_shared_identifier_index() {
        let parsed = parse("[#local]\n== Section\n\nSee <<local>>.").expect("valid source");
        let mut reference = None;
        crate::walker::walk_ast(&parsed.ast, |node| {
            if let crate::walker::SemanticNode::Inline(Inline::Reference(candidate)) = node {
                reference = Some(candidate.clone());
            }
        });
        let reference = reference.expect("reference");
        let inputs = RenderInputs::default();
        let mut usage = inputs.track_usage();
        let planned = plan_reference(
            &reference,
            parsed.ast.identifiers(),
            &RenderPolicy::default(),
            &mut usage,
        );
        assert!(matches!(planned.href, Some(PlannedReferenceHref::Local(_))));
        assert_eq!(planned.fallback, "Section");
        assert!(planned.diagnostics.is_empty());
    }
}
