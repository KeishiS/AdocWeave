//! Versioned, allocation-owning WASM boundary over the deterministic core.

use std::collections::{BTreeMap, BTreeSet};

use adocweave::conformance::{CONFORMANCE_CONTRACT_VERSION, snapshot};
use adocweave::html::RenderPolicy;
use adocweave::limits::{ProcessingLimits, SyntaxMode};
use adocweave::preprocessor::{
    PreprocessOptions, ResourceDocument, ResourceSnapshot, SafeMode, preprocess,
};
use adocweave::url::UrlPolicy;
use adocweave::{CancellationCheck, Engine, NeverCancel, ParseError, ParseOptions, SourceId};
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const WASM_API_VERSION: u16 = 13;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WasmPreprocessRequest {
    pub api_version: u16,
    pub source_id: Option<String>,
    pub source: String,
    #[serde(default)]
    pub resources: BTreeMap<String, WasmResource>,
    #[serde(default)]
    pub options: WasmPreprocessOptions,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WasmResource {
    pub source_id: String,
    pub source: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(default, rename_all = "camelCase", deny_unknown_fields)]
pub struct WasmPreprocessOptions {
    pub base_uri: Option<String>,
    pub safe_mode: WasmSafeMode,
    pub allowed_schemes: BTreeSet<String>,
    pub attributes: BTreeMap<String, String>,
    pub max_include_depth: u32,
    pub max_includes: u32,
    pub max_total_bytes: u32,
    pub max_expanded_nodes: u32,
    pub max_source_map_segments: u32,
}

impl Default for WasmPreprocessOptions {
    fn default() -> Self {
        let options = PreprocessOptions::default();
        Self {
            base_uri: options.base_uri,
            safe_mode: WasmSafeMode::Secure,
            allowed_schemes: options.allowed_schemes,
            attributes: options.attributes,
            max_include_depth: options.max_include_depth,
            max_includes: options.max_includes,
            max_total_bytes: options.max_total_bytes,
            max_expanded_nodes: options.max_expanded_nodes,
            max_source_map_segments: options.max_source_map_segments,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "kebab-case")]
pub enum WasmSafeMode {
    Unsafe,
    Server,
    Safe,
    #[default]
    Secure,
}

#[derive(Clone, Debug, Serialize, Eq, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct WasmPreprocessResponse {
    pub api_version: u16,
    pub source: String,
    pub source_map: Vec<WasmSourceMapSegment>,
}

#[derive(Clone, Debug, Serialize, Eq, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct WasmSourceMapSegment {
    pub output_start: u32,
    pub output_end: u32,
    pub source_id: Option<String>,
    pub source_start: u32,
    pub source_end: u32,
    pub mapping: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WasmRequest {
    pub api_version: u16,
    pub source_id: Option<String>,
    pub version: u32,
    pub generation: u32,
    pub source: String,
    #[serde(default)]
    pub render_inputs: WasmRenderInputs,
    #[serde(default)]
    pub options: WasmOptions,
}

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
    },
    Failed {
        kind: WasmReferenceFailureKind,
        message: String,
    },
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

#[derive(Clone, Debug, Deserialize, Serialize, Eq, PartialEq)]
#[serde(default, rename_all = "camelCase", deny_unknown_fields)]
pub struct WasmOptions {
    pub syntax_mode: WasmSyntaxMode,
    pub limits: WasmLimits,
    pub protected_attributes: BTreeMap<String, String>,
    pub url_policy: WasmUrlPolicy,
}

impl Default for WasmOptions {
    fn default() -> Self {
        Self {
            syntax_mode: WasmSyntaxMode::Permissive,
            limits: WasmLimits::default(),
            protected_attributes: BTreeMap::new(),
            url_policy: WasmUrlPolicy::default(),
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, Eq, PartialEq)]
#[serde(rename_all = "kebab-case")]
pub enum WasmSyntaxMode {
    Permissive,
    Strict,
}

#[derive(Clone, Debug, Deserialize, Serialize, Eq, PartialEq)]
#[serde(default, rename_all = "camelCase", deny_unknown_fields)]
pub struct WasmLimits {
    pub max_input_bytes: u32,
    pub max_output_bytes: u32,
    pub max_line_bytes: u32,
    pub max_list_depth: u32,
    pub max_list_continuations: u32,
    pub max_block_depth: u32,
    pub max_inline_depth: u32,
    pub max_formula_bytes: u32,
    pub max_table_bytes: u32,
    pub max_table_cells: u32,
    pub max_table_columns: u32,
    pub max_table_depth: u32,
    pub max_catalog_entries: u32,
    pub max_catalog_bytes: u32,
    pub max_blocks: u32,
    pub max_nodes: u32,
    pub max_references: u32,
    pub max_attributes: u32,
    pub max_attribute_expansion_depth: u32,
    pub max_attribute_expansion_bytes: u32,
    pub max_diagnostics: u32,
}

impl Default for WasmLimits {
    fn default() -> Self {
        ProcessingLimits::default().into()
    }
}

impl From<ProcessingLimits> for WasmLimits {
    fn from(value: ProcessingLimits) -> Self {
        Self {
            max_input_bytes: value.max_input_bytes,
            max_output_bytes: value.max_output_bytes,
            max_line_bytes: value.max_line_bytes,
            max_list_depth: value.max_list_depth,
            max_list_continuations: value.max_list_continuations,
            max_block_depth: value.max_block_depth,
            max_inline_depth: value.max_inline_depth,
            max_formula_bytes: value.max_formula_bytes,
            max_table_bytes: value.max_table_bytes,
            max_table_cells: value.max_table_cells,
            max_table_columns: value.max_table_columns,
            max_table_depth: value.max_table_depth,
            max_catalog_entries: value.max_catalog_entries,
            max_catalog_bytes: value.max_catalog_bytes,
            max_blocks: value.max_blocks,
            max_nodes: value.max_nodes,
            max_references: value.max_references,
            max_attributes: value.max_attributes,
            max_attribute_expansion_depth: value.max_attribute_expansion_depth,
            max_attribute_expansion_bytes: value.max_attribute_expansion_bytes,
            max_diagnostics: value.max_diagnostics,
        }
    }
}

impl From<WasmLimits> for ProcessingLimits {
    fn from(value: WasmLimits) -> Self {
        Self {
            max_input_bytes: value.max_input_bytes,
            max_output_bytes: value.max_output_bytes,
            max_line_bytes: value.max_line_bytes,
            max_list_depth: value.max_list_depth,
            max_list_continuations: value.max_list_continuations,
            max_block_depth: value.max_block_depth,
            max_inline_depth: value.max_inline_depth,
            max_formula_bytes: value.max_formula_bytes,
            max_table_bytes: value.max_table_bytes,
            max_table_cells: value.max_table_cells,
            max_table_columns: value.max_table_columns,
            max_table_depth: value.max_table_depth,
            max_catalog_entries: value.max_catalog_entries,
            max_catalog_bytes: value.max_catalog_bytes,
            max_blocks: value.max_blocks,
            max_nodes: value.max_nodes,
            max_references: value.max_references,
            max_attributes: value.max_attributes,
            max_attribute_expansion_depth: value.max_attribute_expansion_depth,
            max_attribute_expansion_bytes: value.max_attribute_expansion_bytes,
            max_diagnostics: value.max_diagnostics,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, Eq, PartialEq)]
#[serde(default, rename_all = "camelCase", deny_unknown_fields)]
pub struct WasmUrlPolicy {
    pub allowed_schemes: Vec<String>,
    pub allow_relative: bool,
    pub allow_data_uris: bool,
}

impl Default for WasmUrlPolicy {
    fn default() -> Self {
        let policy = UrlPolicy::default();
        Self {
            allowed_schemes: policy.allowed_schemes.into_iter().collect(),
            allow_relative: policy.allow_relative,
            allow_data_uris: policy.allow_data_uris,
        }
    }
}

#[derive(Clone, Debug, Serialize, Eq, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct WasmResponse {
    pub api_version: u16,
    pub version: u32,
    pub generation: u32,
    pub conformance_contract_version: u16,
    pub parse: ParseSummary,
    pub syntax: String,
    pub ast: String,
    pub html: String,
    pub diagnostics: Value,
    pub render_diagnostics: Value,
    pub symbols: Value,
    pub projection: Value,
}

#[derive(Clone, Debug, Serialize, Eq, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ParseSummary {
    pub profile_version: u16,
    pub block_count: usize,
    pub node_count: usize,
    pub reference_count: usize,
}

#[derive(Clone, Debug, Serialize, Eq, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct WasmError {
    pub code: String,
    pub message: String,
}

pub fn preprocess_request(
    request: WasmPreprocessRequest,
) -> Result<WasmPreprocessResponse, WasmError> {
    if request.api_version != WASM_API_VERSION {
        return Err(WasmError {
            code: "unsupported-api-version".to_owned(),
            message: format!(
                "unsupported WASM API version {} (expected {WASM_API_VERSION})",
                request.api_version
            ),
        });
    }
    let mut snapshot = ResourceSnapshot::default();
    for (target, resource) in request.resources {
        snapshot.insert(
            target,
            ResourceDocument {
                source_id: SourceId::new(resource.source_id),
                source: resource.source,
            },
        );
    }
    let options = request.options;
    let document = preprocess(
        &request.source,
        &snapshot,
        &PreprocessOptions {
            source_id: request.source_id.map(SourceId::new),
            base_uri: options.base_uri,
            safe_mode: match options.safe_mode {
                WasmSafeMode::Unsafe => SafeMode::Unsafe,
                WasmSafeMode::Server => SafeMode::Server,
                WasmSafeMode::Safe => SafeMode::Safe,
                WasmSafeMode::Secure => SafeMode::Secure,
            },
            allowed_schemes: options
                .allowed_schemes
                .into_iter()
                .map(|scheme| scheme.to_ascii_lowercase())
                .collect(),
            attributes: options.attributes,
            max_include_depth: options.max_include_depth,
            max_includes: options.max_includes,
            max_total_bytes: options.max_total_bytes,
            max_expanded_nodes: options.max_expanded_nodes,
            max_source_map_segments: options.max_source_map_segments,
        },
    )
    .map_err(|error| WasmError {
        code: error.kind.as_str().to_owned(),
        message: error.to_string(),
    })?;
    let source_map = document
        .source_map()
        .iter()
        .map(|segment| WasmSourceMapSegment {
            output_start: segment.output_range.start().to_u32(),
            output_end: segment.output_range.end().to_u32(),
            source_id: segment
                .origin
                .source_id
                .as_ref()
                .map(|source_id| source_id.as_str().to_owned()),
            source_start: segment.origin.range.start().to_u32(),
            source_end: segment.origin.range.end().to_u32(),
            mapping: match segment.mapping {
                adocweave::preprocessor::SourceMapping::Identity => "identity",
                adocweave::preprocessor::SourceMapping::WholeOrigin => "whole-origin",
            }
            .to_owned(),
        })
        .collect();
    Ok(WasmPreprocessResponse {
        api_version: WASM_API_VERSION,
        source: document.source,
        source_map,
    })
}

pub fn process_request(
    request: WasmRequest,
    cancellation: &dyn CancellationCheck,
) -> Result<WasmResponse, WasmError> {
    if request.api_version != WASM_API_VERSION {
        return Err(WasmError {
            code: "unsupported-api-version".to_owned(),
            message: format!(
                "unsupported WASM API version {} (expected {WASM_API_VERSION})",
                request.api_version
            ),
        });
    }
    let render_inputs = request.render_inputs;
    let options = request.options;
    validate_wasm_render_inputs(&render_inputs, &options.limits)?;
    let max_output_bytes = usize::try_from(options.limits.max_output_bytes)
        .expect("u32 fits usize on supported targets");
    let analysis = Engine::new(ParseOptions {
        source_id: request.source_id.map(SourceId::new),
        syntax_mode: match options.syntax_mode {
            WasmSyntaxMode::Permissive => SyntaxMode::Permissive,
            WasmSyntaxMode::Strict => SyntaxMode::Strict,
        },
        limits: options.limits.into(),
        protected_attributes: options.protected_attributes,
        url_policy: UrlPolicy {
            allowed_schemes: options
                .url_policy
                .allowed_schemes
                .into_iter()
                .map(|scheme| scheme.to_ascii_lowercase())
                .collect::<BTreeSet<_>>(),
            allow_relative: options.url_policy.allow_relative,
            allow_data_uris: options.url_policy.allow_data_uris,
        },
    })
    .analyze_cancellable(&request.source, cancellation)
    .map_err(wasm_error)?;
    if cancellation.is_cancelled() {
        return Err(cancelled_error());
    }

    let render_inputs = convert_render_inputs(render_inputs, &analysis)?;
    let products = snapshot(&analysis, &RenderPolicy::default(), &render_inputs);
    let diagnostics =
        serde_json::from_str(&products.diagnostics_json).map_err(serialization_error)?;
    let render_diagnostics =
        serde_json::from_str(&products.render_diagnostics_json).map_err(serialization_error)?;
    let symbols = serde_json::from_str(&products.symbols_json).map_err(serialization_error)?;
    let projection =
        serde_json::from_str(&products.projection_json).map_err(serialization_error)?;
    if cancellation.is_cancelled() {
        return Err(cancelled_error());
    }

    let response = WasmResponse {
        api_version: WASM_API_VERSION,
        version: request.version,
        generation: request.generation,
        conformance_contract_version: CONFORMANCE_CONTRACT_VERSION,
        parse: ParseSummary {
            profile_version: analysis.profile_version(),
            block_count: analysis.ast().blocks().len(),
            node_count: analysis.ast().node_count(),
            reference_count: analysis.references().len(),
        },
        syntax: products.syntax,
        ast: products.ast,
        html: products.html,
        diagnostics,
        render_diagnostics,
        symbols,
        projection,
    };
    let output_bytes = serde_json::to_vec(&response)
        .map_err(serialization_error)?
        .len();
    if output_bytes > max_output_bytes {
        return Err(WasmError {
            code: "limit-exceeded".to_owned(),
            message: format!(
                "output bytes limit exceeded (limit {max_output_bytes}, actual {output_bytes})"
            ),
        });
    }
    Ok(response)
}

fn validate_wasm_render_inputs(
    inputs: &WasmRenderInputs,
    limits: &WasmLimits,
) -> Result<(), WasmError> {
    let count = inputs.references.len() as u64 + inputs.resources.len() as u64;
    if count > u64::from(limits.max_references) {
        return Err(render_input_limit_error("render input count"));
    }
    let mut bytes = 0_u64;
    for input in &inputs.references {
        bytes = bytes.saturating_add(match &input.outcome {
            WasmReferenceOutcome::Resolved { href } => href.len() as u64,
            WasmReferenceOutcome::Failed { message, .. } => message.len() as u64,
        });
    }
    for input in &inputs.resources {
        bytes = bytes.saturating_add(match &input.outcome {
            WasmResourceOutcome::Resolved {
                href, media_type, ..
            } => {
                href.len() as u64
                    + media_type
                        .as_ref()
                        .map_or(0, |media_type| media_type.len() as u64)
            }
            WasmResourceOutcome::Failed { message, .. } => message.len() as u64,
        });
    }
    if bytes > u64::from(limits.max_output_bytes) {
        return Err(render_input_limit_error("render input bytes"));
    }
    Ok(())
}

fn render_input_limit_error(resource: &str) -> WasmError {
    WasmError {
        code: "limit-exceeded".to_owned(),
        message: format!("{resource} exceeds the configured processing limit"),
    }
}

fn convert_render_inputs(
    inputs: WasmRenderInputs,
    analysis: &adocweave::Analysis,
) -> Result<adocweave::render::RenderInputs, WasmError> {
    let references = inputs
        .references
        .into_iter()
        .map(|resolution| {
            let range =
                render_input_range(resolution.source_start, resolution.source_end, analysis)?;
            Ok(match resolution.outcome {
                WasmReferenceOutcome::Resolved { href } => {
                    adocweave::reference::ResolvedReference::resolved(range, href)
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
            let range =
                render_input_range(resolution.source_start, resolution.source_end, analysis)?;
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

fn render_input_range(
    start: u32,
    end: u32,
    analysis: &adocweave::Analysis,
) -> Result<adocweave::source::TextRange, WasmError> {
    let start =
        adocweave::source::TextSize::new(start as usize).map_err(|_| invalid_render_input())?;
    let end = adocweave::source::TextSize::new(end as usize).map_err(|_| invalid_render_input())?;
    let range =
        adocweave::source::TextRange::new(start, end).map_err(|_| invalid_render_input())?;
    analysis
        .source_document()
        .text(range)
        .ok_or_else(invalid_render_input)?;
    Ok(range)
}

fn invalid_render_input() -> WasmError {
    WasmError {
        code: "invalid-render-input".to_owned(),
        message: "render input range is not a valid source byte range".to_owned(),
    }
}

pub fn process_json(request: &str) -> Result<String, String> {
    let request = serde_json::from_str(request).map_err(|error| {
        serialize_error(&WasmError {
            code: "invalid-request".to_owned(),
            message: error.to_string(),
        })
    })?;
    process_request(request, &NeverCancel)
        .and_then(|response| serde_json::to_string(&response).map_err(serialization_error))
        .map_err(|error| serialize_error(&error))
}

fn wasm_error(error: ParseError) -> WasmError {
    WasmError {
        code: error.code().as_str().to_owned(),
        message: error.to_string(),
    }
}

fn cancelled_error() -> WasmError {
    WasmError {
        code: "cancelled".to_owned(),
        message: "operation was cancelled".to_owned(),
    }
}

fn serialization_error(error: impl ToString) -> WasmError {
    WasmError {
        code: "serialization-failed".to_owned(),
        message: error.to_string(),
    }
}

fn serialize_error(error: &WasmError) -> String {
    serde_json::to_string(error).unwrap_or_else(|_| {
        "{\"code\":\"serialization-failed\",\"message\":\"failed to serialize error\"}".to_owned()
    })
}

#[cfg(target_arch = "wasm32")]
mod bindings {
    use js_sys::Function;
    use wasm_bindgen::prelude::*;

    use super::*;

    struct JsCancellation(Option<Function>);

    impl CancellationCheck for JsCancellation {
        fn is_cancelled(&self) -> bool {
            self.0.as_ref().is_some_and(|callback| {
                callback
                    .call0(&JsValue::NULL)
                    .ok()
                    .and_then(|value| value.as_bool())
                    .unwrap_or(true)
            })
        }
    }

    #[wasm_bindgen(js_name = process)]
    pub fn process_js(
        request: JsValue,
        cancellation: Option<Function>,
    ) -> Result<JsValue, JsValue> {
        let request = serde_wasm_bindgen::from_value(request).map_err(|error| {
            JsValue::from_str(&serialize_error(&WasmError {
                code: "invalid-request".to_owned(),
                message: error.to_string(),
            }))
        })?;
        let response = process_request(request, &JsCancellation(cancellation))
            .map_err(|error| JsValue::from_str(&serialize_error(&error)))?;
        response
            .serialize(&serde_wasm_bindgen::Serializer::json_compatible())
            .map_err(|error| JsValue::from_str(&serialize_error(&serialization_error(error))))
    }

    #[wasm_bindgen(js_name = preprocess)]
    pub fn preprocess_js(request: JsValue) -> Result<JsValue, JsValue> {
        let request = serde_wasm_bindgen::from_value(request).map_err(|error| {
            JsValue::from_str(&serialize_error(&WasmError {
                code: "invalid-request".to_owned(),
                message: error.to_string(),
            }))
        })?;
        let response = preprocess_request(request)
            .map_err(|error| JsValue::from_str(&serialize_error(&error)))?;
        response
            .serialize(&serde_wasm_bindgen::Serializer::json_compatible())
            .map_err(|error| JsValue::from_str(&serialize_error(&serialization_error(error))))
    }
}

#[cfg(test)]
mod tests {
    use adocweave::CancellationToken;
    use serde_json::json;

    use super::*;

    fn request(source: &str) -> WasmRequest {
        WasmRequest {
            api_version: WASM_API_VERSION,
            source_id: Some("web:document".to_owned()),
            version: 3,
            generation: 7,
            source: source.to_owned(),
            render_inputs: WasmRenderInputs::default(),
            options: WasmOptions::default(),
        }
    }

    #[test]
    fn wasm_api_returns_all_products_from_one_versioned_request() {
        let response =
            process_request(request("= Title\n\n== Section\n"), &NeverCancel).expect("response");

        assert_eq!(response.version, 3);
        assert_eq!(response.generation, 7);
        assert_eq!(
            response.conformance_contract_version,
            CONFORMANCE_CONTRACT_VERSION
        );
        assert!(response.syntax.contains("Document@"));
        assert!(response.ast.contains("\"blocks\""));
        assert!(response.html.contains("<h1"));
        assert_eq!(response.symbols[0]["name"], "Title");
        assert_eq!(
            response.projection["contractVersion"],
            adocweave::projection::PROJECTION_CONTRACT_VERSION
        );
        assert_eq!(response.parse.reference_count, 0);
    }

    #[test]
    fn wasm_api_accepts_the_same_resolved_render_inputs_as_native() {
        let source = "image:https://source.example/image.png[alt]";
        let mut resolved_request = request(source);
        resolved_request
            .render_inputs
            .resources
            .push(WasmResolvedResource {
                source_start: 0,
                source_end: source.len() as u32,
                outcome: WasmResourceOutcome::Resolved {
                    href: "https://cdn.example/image.png".to_owned(),
                    media_type: Some("image/png".to_owned()),
                    byte_length: Some(42),
                },
            });

        let response = process_request(resolved_request, &NeverCancel).expect("response");
        assert_eq!(
            response.html,
            "<p><img src=\"https://cdn.example/image.png\" alt=\"alt\"></p>\n"
        );
        assert_eq!(response.render_diagnostics, json!([]));

        let mut unsafe_request = request(source);
        unsafe_request
            .render_inputs
            .resources
            .push(WasmResolvedResource {
                source_start: 0,
                source_end: source.len() as u32,
                outcome: WasmResourceOutcome::Resolved {
                    href: "javascript:alert(1)".to_owned(),
                    media_type: None,
                    byte_length: None,
                },
            });
        let unsafe_response = process_request(unsafe_request, &NeverCancel).expect("response");
        assert_eq!(unsafe_response.html, "<p>alt</p>\n");
        assert_eq!(
            unsafe_response.render_diagnostics[0]["code"],
            "invalid-url-scheme"
        );

        let mut limited = request(source);
        limited.options.limits.max_references = 0;
        limited.render_inputs.resources.push(WasmResolvedResource {
            source_start: 0,
            source_end: source.len() as u32,
            outcome: WasmResourceOutcome::Resolved {
                href: "https://cdn.example/image.png".to_owned(),
                media_type: None,
                byte_length: None,
            },
        });
        let error = process_request(limited, &NeverCancel).expect_err("render input limit");
        assert_eq!(error.code, "limit-exceeded");

        let mut invalid = request(source);
        invalid.render_inputs.resources.push(WasmResolvedResource {
            source_start: 0,
            source_end: source.len() as u32 + 1,
            outcome: WasmResourceOutcome::Resolved {
                href: "https://cdn.example/image.png".to_owned(),
                media_type: None,
                byte_length: None,
            },
        });
        let error = process_request(invalid, &NeverCancel).expect_err("outside source");
        assert_eq!(error.code, "invalid-render-input");
    }

    #[test]
    fn wasm_api_rejects_unknown_fields_and_versions() {
        let invalid = json!({
            "apiVersion": WASM_API_VERSION,
            "sourceId": null,
            "version": 1,
            "generation": 1,
            "source": "text",
            "unexpected": true
        })
        .to_string();
        let error = process_json(&invalid).expect_err("invalid request");
        assert!(error.contains("invalid-request"));

        let error = process_request(
            WasmRequest {
                api_version: WASM_API_VERSION + 1,
                ..request("text")
            },
            &NeverCancel,
        )
        .expect_err("unsupported version");
        assert_eq!(error.code, "unsupported-api-version");
    }

    #[test]
    fn wasm_api_cancellation_uses_the_core_checkpoints() {
        let cancellation = CancellationToken::new();
        cancellation.cancel();
        let error = process_request(request("text"), &cancellation).expect_err("cancelled");
        assert_eq!(error.code, "cancelled");
    }

    #[test]
    fn wasm_api_large_input_uses_the_same_core_limit() {
        let max_input = usize::try_from(ParseOptions::default().limits.max_input_bytes)
            .expect("u32 fits usize on supported targets");
        let source = "x".repeat(max_input + 1);
        let error = process_request(request(&source), &NeverCancel).expect_err("limit");
        assert_eq!(error.code, "limit-exceeded");
    }

    #[test]
    fn wasm_options_are_partial_overrides_and_bound_the_complete_response() {
        let value = json!({
            "apiVersion": WASM_API_VERSION,
            "sourceId": null,
            "version": 1,
            "generation": 1,
            "source": "text",
            "options": {"limits": {"maxOutputBytes": 1}}
        });
        let request: WasmRequest = serde_json::from_value(value).expect("partial options");
        assert_eq!(request.options.limits.max_input_bytes, 10 * 1024 * 1024);
        let error = process_request(request, &NeverCancel).expect_err("output limit");
        assert_eq!(error.code, "limit-exceeded");
    }

    #[test]
    fn preprocessing_uses_the_same_snapshot_model_as_the_native_core() {
        let resources = BTreeMap::from([(
            "parts/intro.adoc".to_owned(),
            WasmResource {
                source_id: "intro".to_owned(),
                source: "== Intro\n".to_owned(),
            },
        )]);
        let response = preprocess_request(WasmPreprocessRequest {
            api_version: WASM_API_VERSION,
            source_id: Some("root".to_owned()),
            source: "include::intro.adoc[leveloffset=+1]\n".to_owned(),
            resources,
            options: WasmPreprocessOptions {
                base_uri: Some("parts".to_owned()),
                ..WasmPreprocessOptions::default()
            },
        })
        .expect("preprocessed response");
        assert_eq!(response.source, "=== Intro\n");
        assert_eq!(response.source_map[0].source_id.as_deref(), Some("intro"));
        assert_eq!(response.source_map[0].mapping, "whole-origin");

        let mut native_snapshot = ResourceSnapshot::default();
        native_snapshot.insert(
            "parts/intro.adoc",
            ResourceDocument {
                source_id: SourceId::new("intro"),
                source: "== Intro\n".to_owned(),
            },
        );
        let native = preprocess(
            "include::intro.adoc[leveloffset=+1]\n",
            &native_snapshot,
            &PreprocessOptions {
                base_uri: Some("parts".to_owned()),
                ..PreprocessOptions::default()
            },
        )
        .expect("native preprocessing");
        assert_eq!(response.source, native.source);
        assert_eq!(response.source_map.len(), native.source_map().len());
        assert_eq!(
            response.source_map[0].source_start,
            native.source_map()[0].origin.range.start().to_u32()
        );
        assert_eq!(
            response.source_map[0].source_end,
            native.source_map()[0].origin.range.end().to_u32()
        );
    }
}
