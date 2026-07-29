//! Analysis-dependent conversion from normalized render inputs to core values.

use adocweave::Analysis;

use crate::WasmError;
use crate::render_input_normalization::NormalizedRenderInputs;
use crate::render_input_wire::{
    WasmReferenceFailureKind, WasmReferenceNotice, WasmReferenceOutcome, WasmResourceFailureKind,
    WasmResourceOutcome,
};

pub(crate) fn convert(
    inputs: NormalizedRenderInputs,
    analysis: &Analysis,
) -> Result<adocweave::resolution::RenderInputs, WasmError> {
    let inputs = inputs.into_wire();
    let references =
        inputs
            .references
            .into_iter()
            .map(|resolution| {
                let range = source_range(resolution.source_start, resolution.source_end, analysis)?;
                Ok(match resolution.outcome {
                WasmReferenceOutcome::Resolved {
                    href,
                    display_text,
                    notices,
                } => {
                    let mut resolved =
                        adocweave::resolution::ResolvedReference::resolved(range, href)
                            .with_notices(
                            notices
                                .into_iter()
                                .map(|notice| adocweave::resolution::ResolutionNotice {
                                    kind: match notice {
                                        WasmReferenceNotice::Fallback => {
                                            adocweave::resolution::ResolutionNoticeKind::Fallback
                                        }
                                    },
                                })
                                .collect(),
                        );
                    if let Some(display_text) = display_text {
                        resolved = resolved.with_display_text(display_text);
                    }
                    resolved
                }
                WasmReferenceOutcome::Failed { kind } => {
                    adocweave::resolution::ResolvedReference::failed(
                        range,
                        adocweave::resolution::ResolverFailure {
                            kind: match kind {
                                WasmReferenceFailureKind::MissingTarget => {
                                    adocweave::resolution::ResolutionFailureKind::MissingTarget
                                }
                                WasmReferenceFailureKind::MissingAnchor => {
                                    adocweave::resolution::ResolutionFailureKind::MissingAnchor
                                }
                                WasmReferenceFailureKind::AmbiguousTarget => {
                                    adocweave::resolution::ResolutionFailureKind::AmbiguousTarget
                                }
                                WasmReferenceFailureKind::OutsideRoot => {
                                    adocweave::resolution::ResolutionFailureKind::OutsideRoot
                                }
                                WasmReferenceFailureKind::ResolverFailure => {
                                    adocweave::resolution::ResolutionFailureKind::ResolverFailure
                                }
                            },
                        },
                    )
                }
            })
            })
            .collect::<Result<Vec<_>, WasmError>>()?;
    let resources = inputs
        .resources
        .into_iter()
        .map(|resolution| {
            let range = source_range(resolution.source_start, resolution.source_end, analysis)?;
            Ok(match resolution.outcome {
                WasmResourceOutcome::Resolved {
                    href,
                    media_type,
                    byte_length,
                } => adocweave::resolution::ResolvedResource::resolved(
                    range,
                    href,
                    adocweave::resolution::MediaType::parse(&media_type)
                        .map_err(|_| invalid_input())?,
                    byte_length,
                ),
                WasmResourceOutcome::Failed { kind } => {
                    adocweave::resolution::ResolvedResource::failed(
                        range,
                        adocweave::resolution::ResourceFailure {
                            kind: match kind {
                                WasmResourceFailureKind::Missing => {
                                    adocweave::resolution::ResourceFailureKind::Missing
                                }
                                WasmResourceFailureKind::OutsideRoot => {
                                    adocweave::resolution::ResourceFailureKind::OutsideRoot
                                }
                                WasmResourceFailureKind::SchemeDenied => {
                                    adocweave::resolution::ResourceFailureKind::SchemeDenied
                                }
                                WasmResourceFailureKind::PermissionDenied => {
                                    adocweave::resolution::ResourceFailureKind::PermissionDenied
                                }
                                WasmResourceFailureKind::MediaTypeUnavailable => {
                                    adocweave::resolution::ResourceFailureKind::MediaTypeUnavailable
                                }
                                WasmResourceFailureKind::ResolverFailure => {
                                    adocweave::resolution::ResourceFailureKind::ResolverFailure
                                }
                            },
                        },
                    )
                }
            })
        })
        .collect::<Result<Vec<_>, WasmError>>()?;
    Ok(adocweave::resolution::RenderInputs::new(
        references, resources,
    ))
}

fn source_range(
    start: u32,
    end: u32,
    analysis: &Analysis,
) -> Result<adocweave::text::TextRange, WasmError> {
    let start = adocweave::text::TextSize::new(start as usize).map_err(|_| invalid_input())?;
    let end = adocweave::text::TextSize::new(end as usize).map_err(|_| invalid_input())?;
    let range = adocweave::text::TextRange::new(start, end).map_err(|_| invalid_input())?;
    analysis
        .source_document()
        .text(range)
        .ok_or_else(invalid_input)?;
    Ok(range)
}

fn invalid_input() -> WasmError {
    WasmError {
        code: "invalid-render-input".to_owned(),
        message: "render input is invalid".to_owned(),
    }
}
