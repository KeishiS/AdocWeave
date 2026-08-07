//! Analysis-independent render-input validation.

use crate::render_input_wire::{
    WasmCitationOutcome, WasmReferenceOutcome, WasmRenderInputs, WasmResourceOutcome,
};
use crate::render_input_wire_generated::MAX_SAFE_INTEGER;
use crate::{WasmError, WasmLimits, WasmOutputLimits};

/// Render inputs whose count and allocation limits were validated.
///
/// The inner wire value is private so meaning conversion cannot consume an
/// unnormalized public value.
#[derive(Debug)]
pub(crate) struct NormalizedRenderInputs(WasmRenderInputs);

pub(crate) fn normalize(
    inputs: WasmRenderInputs,
    analysis_limits: &WasmLimits,
    output_limits: &WasmOutputLimits,
) -> Result<NormalizedRenderInputs, WasmError> {
    let count = inputs.references.len() as u64
        + inputs.resources.len() as u64
        + inputs.citations.len() as u64
        + inputs
            .generated_bibliography
            .as_ref()
            .map_or(0, |bibliography| bibliography.entries.len() as u64);
    if count > u64::from(analysis_limits.max_references) {
        return Err(limit_error("render input count"));
    }
    let reference_bytes = inputs.references.iter().map(|input| match &input.outcome {
        WasmReferenceOutcome::Resolved {
            href, display_text, ..
        } => href.len() as u64 + display_text.as_ref().map_or(0, |text| text.len()) as u64,
        WasmReferenceOutcome::Failed { .. } => 0,
    });
    let resource_bytes = inputs.resources.iter().map(|input| match &input.outcome {
        WasmResourceOutcome::Resolved {
            href, media_type, ..
        } => href.len() as u64 + media_type.len() as u64,
        WasmResourceOutcome::Failed { .. } => 0,
    });
    // A citation carries the text the host wants shown, so its segments count
    // against the same output budget as a resolved href.
    let citation_bytes = inputs.citations.iter().map(|input| match &input.outcome {
        WasmCitationOutcome::Resolved { segments } => segments
            .iter()
            .map(|segment| {
                segment.text.len() as u64 + segment.anchor.as_ref().map_or(0, String::len) as u64
            })
            .fold(0_u64, u64::saturating_add),
        WasmCitationOutcome::Failed { .. } => 0,
    });
    let bibliography_bytes = inputs
        .generated_bibliography
        .iter()
        .flat_map(|bibliography| {
            std::iter::once(bibliography.title.len() as u64).chain(bibliography.entries.iter().map(
                |entry| {
                    entry.citation_key.len() as u64
                        + entry.text.len() as u64
                        + entry.label.as_ref().map_or(0, String::len) as u64
                },
            ))
        });
    let bytes = reference_bytes
        .chain(resource_bytes)
        .chain(citation_bytes)
        .chain(bibliography_bytes)
        .fold(0_u64, u64::saturating_add);
    if bytes > u64::from(output_limits.max_output_bytes) {
        return Err(limit_error("render input bytes"));
    }
    if inputs.resources.iter().any(|input| {
        matches!(
            input.outcome,
            WasmResourceOutcome::Resolved {
                byte_length: Some(value),
                ..
            } if value > MAX_SAFE_INTEGER
        )
    }) {
        return Err(invalid_input());
    }
    Ok(NormalizedRenderInputs(inputs))
}

impl NormalizedRenderInputs {
    pub(super) fn into_wire(self) -> WasmRenderInputs {
        self.0
    }
}

fn limit_error(resource: &str) -> WasmError {
    WasmError {
        code: "limit-exceeded".to_owned(),
        message: format!("{resource} exceeds the configured processing limit"),
    }
}

fn invalid_input() -> WasmError {
    WasmError {
        code: "invalid-render-input".to_owned(),
        message: "render input is invalid".to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::render_input_wire::{
        WasmGeneratedBibliography, WasmGeneratedBibliographyEntry, WasmReferenceFailureKind,
        WasmReferenceOutcome, WasmResolvedReference, WasmResolvedResource, WasmResourceFailureKind,
        WasmResourceOutcome,
    };

    fn limits(max_references: u32) -> WasmLimits {
        WasmLimits {
            max_references,
            ..WasmLimits::default()
        }
    }

    fn output_limits(max_output_bytes: u32) -> WasmOutputLimits {
        WasmOutputLimits { max_output_bytes }
    }

    #[test]
    fn count_limit_accepts_the_exact_boundary_and_rejects_one_more() {
        let reference = WasmResolvedReference {
            source_start: 0,
            source_end: 0,
            outcome: WasmReferenceOutcome::Failed {
                kind: WasmReferenceFailureKind::MissingTarget,
            },
        };
        let inputs = WasmRenderInputs {
            references: vec![reference.clone(), reference.clone()],
            resources: Vec::new(),
            citations: Vec::new(),
            generated_bibliography: None,
        };
        assert!(normalize(inputs, &limits(2), &WasmOutputLimits::default()).is_ok());

        let inputs = WasmRenderInputs {
            references: vec![reference.clone(), reference.clone(), reference],
            resources: Vec::new(),
            citations: Vec::new(),
            generated_bibliography: None,
        };
        let error = normalize(inputs, &limits(2), &WasmOutputLimits::default()).unwrap_err();
        assert_eq!(error.code, "limit-exceeded");
    }

    #[test]
    fn byte_limit_accepts_the_exact_boundary_and_rejects_one_more() {
        let inputs = |href: &str| WasmRenderInputs {
            references: vec![WasmResolvedReference {
                source_start: 0,
                source_end: 0,
                outcome: WasmReferenceOutcome::Resolved {
                    href: href.to_owned(),
                    display_text: Some("xy".to_owned()),
                    notices: Vec::new(),
                },
            }],
            resources: Vec::new(),
            citations: Vec::new(),
            generated_bibliography: None,
        };
        assert!(normalize(inputs("abc"), &limits(1), &output_limits(5)).is_ok());
        let error = normalize(inputs("abcd"), &limits(1), &output_limits(5)).unwrap_err();
        assert_eq!(error.code, "limit-exceeded");
    }

    #[test]
    fn bibliography_entries_share_the_count_and_output_budgets() {
        let inputs = |text: &str| WasmRenderInputs {
            generated_bibliography: Some(WasmGeneratedBibliography {
                title: "R".to_owned(),
                entries: vec![WasmGeneratedBibliographyEntry {
                    citation_key: "k".to_owned(),
                    text: text.to_owned(),
                    label: Some("l".to_owned()),
                    number: Some(1),
                }],
            }),
            ..WasmRenderInputs::default()
        };

        assert!(normalize(inputs("x"), &limits(1), &output_limits(4)).is_ok());
        assert_eq!(
            normalize(inputs("xx"), &limits(1), &output_limits(4))
                .expect_err("one extra byte exceeds the output limit")
                .code,
            "limit-exceeded"
        );
        assert_eq!(
            normalize(inputs("x"), &limits(0), &output_limits(4))
                .expect_err("one bibliography entry consumes one input slot")
                .code,
            "limit-exceeded"
        );
    }

    #[test]
    fn duplicates_and_failed_outcomes_are_preserved() {
        let reference = WasmResolvedReference {
            source_start: 1,
            source_end: 2,
            outcome: WasmReferenceOutcome::Failed {
                kind: WasmReferenceFailureKind::MissingAnchor,
            },
        };
        let resource = WasmResolvedResource {
            source_start: 3,
            source_end: 4,
            outcome: WasmResourceOutcome::Failed {
                kind: WasmResourceFailureKind::PermissionDenied,
            },
        };
        let normalized = normalize(
            WasmRenderInputs {
                references: vec![reference.clone(), reference],
                resources: vec![resource.clone(), resource],
                citations: Vec::new(),
                generated_bibliography: None,
            },
            &limits(4),
            &WasmOutputLimits::default(),
        )
        .unwrap()
        .into_wire();

        assert_eq!(normalized.references.len(), 2);
        assert_eq!(normalized.resources.len(), 2);
        assert!(matches!(
            normalized.references[0].outcome,
            WasmReferenceOutcome::Failed {
                kind: WasmReferenceFailureKind::MissingAnchor
            }
        ));
        assert!(matches!(
            normalized.resources[0].outcome,
            WasmResourceOutcome::Failed {
                kind: WasmResourceFailureKind::PermissionDenied
            }
        ));
    }

    #[test]
    fn resource_byte_length_rejects_values_beyond_the_schema_safe_integer() {
        let inputs = |byte_length| WasmRenderInputs {
            references: Vec::new(),
            resources: vec![WasmResolvedResource {
                source_start: 0,
                source_end: 0,
                outcome: WasmResourceOutcome::Resolved {
                    href: String::new(),
                    media_type: "image/png".to_owned(),
                    byte_length: Some(byte_length),
                },
            }],
            citations: Vec::new(),
            generated_bibliography: None,
        };
        assert!(
            normalize(
                inputs(MAX_SAFE_INTEGER),
                &limits(1),
                &WasmOutputLimits::default()
            )
            .is_ok()
        );
        let error = normalize(
            inputs(MAX_SAFE_INTEGER + 1),
            &limits(1),
            &WasmOutputLimits::default(),
        )
        .unwrap_err();
        assert_eq!(error.code, "invalid-render-input");
    }
}
