use adocweave::Analysis;
use serde::Deserialize;

use crate::{WasmError, WasmLimits};

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WasmRenderInputs {
    #[serde(default)]
    pub references: Vec<WasmResolvedReference>,
    #[serde(default)]
    pub resources: Vec<WasmResolvedResource>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WasmResolvedReference {
    pub source_start: u32,
    pub source_end: u32,
    pub outcome: WasmReferenceOutcome,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(tag = "status", rename_all = "kebab-case", deny_unknown_fields)]
pub enum WasmReferenceOutcome {
    Resolved {
        href: String,
        #[serde(default, rename = "displayText")]
        display_text: Option<String>,
        #[serde(default)]
        notices: Vec<WasmReferenceNotice>,
    },
    Failed {
        kind: WasmReferenceFailureKind,
        message: String,
    },
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "kebab-case")]
pub enum WasmReferenceNotice {
    Fallback,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "kebab-case")]
pub enum WasmReferenceFailureKind {
    MissingTarget,
    MissingAnchor,
    AmbiguousTarget,
    OutsideRoot,
    ResolverFailure,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WasmResolvedResource {
    pub source_start: u32,
    pub source_end: u32,
    pub outcome: WasmResourceOutcome,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(tag = "status", rename_all = "kebab-case", deny_unknown_fields)]
pub enum WasmResourceOutcome {
    Resolved {
        href: String,
        #[serde(rename = "mediaType")]
        media_type: Option<String>,
        #[serde(rename = "byteLength")]
        byte_length: Option<u64>,
    },
    Failed {
        kind: WasmResourceFailureKind,
        message: String,
    },
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "kebab-case")]
pub enum WasmResourceFailureKind {
    Missing,
    OutsideRoot,
    SchemeDenied,
    PermissionDenied,
    ResolverFailure,
}

pub(crate) fn validate(inputs: &WasmRenderInputs, limits: &WasmLimits) -> Result<(), WasmError> {
    let count = inputs.references.len() as u64 + inputs.resources.len() as u64;
    if count > u64::from(limits.max_references) {
        return Err(limit_error("render input count"));
    }
    let reference_bytes = inputs.references.iter().map(|input| match &input.outcome {
        WasmReferenceOutcome::Resolved {
            href, display_text, ..
        } => href.len() as u64 + display_text.as_ref().map_or(0, |text| text.len()) as u64,
        WasmReferenceOutcome::Failed { message, .. } => message.len() as u64,
    });
    let resource_bytes = inputs.resources.iter().map(|input| match &input.outcome {
        WasmResourceOutcome::Resolved {
            href, media_type, ..
        } => href.len() as u64 + media_type.as_ref().map_or(0, |value| value.len() as u64),
        WasmResourceOutcome::Failed { message, .. } => message.len() as u64,
    });
    let bytes = reference_bytes
        .chain(resource_bytes)
        .fold(0_u64, u64::saturating_add);
    if bytes > u64::from(limits.max_output_bytes) {
        return Err(limit_error("render input bytes"));
    }
    Ok(())
}

pub(crate) fn convert(
    inputs: WasmRenderInputs,
    analysis: &Analysis,
) -> Result<adocweave::render::RenderInputs, WasmError> {
    let references = inputs
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
                        adocweave::reference::ResolvedReference::resolved(range, href)
                            .with_notices(
                                notices
                                    .into_iter()
                                    .map(|notice| adocweave::reference::ResolutionNotice {
                                        kind: match notice {
                                            WasmReferenceNotice::Fallback => {
                                                adocweave::reference::ResolutionNoticeKind::Fallback
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
                WasmReferenceOutcome::Failed { kind, message } => {
                    adocweave::reference::ResolvedReference::failed(
                        range,
                        adocweave::reference::ResolverFailure {
                            kind: match kind {
                                WasmReferenceFailureKind::MissingTarget => {
                                    adocweave::reference::ResolutionFailureKind::MissingTarget
                                }
                                WasmReferenceFailureKind::MissingAnchor => {
                                    adocweave::reference::ResolutionFailureKind::MissingAnchor
                                }
                                WasmReferenceFailureKind::AmbiguousTarget => {
                                    adocweave::reference::ResolutionFailureKind::AmbiguousTarget
                                }
                                WasmReferenceFailureKind::OutsideRoot => {
                                    adocweave::reference::ResolutionFailureKind::OutsideRoot
                                }
                                WasmReferenceFailureKind::ResolverFailure => {
                                    adocweave::reference::ResolutionFailureKind::ResolverFailure
                                }
                            },
                            message,
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
                } => adocweave::resource::ResolvedResource::resolved(
                    range,
                    href,
                    media_type,
                    byte_length,
                ),
                WasmResourceOutcome::Failed { kind, message } => {
                    adocweave::resource::ResolvedResource::failed(
                        range,
                        adocweave::resource::ResourceFailure {
                            kind: match kind {
                                WasmResourceFailureKind::Missing => {
                                    adocweave::resource::ResourceFailureKind::Missing
                                }
                                WasmResourceFailureKind::OutsideRoot => {
                                    adocweave::resource::ResourceFailureKind::OutsideRoot
                                }
                                WasmResourceFailureKind::SchemeDenied => {
                                    adocweave::resource::ResourceFailureKind::SchemeDenied
                                }
                                WasmResourceFailureKind::PermissionDenied => {
                                    adocweave::resource::ResourceFailureKind::PermissionDenied
                                }
                                WasmResourceFailureKind::ResolverFailure => {
                                    adocweave::resource::ResourceFailureKind::ResolverFailure
                                }
                            },
                            message,
                        },
                    )
                }
            })
        })
        .collect::<Result<Vec<_>, WasmError>>()?;
    Ok(adocweave::render::RenderInputs::new(references, resources))
}

fn source_range(
    start: u32,
    end: u32,
    analysis: &Analysis,
) -> Result<adocweave::source::TextRange, WasmError> {
    let start = adocweave::source::TextSize::new(start as usize).map_err(|_| invalid_input())?;
    let end = adocweave::source::TextSize::new(end as usize).map_err(|_| invalid_input())?;
    let range = adocweave::source::TextRange::new(start, end).map_err(|_| invalid_input())?;
    analysis
        .source_document()
        .text(range)
        .ok_or_else(invalid_input)?;
    Ok(range)
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
        message: "render input range is not a valid source byte range".to_owned(),
    }
}
