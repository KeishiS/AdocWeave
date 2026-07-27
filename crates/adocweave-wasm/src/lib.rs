//! Versioned, allocation-owning WASM boundary over the deterministic core.

use std::collections::{BTreeMap, BTreeSet};

use adocweave::output::diagnostics::{LintConfig, RuleSettings, Severity, lint_rule};
use adocweave::output::html::RenderPolicy;
use adocweave::preprocess::{
    PreprocessOptions, ResourceDocument, ResourceSnapshot, SafeMode, preprocess,
};
use adocweave::resolution::{ActiveUrlPolicy, AuthoredUrlPolicy};
use adocweave::{AnalysisLimits, SyntaxMode};
use adocweave::{
    AnalysisOptions, CancellationCheck, DiagnosticProfile, Engine, NeverCancel, OutputLimits,
    ParseError, SourceId, SyntaxOptions, VERSION,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;

mod protocol_generated;
mod render_inputs;
pub use protocol_generated::{PROTOCOL_SCHEMA_VERSION, WORKER_PROTOCOL_VERSION, WasmProductSet};
pub use render_inputs::{
    WasmReferenceFailureKind, WasmReferenceNotice, WasmReferenceOutcome, WasmRenderInputs,
    WasmResolvedReference, WasmResolvedResource, WasmResourceFailureKind, WasmResourceOutcome,
};

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WasmPreprocessRequest {
    pub package_version: String,
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
    pub enable_includes: bool,
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
            enable_includes: options.enable_includes,
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
    pub package_version: &'static str,
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

/// A half-open UTF-8 byte range in the submitted source.
#[derive(Clone, Copy, Debug, Serialize, Eq, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct WasmTextRange {
    pub start: u32,
    pub end: u32,
}

/// One source-preserving standard document-attribute occurrence.
#[derive(Clone, Debug, Serialize, Eq, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct WasmDocumentAttributeOccurrence {
    pub range: WasmTextRange,
    pub name_range: WasmTextRange,
    pub value_range: WasmTextRange,
    pub name: String,
    pub raw_value: String,
    pub operation: WasmDocumentAttributeOperation,
}

#[derive(Clone, Copy, Debug, Serialize, Eq, PartialEq)]
#[serde(rename_all = "kebab-case")]
pub enum WasmDocumentAttributeOperation {
    Set,
    Unset,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WasmRequest {
    pub package_version: String,
    pub source_id: Option<String>,
    pub version: u32,
    pub generation: u32,
    pub source: String,
    #[serde(default)]
    pub products: WasmProductSet,
    #[serde(default)]
    pub render_inputs: WasmRenderInputs,
    #[serde(default)]
    pub analysis_options: WasmAnalysisOptions,
    #[serde(default)]
    pub render_policy: WasmRenderPolicy,
    #[serde(default)]
    pub output_limits: WasmOutputLimits,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, Eq, PartialEq)]
#[serde(default, rename_all = "camelCase", deny_unknown_fields)]
pub struct WasmAnalysisOptions {
    pub syntax: WasmSyntaxOptions,
    pub diagnostics: WasmDiagnosticProfile,
}

#[derive(Clone, Debug, Deserialize, Serialize, Eq, PartialEq)]
#[serde(default, rename_all = "camelCase", deny_unknown_fields)]
pub struct WasmSyntaxOptions {
    pub syntax_mode: WasmSyntaxMode,
    pub limits: WasmLimits,
}

impl Default for WasmSyntaxOptions {
    fn default() -> Self {
        Self {
            syntax_mode: WasmSyntaxMode::Permissive,
            limits: WasmLimits::default(),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, Eq, PartialEq)]
#[serde(default, rename_all = "camelCase", deny_unknown_fields)]
pub struct WasmDiagnosticProfile {
    pub protected_attributes: BTreeMap<String, String>,
    pub authored_urls: WasmAuthoredUrlPolicy,
    pub max_diagnostics: u32,
    pub rules: BTreeMap<String, WasmRuleSettings>,
}

impl Default for WasmDiagnosticProfile {
    fn default() -> Self {
        Self {
            protected_attributes: BTreeMap::new(),
            authored_urls: WasmAuthoredUrlPolicy::default(),
            max_diagnostics: 1_000,
            rules: BTreeMap::new(),
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, Eq, PartialEq)]
#[serde(default, rename_all = "camelCase", deny_unknown_fields)]
pub struct WasmRuleSettings {
    pub enabled: bool,
    pub severity: WasmSeverity,
}

impl Default for WasmRuleSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            severity: WasmSeverity::Warning,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, Eq, PartialEq)]
#[serde(rename_all = "kebab-case")]
pub enum WasmSeverity {
    Error,
    #[default]
    Warning,
    Information,
    Hint,
}

#[derive(Clone, Debug, Deserialize, Serialize, Eq, PartialEq)]
#[serde(default, rename_all = "camelCase", deny_unknown_fields)]
pub struct WasmRenderPolicy {
    pub active_urls: WasmActiveUrlPolicy,
    pub external_links: WasmExternalLinkPolicy,
    pub source_languages: WasmSourceLanguagePolicy,
    pub math_languages: Vec<WasmMathLanguage>,
    pub unresolved_references: WasmUnresolvedReferencePresentation,
    pub resources: WasmResourceCapabilities,
    pub document_mode: WasmDocumentMode,
    pub stylesheets: Vec<WasmStylesheet>,
}

impl Default for WasmRenderPolicy {
    fn default() -> Self {
        Self {
            active_urls: WasmActiveUrlPolicy::default(),
            external_links: WasmExternalLinkPolicy::default(),
            source_languages: WasmSourceLanguagePolicy::default(),
            math_languages: vec![WasmMathLanguage::Latex, WasmMathLanguage::Typst],
            unresolved_references: WasmUnresolvedReferencePresentation::Target,
            resources: WasmResourceCapabilities::default(),
            document_mode: WasmDocumentMode::Fragment,
            stylesheets: Vec::new(),
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, Eq, PartialEq)]
#[serde(default, rename_all = "camelCase", deny_unknown_fields)]
pub struct WasmOutputLimits {
    pub max_output_bytes: u32,
}

impl Default for WasmOutputLimits {
    fn default() -> Self {
        Self {
            max_output_bytes: OutputLimits::default().max_output_bytes,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, Eq, PartialEq)]
#[serde(rename_all = "kebab-case")]
pub enum WasmDocumentMode {
    #[default]
    Fragment,
    Complete,
}

/// A host-supplied stylesheet forwarded to the core stylesheet policy.
/// Rejected sources surface as `renderDiagnostics`, never as emitted CSS.
#[derive(Clone, Debug, Deserialize, Serialize, Eq, PartialEq)]
#[serde(tag = "kind", rename_all = "kebab-case", deny_unknown_fields)]
pub enum WasmStylesheet {
    Inline { css: String },
    External { url: String },
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, Eq, PartialEq)]
#[serde(default, rename_all = "camelCase", deny_unknown_fields)]
pub struct WasmExternalLinkPolicy {
    pub open_in_new_context: bool,
    pub noreferrer: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize, Eq, PartialEq)]
#[serde(default, rename_all = "camelCase", deny_unknown_fields)]
pub struct WasmSourceLanguagePolicy {
    pub allowed: Option<Vec<String>>,
    pub unknown: WasmUnknownSourceLanguage,
}

impl Default for WasmSourceLanguagePolicy {
    fn default() -> Self {
        Self {
            allowed: None,
            unknown: WasmUnknownSourceLanguage::PreserveSanitized,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, Eq, PartialEq)]
#[serde(rename_all = "kebab-case")]
pub enum WasmUnknownSourceLanguage {
    #[default]
    PreserveSanitized,
    OmitClass,
    Diagnostic,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, Eq, PartialEq)]
#[serde(rename_all = "kebab-case")]
pub enum WasmMathLanguage {
    Latex,
    Typst,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, Eq, PartialEq)]
#[serde(rename_all = "kebab-case")]
pub enum WasmUnresolvedReferencePresentation {
    #[default]
    Target,
    LabelOnly,
    Hidden,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, Eq, PartialEq)]
#[serde(default, rename_all = "camelCase", deny_unknown_fields)]
pub struct WasmResourceCapabilities {
    pub images: bool,
    pub media: bool,
}

impl Default for WasmResourceCapabilities {
    fn default() -> Self {
        Self {
            images: true,
            media: true,
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
}

impl Default for WasmLimits {
    fn default() -> Self {
        AnalysisLimits::default().into()
    }
}

impl From<AnalysisLimits> for WasmLimits {
    fn from(value: AnalysisLimits) -> Self {
        Self {
            max_input_bytes: value.max_input_bytes,
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
        }
    }
}

impl From<WasmLimits> for AnalysisLimits {
    fn from(value: WasmLimits) -> Self {
        Self {
            max_input_bytes: value.max_input_bytes,
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
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, Eq, PartialEq)]
#[serde(default, rename_all = "camelCase", deny_unknown_fields)]
pub struct WasmAuthoredUrlPolicy {
    pub allowed_schemes: Vec<String>,
    pub allow_relative: bool,
}

impl Default for WasmAuthoredUrlPolicy {
    fn default() -> Self {
        let policy = AuthoredUrlPolicy::default();
        Self {
            allowed_schemes: policy.allowed_schemes.into_iter().collect(),
            allow_relative: policy.allow_relative,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, Eq, PartialEq)]
#[serde(default, rename_all = "camelCase", deny_unknown_fields)]
pub struct WasmActiveUrlPolicy {
    pub allowed_schemes: Vec<String>,
    pub allow_authored_relative: bool,
    pub allow_resolved_relative: bool,
    pub allow_resolved_root_relative: bool,
    pub allow_data_uris: bool,
}

impl Default for WasmActiveUrlPolicy {
    fn default() -> Self {
        let policy = ActiveUrlPolicy::default();
        Self {
            allowed_schemes: policy.allowed_schemes.into_iter().collect(),
            allow_authored_relative: policy.allow_authored_relative,
            allow_resolved_relative: policy.allow_resolved_relative,
            allow_resolved_root_relative: policy.allow_resolved_root_relative,
            allow_data_uris: policy.allow_data_uris,
        }
    }
}

#[derive(Clone, Debug, Serialize, Eq, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct WasmResponse {
    pub package_version: &'static str,
    pub version: u32,
    pub generation: u32,
    pub products: WasmProductSet,
    pub parse: ParseSummary,
    pub syntax: String,
    pub ast: String,
    pub html: String,
    pub attribute_occurrences: Vec<WasmDocumentAttributeOccurrence>,
    pub resource_queries: Vec<WasmResourceQuery>,
    pub diagnostics: Value,
    pub render_diagnostics: Value,
    pub symbols: Value,
    pub projection: Value,
}

#[derive(Clone, Debug, Serialize, Eq, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct WasmResourceQuery {
    pub purpose: WasmResourcePurpose,
    pub form: WasmMacroForm,
    pub owner_range: WasmTextRange,
    pub range: WasmTextRange,
    pub target_range: WasmTextRange,
    pub target: String,
}

#[derive(Clone, Copy, Debug, Serialize, Eq, PartialEq)]
#[serde(rename_all = "kebab-case")]
pub enum WasmResourcePurpose {
    Image,
    Icon,
    Audio,
    Video,
    VideoPoster,
}

#[derive(Clone, Copy, Debug, Serialize, Eq, PartialEq)]
#[serde(rename_all = "kebab-case")]
pub enum WasmMacroForm {
    Inline,
    Block,
}

#[derive(Clone, Debug, Serialize, Eq, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ParseSummary {
    pub package_version: &'static str,
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
    if request.package_version != VERSION {
        return Err(WasmError {
            code: "unsupported-api-version".to_owned(),
            message: format!(
                "unsupported package version {} (expected {VERSION})",
                request.package_version
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
            enable_includes: options.enable_includes,
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
                adocweave::preprocess::SourceMapping::Identity => "identity",
                adocweave::preprocess::SourceMapping::WholeOrigin => "whole-origin",
            }
            .to_owned(),
        })
        .collect();
    Ok(WasmPreprocessResponse {
        package_version: VERSION,
        source: document.source,
        source_map,
    })
}

pub fn process_request(
    request: WasmRequest,
    cancellation: &dyn CancellationCheck,
) -> Result<WasmResponse, WasmError> {
    if request.package_version != VERSION {
        return Err(WasmError {
            code: "unsupported-api-version".to_owned(),
            message: format!(
                "unsupported package version {} (expected {VERSION})",
                request.package_version
            ),
        });
    }
    let requested_products = request.products;
    let products: adocweave::ProductSet = requested_products.into();
    let render_inputs = request.render_inputs;
    let analysis_options = request.analysis_options;
    let render_options = request.render_policy;
    let output_limits = request.output_limits;
    render_inputs::validate(
        &render_inputs,
        &analysis_options.syntax.limits,
        &output_limits,
    )?;
    let max_output_bytes = usize::try_from(output_limits.max_output_bytes)
        .expect("u32 fits usize on supported targets");
    let authored_url_policy = AuthoredUrlPolicy {
        allowed_schemes: analysis_options
            .diagnostics
            .authored_urls
            .allowed_schemes
            .into_iter()
            .map(|scheme| scheme.to_ascii_lowercase())
            .collect::<BTreeSet<_>>(),
        allow_relative: analysis_options.diagnostics.authored_urls.allow_relative,
    };
    let active_url_policy = ActiveUrlPolicy {
        allowed_schemes: render_options
            .active_urls
            .allowed_schemes
            .into_iter()
            .map(|scheme| scheme.to_ascii_lowercase())
            .collect::<BTreeSet<_>>(),
        allow_authored_relative: render_options.active_urls.allow_authored_relative,
        allow_resolved_relative: render_options.active_urls.allow_resolved_relative,
        allow_resolved_root_relative: render_options.active_urls.allow_resolved_root_relative,
        allow_data_uris: render_options.active_urls.allow_data_uris,
    };
    let mut lint = LintConfig::default();
    lint.protected_attributes = analysis_options.diagnostics.protected_attributes;
    lint.authored_url_policy = authored_url_policy;
    lint.max_diagnostics = usize::try_from(analysis_options.diagnostics.max_diagnostics)
        .expect("u32 fits usize on supported targets");
    for (code, settings) in analysis_options.diagnostics.rules {
        let Some(descriptor) = lint_rule(&code) else {
            return Err(WasmError {
                code: "invalid-options".to_owned(),
                message: format!("unknown lint rule: {code}"),
            });
        };
        lint.set_rule(
            descriptor.id,
            RuleSettings {
                enabled: settings.enabled,
                severity: match settings.severity {
                    WasmSeverity::Error => Severity::Error,
                    WasmSeverity::Warning => Severity::Warning,
                    WasmSeverity::Information => Severity::Information,
                    WasmSeverity::Hint => Severity::Hint,
                },
            },
        );
    }
    let source_id = request.source_id.map(SourceId::new);
    let analysis = Engine::new(AnalysisOptions {
        syntax: SyntaxOptions {
            syntax_mode: match analysis_options.syntax.syntax_mode {
                WasmSyntaxMode::Permissive => SyntaxMode::Permissive,
                WasmSyntaxMode::Strict => SyntaxMode::Strict,
            },
            limits: analysis_options.syntax.limits.into(),
        },
        diagnostics: DiagnosticProfile { lint },
    })
    .analyze_cancellable_with_source_id(source_id.as_ref(), &request.source, cancellation)
    .map_err(wasm_error)?;
    if cancellation.is_cancelled() {
        return Err(cancelled_error());
    }

    let render_inputs = render_inputs::convert(render_inputs, &analysis)?;
    let render_policy = RenderPolicy {
        active_urls: active_url_policy,
        external_links: if render_options.external_links.open_in_new_context {
            adocweave::output::html::ExternalLinkPresentation::NewContext {
                noreferrer: render_options.external_links.noreferrer,
            }
        } else {
            adocweave::output::html::ExternalLinkPresentation::SameContext
        },
        source_languages: adocweave::output::html::SourceLanguagePolicy {
            allowed: render_options.source_languages.allowed.map(|languages| {
                languages
                    .into_iter()
                    .map(|language| language.to_ascii_lowercase())
                    .collect()
            }),
            unknown: match render_options.source_languages.unknown {
                WasmUnknownSourceLanguage::PreserveSanitized => {
                    adocweave::output::html::UnknownSourceLanguage::PreserveSanitized
                }
                WasmUnknownSourceLanguage::OmitClass => {
                    adocweave::output::html::UnknownSourceLanguage::OmitClass
                }
                WasmUnknownSourceLanguage::Diagnostic => {
                    adocweave::output::html::UnknownSourceLanguage::Diagnostic
                }
            },
        },
        math_languages: adocweave::output::html::MathLanguagePolicy {
            allowed: render_options
                .math_languages
                .into_iter()
                .map(|language| match language {
                    WasmMathLanguage::Latex => adocweave::semantic::MathLanguage::Latex,
                    WasmMathLanguage::Typst => adocweave::semantic::MathLanguage::Typst,
                })
                .collect(),
        },
        unresolved_references: match render_options.unresolved_references {
            WasmUnresolvedReferencePresentation::Target => {
                adocweave::output::html::UnresolvedReferencePresentation::Target
            }
            WasmUnresolvedReferencePresentation::LabelOnly => {
                adocweave::output::html::UnresolvedReferencePresentation::LabelOnly
            }
            WasmUnresolvedReferencePresentation::Hidden => {
                adocweave::output::html::UnresolvedReferencePresentation::Hidden
            }
        },
        resources: adocweave::output::html::ResourceCapabilities {
            images: render_options.resources.images,
            media: render_options.resources.media,
        },
        document_mode: match render_options.document_mode {
            WasmDocumentMode::Fragment => adocweave::output::html::HtmlDocumentMode::Fragment,
            WasmDocumentMode::Complete => adocweave::output::html::HtmlDocumentMode::Complete,
        },
        stylesheets: adocweave::output::html::StylesheetPolicy {
            sources: render_options
                .stylesheets
                .into_iter()
                .map(|stylesheet| match stylesheet {
                    WasmStylesheet::Inline { css } => {
                        adocweave::output::html::StylesheetSource::Inline(css)
                    }
                    WasmStylesheet::External { url } => {
                        adocweave::output::html::StylesheetSource::External(url)
                    }
                })
                .collect(),
            ..adocweave::output::html::StylesheetPolicy::default()
        },
        ..RenderPolicy::default()
    };
    let products = adocweave::output::conformance::products(
        &analysis,
        &render_policy,
        &render_inputs,
        products,
    );
    let diagnostics = products
        .diagnostics_json
        .as_deref()
        .map(serde_json::from_str)
        .transpose()
        .map_err(serialization_error)?;
    let render_diagnostics = products
        .render_diagnostics_json
        .as_deref()
        .map(serde_json::from_str)
        .transpose()
        .map_err(serialization_error)?;
    let symbols = products
        .symbols_json
        .as_deref()
        .map(serde_json::from_str)
        .transpose()
        .map_err(serialization_error)?;
    let projection = products
        .projection_json
        .as_deref()
        .map(serde_json::from_str)
        .transpose()
        .map_err(serialization_error)?;
    if cancellation.is_cancelled() {
        return Err(cancelled_error());
    }

    let response = WasmResponse {
        package_version: VERSION,
        version: request.version,
        generation: request.generation,
        products: requested_products,
        parse: ParseSummary {
            package_version: analysis.package_version(),
            block_count: analysis.document().blocks().len(),
            node_count: analysis.document().node_count(),
            reference_count: analysis.references().len(),
        },
        syntax: products.syntax.unwrap_or_default(),
        ast: products.canonical_ast.unwrap_or_default(),
        html: products.html.unwrap_or_default(),
        attribute_occurrences: products
            .attribute_occurrences
            .unwrap_or_default()
            .iter()
            .map(wasm_document_attribute_occurrence)
            .collect(),
        resource_queries: products
            .resource_queries
            .unwrap_or_default()
            .into_iter()
            .map(|query| {
                let reference = query.reference;
                WasmResourceQuery {
                    purpose: match reference.purpose() {
                        adocweave::resolution::ResourcePurpose::Image => WasmResourcePurpose::Image,
                        adocweave::resolution::ResourcePurpose::Icon => WasmResourcePurpose::Icon,
                        adocweave::resolution::ResourcePurpose::Audio => WasmResourcePurpose::Audio,
                        adocweave::resolution::ResourcePurpose::Video => WasmResourcePurpose::Video,
                        adocweave::resolution::ResourcePurpose::VideoPoster => {
                            WasmResourcePurpose::VideoPoster
                        }
                    },
                    form: match reference.form() {
                        adocweave::semantic::MacroForm::Inline => WasmMacroForm::Inline,
                        adocweave::semantic::MacroForm::Block => WasmMacroForm::Block,
                    },
                    owner_range: wasm_text_range(reference.owner_range()),
                    range: wasm_text_range(reference.range()),
                    target_range: wasm_text_range(reference.target_range()),
                    target: reference.target().to_owned(),
                }
            })
            .collect(),
        diagnostics: diagnostics.unwrap_or_else(|| Value::Array(Vec::new())),
        render_diagnostics: render_diagnostics.unwrap_or_else(|| Value::Array(Vec::new())),
        symbols: symbols.unwrap_or_else(|| Value::Array(Vec::new())),
        projection: projection.unwrap_or(Value::Null),
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

fn wasm_document_attribute_occurrence(
    occurrence: &adocweave::semantic::DocumentAttributeOccurrence,
) -> WasmDocumentAttributeOccurrence {
    WasmDocumentAttributeOccurrence {
        range: wasm_text_range(occurrence.range),
        name_range: wasm_text_range(occurrence.name_range),
        value_range: wasm_text_range(occurrence.value_range),
        name: occurrence.name.clone(),
        raw_value: occurrence.raw_value.clone(),
        operation: match occurrence.operation {
            adocweave::semantic::DocumentAttributeOperation::Set => {
                WasmDocumentAttributeOperation::Set
            }
            adocweave::semantic::DocumentAttributeOperation::Unset => {
                WasmDocumentAttributeOperation::Unset
            }
        },
    }
}

fn wasm_text_range(range: adocweave::text::TextRange) -> WasmTextRange {
    WasmTextRange {
        start: range.start().to_u32(),
        end: range.end().to_u32(),
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
            package_version: VERSION.to_owned(),
            source_id: Some("web:document".to_owned()),
            version: 3,
            generation: 7,
            source: source.to_owned(),
            products: WasmProductSet {
                syntax: true,
                canonical_ast: true,
                html: true,
                attribute_occurrences: true,
                resource_queries: true,
                diagnostics: true,
                symbols: true,
                projection: true,
            },
            render_inputs: WasmRenderInputs::default(),
            analysis_options: WasmAnalysisOptions::default(),
            render_policy: WasmRenderPolicy::default(),
            output_limits: WasmOutputLimits::default(),
        }
    }

    #[test]
    fn wasm_api_returns_all_products_from_one_versioned_request() {
        let response =
            process_request(request("= Title\n\n== Section\n"), &NeverCancel).expect("response");

        assert_eq!(response.version, 3);
        assert_eq!(response.generation, 7);
        assert_eq!(response.package_version, VERSION);
        assert!(response.syntax.contains("Document@"));
        assert!(response.ast.contains("\"blocks\""));
        assert!(response.html.contains("<h1"));
        assert_eq!(response.symbols[0]["name"], "Title");
        assert_eq!(response.projection["packageVersion"], VERSION);
        assert_eq!(response.parse.reference_count, 0);
    }

    #[test]
    fn wasm_default_product_set_omits_unused_canonical_products() {
        let mut request = request("= Title\n\nText");
        request.products = WasmProductSet::default();
        let response = process_request(request, &NeverCancel).expect("response");

        assert!(response.syntax.is_empty());
        assert!(response.ast.is_empty());
        assert!(response.attribute_occurrences.is_empty());
        assert!(response.symbols.as_array().is_some_and(Vec::is_empty));
        assert!(!response.html.is_empty());
        assert!(response.projection.is_object());
    }

    #[test]
    fn wasm_api_exposes_source_preserving_document_attribute_occurrences() {
        let source = include_str!("../../../fixtures/attributes/public-occurrences.adoc");
        let response = process_request(request(source), &NeverCancel).expect("response");

        assert_eq!(response.attribute_occurrences.len(), 5);
        assert_eq!(response.attribute_occurrences[0].name, "duplicate");
        assert_eq!(response.attribute_occurrences[0].raw_value, "first");
        assert_eq!(
            response.attribute_occurrences[1].operation,
            WasmDocumentAttributeOperation::Set
        );
        assert_eq!(response.attribute_occurrences[2].raw_value, "");
        assert_eq!(
            response.attribute_occurrences[3].operation,
            WasmDocumentAttributeOperation::Unset
        );
        assert_eq!(
            response.attribute_occurrences[4].operation,
            WasmDocumentAttributeOperation::Unset
        );
        assert!(
            response.attribute_occurrences[2].value_range.start
                == response.attribute_occurrences[2].value_range.end
        );
        assert_eq!(
            &source[usize::try_from(response.attribute_occurrences[0].range.start).expect("offset")
                ..usize::try_from(response.attribute_occurrences[0].range.end).expect("offset")],
            ":duplicate: first\n"
        );
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
                    media_type: "image/png".to_owned(),
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
                    media_type: "image/png".to_owned(),
                    byte_length: None,
                },
            });
        let unsafe_response = process_request(unsafe_request, &NeverCancel).expect("response");
        assert_eq!(unsafe_response.html, "<p>alt</p>\n");
        assert_eq!(
            unsafe_response.render_diagnostics[0]["code"],
            "invalid-url-scheme"
        );

        let mut root_relative_request = request(source);
        root_relative_request
            .render_policy
            .active_urls
            .allow_resolved_root_relative = true;
        root_relative_request
            .render_inputs
            .resources
            .push(WasmResolvedResource {
                source_start: 0,
                source_end: source.len() as u32,
                outcome: WasmResourceOutcome::Resolved {
                    href: "/assets/image.png".to_owned(),
                    media_type: "image/png".to_owned(),
                    byte_length: None,
                },
            });
        let root_relative = process_request(root_relative_request, &NeverCancel).expect("response");
        assert_eq!(
            root_relative.html,
            "<p><img src=\"/assets/image.png\" alt=\"alt\"></p>\n"
        );
        assert_eq!(root_relative.render_diagnostics, json!([]));

        let mut limited = request(source);
        limited.analysis_options.syntax.limits.max_references = 0;
        limited.render_inputs.resources.push(WasmResolvedResource {
            source_start: 0,
            source_end: source.len() as u32,
            outcome: WasmResourceOutcome::Resolved {
                href: "https://cdn.example/image.png".to_owned(),
                media_type: "image/png".to_owned(),
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
                media_type: "image/png".to_owned(),
                byte_length: None,
            },
        });
        let error = process_request(invalid, &NeverCancel).expect_err("outside source");
        assert_eq!(error.code, "invalid-render-input");
        assert_eq!(error.message, "render input is invalid");

        let mut invalid_media_type = request(source);
        invalid_media_type
            .render_inputs
            .resources
            .push(WasmResolvedResource {
                source_start: 0,
                source_end: source.len() as u32,
                outcome: WasmResourceOutcome::Resolved {
                    href: "https://cdn.example/image.png".to_owned(),
                    media_type: "image/png; arbitrary garbage".to_owned(),
                    byte_length: None,
                },
            });
        let error =
            process_request(invalid_media_type, &NeverCancel).expect_err("invalid media type");
        assert_eq!(error.code, "invalid-render-input");
        assert_eq!(error.message, "render input is invalid");
    }

    #[test]
    fn wasm_rejects_malformed_authored_urls() {
        for target in ["http//example.com", "bad%ZZpath", "trailing%"] {
            let response =
                process_request(request(&format!("link:{target}[unsafe]")), &NeverCancel)
                    .expect("response");

            assert!(
                response
                    .diagnostics
                    .as_array()
                    .expect("diagnostic array")
                    .iter()
                    .any(|diagnostic| diagnostic["code"] == "invalid-url-scheme"),
                "{target}"
            );
            assert!(!response.html.contains("href="), "{target}");
        }
    }

    #[test]
    fn wasm_api_exposes_primary_and_poster_resource_queries() {
        let source = "video:demo.mp4[Demo,poster=\"ポスター.jpg\"]";
        let response = process_request(request(source), &NeverCancel).expect("response");
        assert_eq!(response.resource_queries.len(), 2);
        assert_eq!(
            response.resource_queries[0].purpose,
            WasmResourcePurpose::Video
        );
        assert_eq!(
            response.resource_queries[1].purpose,
            WasmResourcePurpose::VideoPoster
        );
        assert_eq!(response.resource_queries[1].target, "ポスター.jpg");
        let range = response.resource_queries[1].target_range;
        assert_eq!(
            &source[usize::try_from(range.start).expect("start")
                ..usize::try_from(range.end).expect("end")],
            "ポスター.jpg"
        );
        assert_eq!(
            response.resource_queries[1].owner_range,
            response.resource_queries[0].owner_range
        );
    }

    #[test]
    fn wasm_resolved_reference_display_text_is_escaped_plain_text() {
        let source = "xref:note:01800000-0000-7000-8000-000000000001[]";
        let mut resolved_request = request(source);
        resolved_request
            .render_policy
            .active_urls
            .allow_resolved_root_relative = true;
        resolved_request
            .render_inputs
            .references
            .push(WasmResolvedReference {
                source_start: 0,
                source_end: source.len() as u32,
                outcome: WasmReferenceOutcome::Resolved {
                    href: "/notes/01800000-0000-7000-8000-000000000001".to_owned(),
                    display_text: Some("公開 <タイトル> & *not markup*".to_owned()),
                    notices: Vec::new(),
                },
            });

        let response = process_request(resolved_request, &NeverCancel).expect("response");

        assert_eq!(
            response.html,
            "<p><a href=\"/notes/01800000-0000-7000-8000-000000000001\">公開 &lt;タイトル&gt; &amp; *not markup*</a></p>\n"
        );
        assert_eq!(
            response.projection["referenceEdges"][0]["resolution"]["displayText"],
            "公開 <タイトル> & *not markup*"
        );

        let mut oversized = request(source);
        oversized.output_limits.max_output_bytes = 4;
        oversized
            .render_inputs
            .references
            .push(WasmResolvedReference {
                source_start: 0,
                source_end: source.len() as u32,
                outcome: WasmReferenceOutcome::Resolved {
                    href: "x".to_owned(),
                    display_text: Some("title".to_owned()),
                    notices: Vec::new(),
                },
            });
        let error = process_request(oversized, &NeverCancel).expect_err("display text limit");
        assert_eq!(error.code, "limit-exceeded");
    }

    #[test]
    fn wasm_applies_the_complete_host_render_profile() {
        let source = "https://example.com/[External]\n\n[source,python]\n----\nprint(1)\n----\n\nstem:[x] xref:note:secret[] image:https://example/x.png[alt]";
        let mut request = request(source);
        request.render_policy.external_links = WasmExternalLinkPolicy {
            open_in_new_context: true,
            noreferrer: true,
        };
        request.render_policy.source_languages = WasmSourceLanguagePolicy {
            allowed: Some(vec!["rust".to_owned()]),
            unknown: WasmUnknownSourceLanguage::Diagnostic,
        };
        request.render_policy.math_languages.clear();
        request.render_policy.unresolved_references =
            WasmUnresolvedReferencePresentation::LabelOnly;
        request.render_policy.resources = WasmResourceCapabilities {
            images: false,
            media: false,
        };

        let response = process_request(request, &NeverCancel).expect("response");
        assert!(
            response
                .html
                .contains("target=\"_blank\" rel=\"noopener noreferrer\"")
        );
        assert!(!response.html.contains("language-python"));
        assert!(!response.html.contains("math-latex"));
        assert!(!response.html.contains("note:secret"));
        assert!(!response.html.contains("<img"));
        assert_eq!(response.projection["formulas"][0]["source"], "x");
        let codes = response
            .render_diagnostics
            .as_array()
            .expect("render diagnostics")
            .iter()
            .filter_map(|diagnostic| diagnostic["code"].as_str())
            .collect::<Vec<_>>();
        assert!(codes.contains(&"source-language-not-allowed"));
        assert!(codes.contains(&"math-language-not-allowed"));
        assert!(codes.contains(&"resource-capability-disabled"));
    }

    #[test]
    fn wasm_stylesheets_render_only_into_the_complete_document_head() {
        let mut complete = request("paragraph");
        complete.render_policy.document_mode = WasmDocumentMode::Complete;
        complete.render_policy.stylesheets = vec![
            WasmStylesheet::Inline {
                css: "p { margin: 0; }".to_owned(),
            },
            WasmStylesheet::External {
                url: "https://example.com/theme.css".to_owned(),
            },
        ];

        let response = process_request(complete, &NeverCancel).expect("response");
        assert!(response.html.starts_with("<!doctype html>"));
        assert!(
            response
                .html
                .contains("<style>\np { margin: 0; }\n</style>")
        );
        assert!(
            response
                .html
                .contains("<link rel=\"stylesheet\" href=\"https://example.com/theme.css\">")
        );
        assert_eq!(response.render_diagnostics, json!([]));

        let mut fragment = request("paragraph");
        fragment.render_policy.stylesheets = vec![WasmStylesheet::Inline {
            css: "p {}".to_owned(),
        }];
        let response = process_request(fragment, &NeverCancel).expect("response");
        assert_eq!(response.html, "<p>paragraph</p>\n");
        assert_eq!(
            response.render_diagnostics[0]["code"],
            "stylesheet-not-applicable"
        );
    }

    #[test]
    fn wasm_stylesheets_fail_closed_on_hostile_configuration() {
        let mut hostile = request("paragraph");
        hostile.render_policy.document_mode = WasmDocumentMode::Complete;
        hostile.render_policy.stylesheets = vec![
            WasmStylesheet::Inline {
                css: "p {}</style><script>alert(1)</script>".to_owned(),
            },
            WasmStylesheet::External {
                url: "javascript:alert(1)".to_owned(),
            },
        ];

        let response = process_request(hostile, &NeverCancel).expect("response");
        assert!(!response.html.contains("<style"));
        assert!(!response.html.contains("<link"));
        assert!(!response.html.contains("script"));
        let codes = response
            .render_diagnostics
            .as_array()
            .expect("render diagnostics")
            .iter()
            .filter_map(|diagnostic| diagnostic["code"].as_str())
            .collect::<Vec<_>>();
        assert!(codes.contains(&"invalid-stylesheet-content"));
        assert!(codes.contains(&"invalid-stylesheet-url"));
    }

    #[test]
    fn wasm_api_rejects_unknown_fields_and_versions() {
        let invalid = json!({
            "packageVersion": VERSION,
            "sourceId": null,
            "version": 1,
            "generation": 1,
            "source": "text",
            "unexpected": true
        })
        .to_string();
        let error = process_json(&invalid).expect_err("invalid request");
        assert!(error.contains("invalid-request"));

        let legacy_options = json!({
            "packageVersion": VERSION,
            "sourceId": null,
            "version": 1,
            "generation": 1,
            "source": "text",
            "options": {"syntaxMode": "strict"}
        })
        .to_string();
        let error = process_json(&legacy_options).expect_err("legacy options are rejected");
        assert!(error.contains("invalid-request"));

        let leaked_failure = json!({
            "packageVersion": VERSION,
            "sourceId": null,
            "version": 1,
            "generation": 1,
            "source": "xref:note:private[]",
            "renderInputs": {
                "references": [{
                    "sourceStart": 0,
                    "sourceEnd": 19,
                    "outcome": {
                        "status": "failed",
                        "kind": "missing-target",
                        "message": "ACL denied: private title"
                    }
                }]
            }
        })
        .to_string();
        let error = process_json(&leaked_failure).expect_err("failure detail is forbidden");
        assert!(error.contains("invalid-request"));

        let error = process_request(
            WasmRequest {
                package_version: "0.0.0".to_owned(),
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
        let max_input = usize::try_from(AnalysisOptions::default().syntax.limits.max_input_bytes)
            .expect("u32 fits usize on supported targets");
        let source = "x".repeat(max_input + 1);
        let error = process_request(request(&source), &NeverCancel).expect_err("limit");
        assert_eq!(error.code, "limit-exceeded");
    }

    #[test]
    fn wasm_options_are_partial_overrides_and_bound_the_complete_response() {
        let value = json!({
            "packageVersion": VERSION,
            "sourceId": null,
            "version": 1,
            "generation": 1,
            "source": "text",
            "outputLimits": {"maxOutputBytes": 1}
        });
        let request: WasmRequest = serde_json::from_value(value).expect("partial options");
        assert_eq!(
            request.analysis_options.syntax.limits.max_input_bytes,
            10 * 1024 * 1024
        );
        let error = process_request(request, &NeverCancel).expect_err("output limit");
        assert_eq!(error.code, "limit-exceeded");
    }

    #[test]
    fn wasm_diagnostic_profile_uses_the_typed_lint_registry() {
        let mut configured = request("text \n");
        configured.analysis_options.diagnostics.rules.insert(
            "trailing-whitespace".to_owned(),
            WasmRuleSettings {
                enabled: true,
                severity: WasmSeverity::Error,
            },
        );
        let response = process_request(configured, &NeverCancel).expect("configured diagnostics");
        assert_eq!(response.diagnostics[0]["code"], "trailing-whitespace");
        assert_eq!(response.diagnostics[0]["severity"], "error");

        let mut unknown = request("text");
        unknown
            .analysis_options
            .diagnostics
            .rules
            .insert("unknown-rule".to_owned(), WasmRuleSettings::default());
        let error = process_request(unknown, &NeverCancel).expect_err("unknown lint rule");
        assert_eq!(error.code, "invalid-options");
    }

    #[test]
    fn opt_in_macro_boundary_matches_the_native_diagnostic_contract() {
        let source = "本文xref:guide.adoc[Guide]\n";
        let default_response =
            process_request(request(source), &NeverCancel).expect("default diagnostics");
        assert!(
            default_response
                .diagnostics
                .as_array()
                .expect("diagnostics")
                .iter()
                .all(|diagnostic| diagnostic["code"] != "macro-boundary")
        );

        let mut configured = request(source);
        configured.analysis_options.diagnostics.rules.insert(
            "macro-boundary".to_owned(),
            WasmRuleSettings {
                enabled: true,
                severity: WasmSeverity::Warning,
            },
        );
        let wasm = process_request(configured, &NeverCancel).expect("opt-in diagnostics");
        let wasm = wasm
            .diagnostics
            .as_array()
            .expect("diagnostics")
            .iter()
            .find(|diagnostic| diagnostic["code"] == "macro-boundary")
            .expect("macro-boundary diagnostic");

        let mut lint = LintConfig::default();
        lint.set_rule(
            adocweave::output::diagnostics::MACRO_BOUNDARY,
            RuleSettings {
                enabled: true,
                severity: Severity::Warning,
            },
        );
        let native = Engine::new(AnalysisOptions {
            diagnostics: DiagnosticProfile { lint },
            ..AnalysisOptions::default()
        })
        .analyze(source)
        .expect("native analysis");
        let native = native
            .diagnostics()
            .iter()
            .find(|diagnostic| diagnostic.code.as_str() == "macro-boundary")
            .expect("native macro-boundary diagnostic");

        assert_eq!(wasm["code"], native.code.as_str());
        assert_eq!(wasm["severity"], native.severity.as_str());
        assert_eq!(wasm["range"]["start"], native.range.start().to_u32());
        assert_eq!(wasm["range"]["end"], native.range.end().to_u32());
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
            package_version: VERSION.to_owned(),
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
