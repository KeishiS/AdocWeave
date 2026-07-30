//! Meaning conversion from normalized wire values to core execution values.

use std::collections::BTreeSet;

#[cfg(test)]
use adocweave::OutputLimits;
use adocweave::output::conformance::ProductSet;
use adocweave::output::diagnostics::{LintConfig, RuleSettings, Severity, lint_rule};
use adocweave::output::html::{
    ExternalLinkPresentation, HtmlDocumentMode, MathLanguagePolicy, RenderPolicy,
    ResourceCapabilities, SourceLanguagePolicy, StylesheetPolicy, StylesheetSource,
    UnknownSourceLanguage, UnresolvedReferencePresentation,
};
use adocweave::preprocess::{EffectiveProcessingOptions, ResourceSnapshot};
use adocweave::resolution::{ActiveUrlPolicy, AuthoredUrlPolicy};
use adocweave::{
    AnalysisLimits, AnalysisOptions, DiagnosticProfile, Engine, SourceId, SyntaxMode, SyntaxOptions,
};

use crate::WasmError;
use crate::preprocess_wire::{resource_snapshot, to_core_options};
use crate::protocol_generated::WasmProductSet;
use crate::render_input_normalization::NormalizedRenderInputs;
use crate::request_enum_generated::{
    WasmDocumentMode, WasmSyntaxMode, WasmUnknownSourceLanguage,
    WasmUnresolvedReferencePresentation,
};
use crate::request_normalization::NormalizedRequest;
use crate::request_wire::{WasmLimits, WasmStylesheet};
use crate::shared_wire_generated::{WasmMathLanguage, WasmSeverity};

pub(crate) enum ProcessingExecution {
    Standalone {
        engine: Engine,
    },
    Combined {
        snapshot: ResourceSnapshot,
        options: EffectiveProcessingOptions,
    },
}

/// Core configuration and remaining analysis-dependent wire inputs.
///
/// Construction consumes `NormalizedRequest`, so execution cannot bypass the
/// public version and cross-field validation stage.
pub(crate) struct ExecutionRequest {
    pub(crate) version: u32,
    pub(crate) generation: u32,
    pub(crate) source: String,
    pub(crate) source_id: Option<SourceId>,
    pub(crate) requested_products: WasmProductSet,
    pub(crate) products: ProductSet,
    pub(crate) render_inputs: NormalizedRenderInputs,
    pub(crate) processing: ProcessingExecution,
    pub(crate) render_policy: RenderPolicy,
    pub(crate) max_output_bytes: usize,
}

pub(crate) fn convert(request: NormalizedRequest) -> Result<ExecutionRequest, WasmError> {
    let (request, render_inputs) = request.into_parts();
    let requested_products = request.products;
    let products = requested_products.into();
    let analysis_options = request.analysis_options;
    let render_options = request.render_policy;
    let source_id = request.source_id.map(SourceId::new);
    let analysis_options = AnalysisOptions {
        syntax: SyntaxOptions {
            syntax_mode: match analysis_options.syntax.syntax_mode {
                WasmSyntaxMode::Permissive => SyntaxMode::Permissive,
                WasmSyntaxMode::Strict => SyntaxMode::Strict,
            },
            limits: analysis_limits(analysis_options.syntax.limits),
        },
        diagnostics: DiagnosticProfile {
            lint: lint_config(analysis_options.diagnostics)?,
        },
        attributes: analysis_options.attributes,
    };
    let processing = match request.preprocess {
        Some(input) => {
            let options = to_core_options(source_id.clone(), input.options);
            EffectiveProcessingOptions::new(analysis_options.clone(), options)
                .map(|options| ProcessingExecution::Combined {
                    snapshot: resource_snapshot(input.resources),
                    options,
                })
                .map_err(|error| WasmError {
                    code: "invalid-options".to_owned(),
                    message: error.to_string(),
                })?
        }
        None => ProcessingExecution::Standalone {
            engine: Engine::new(analysis_options),
        },
    };
    let max_output_bytes = usize::try_from(request.output_limits.max_output_bytes)
        .expect("u32 fits usize on supported targets");

    Ok(ExecutionRequest {
        version: request.version,
        generation: request.generation,
        source: request.source,
        source_id,
        requested_products,
        products,
        render_inputs,
        processing,
        render_policy: render_policy(render_options),
        max_output_bytes,
    })
}

fn lint_config(
    diagnostics: crate::request_wire::WasmDiagnosticProfile,
) -> Result<LintConfig, WasmError> {
    let authored_url_policy = AuthoredUrlPolicy {
        allowed_schemes: diagnostics
            .authored_urls
            .allowed_schemes
            .into_iter()
            .map(|scheme| scheme.to_ascii_lowercase())
            .collect::<BTreeSet<_>>(),
        allow_relative: diagnostics.authored_urls.allow_relative,
    };
    let mut lint = LintConfig::default();
    lint.protected_attributes = diagnostics.protected_attributes;
    lint.authored_url_policy = authored_url_policy;
    lint.max_diagnostics =
        usize::try_from(diagnostics.max_diagnostics).expect("u32 fits usize on supported targets");
    for (code, settings) in diagnostics.rules {
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
    Ok(lint)
}

fn render_policy(options: crate::request_wire::WasmRenderPolicy) -> RenderPolicy {
    let active_urls = ActiveUrlPolicy {
        allowed_schemes: options
            .active_urls
            .allowed_schemes
            .into_iter()
            .map(|scheme| scheme.to_ascii_lowercase())
            .collect::<BTreeSet<_>>(),
        allow_authored_relative: options.active_urls.allow_authored_relative,
        allow_resolved_relative: options.active_urls.allow_resolved_relative,
        allow_resolved_root_relative: options.active_urls.allow_resolved_root_relative,
        allow_data_uris: options.active_urls.allow_data_uris,
    };
    RenderPolicy {
        active_urls,
        external_links: if options.external_links.open_in_new_context {
            ExternalLinkPresentation::NewContext {
                noreferrer: options.external_links.noreferrer,
            }
        } else {
            ExternalLinkPresentation::SameContext
        },
        source_languages: SourceLanguagePolicy {
            allowed: options.source_languages.allowed.map(|languages| {
                languages
                    .into_iter()
                    .map(|language| language.to_ascii_lowercase())
                    .collect()
            }),
            unknown: match options.source_languages.unknown {
                WasmUnknownSourceLanguage::PreserveSanitized => {
                    UnknownSourceLanguage::PreserveSanitized
                }
                WasmUnknownSourceLanguage::OmitClass => UnknownSourceLanguage::OmitClass,
                WasmUnknownSourceLanguage::Diagnostic => UnknownSourceLanguage::Diagnostic,
            },
        },
        math_languages: MathLanguagePolicy {
            allowed: options
                .math_languages
                .into_iter()
                .map(|language| match language {
                    WasmMathLanguage::Latex => adocweave::semantic::MathLanguage::Latex,
                    WasmMathLanguage::Typst => adocweave::semantic::MathLanguage::Typst,
                })
                .collect(),
        },
        unresolved_references: match options.unresolved_references {
            WasmUnresolvedReferencePresentation::Target => UnresolvedReferencePresentation::Target,
            WasmUnresolvedReferencePresentation::LabelOnly => {
                UnresolvedReferencePresentation::LabelOnly
            }
            WasmUnresolvedReferencePresentation::Hidden => UnresolvedReferencePresentation::Hidden,
        },
        resources: ResourceCapabilities {
            images: options.resources.images,
            media: options.resources.media,
        },
        document_mode: match options.document_mode {
            WasmDocumentMode::Fragment => HtmlDocumentMode::Fragment,
            WasmDocumentMode::Complete => HtmlDocumentMode::Complete,
        },
        stylesheets: StylesheetPolicy {
            sources: options
                .stylesheets
                .into_iter()
                .map(|stylesheet| match stylesheet {
                    WasmStylesheet::Inline { css } => StylesheetSource::Inline(css),
                    WasmStylesheet::External { url } => StylesheetSource::External(url),
                })
                .collect(),
            ..StylesheetPolicy::default()
        },
        ..RenderPolicy::default()
    }
}

fn analysis_limits(value: WasmLimits) -> AnalysisLimits {
    value.into()
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::request_wire::{WasmOutputLimits, WasmRenderPolicy};

    #[test]
    fn wire_defaults_match_core_defaults_at_the_conversion_boundary() {
        assert_eq!(
            analysis_limits(WasmLimits::default()),
            AnalysisLimits::default()
        );
        assert_eq!(
            WasmOutputLimits::default().max_output_bytes,
            OutputLimits::default().max_output_bytes
        );
        assert_eq!(
            render_policy(WasmRenderPolicy::default()),
            RenderPolicy::default()
        );
    }
}
