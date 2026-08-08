use adocweave::{SourceId, VERSION};
use serde::Deserialize;

use crate::{
    ParseSummary, WasmAttributeBindingQuery, WasmAttributeExpansionError,
    WasmAttributeQueryProduct, WasmAttributeReferenceQuery, WasmAttributeValueContinuation,
    WasmDiagnostic, WasmDocumentAttributeContinuation, WasmDocumentAttributeOccurrence,
    WasmDocumentAttributeOperation, WasmDocumentAttributeValue, WasmDocumentAttributeValueLine,
    WasmDocumentProjection, WasmDocumentSymbol, WasmError, WasmMacroForm, WasmProductSet,
    WasmResourcePurpose, WasmResourceQuery, WasmResponse, WasmTextRange, serialization_error,
};

pub(crate) fn project_response(
    products: adocweave::output::conformance::DocumentProducts,
    requested_products: WasmProductSet,
    version: u32,
    generation: u32,
    analysis: &adocweave::Analysis,
    source_id: Option<&SourceId>,
    attribute_projection: Option<&adocweave::preprocess::AnalysisProjection>,
) -> Result<WasmResponse, WasmError> {
    let diagnostics =
        parse_optional_product::<Vec<WasmDiagnostic>>(products.diagnostics_json.as_deref())?;
    let render_diagnostics =
        parse_optional_product::<Vec<WasmDiagnostic>>(products.render_diagnostics_json.as_deref())?;
    let symbols =
        parse_optional_product::<Vec<WasmDocumentSymbol>>(products.symbols_json.as_deref())?;
    let projection =
        parse_optional_product::<WasmDocumentProjection>(products.projection_json.as_deref())?;
    Ok(WasmResponse {
        package_version: VERSION.to_owned(),
        version,
        generation,
        products: requested_products,
        parse: ParseSummary {
            block_count: response_count("blockCount", analysis.document().blocks().len())?,
            node_count: response_count("nodeCount", analysis.document().node_count())?,
            reference_count: response_count("referenceCount", analysis.references().len())?,
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
        attribute_queries: products
            .attribute_queries
            .map(|queries| wasm_attribute_query_product(&queries, source_id, attribute_projection))
            .unwrap_or_default(),
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
        diagnostics: diagnostics.unwrap_or_default(),
        render_diagnostics: render_diagnostics.unwrap_or_default(),
        symbols: symbols.unwrap_or_default(),
        projection,
    })
}

pub(crate) fn enforce_output_limit(
    response: &WasmResponse,
    max_output_bytes: usize,
) -> Result<(), WasmError> {
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
    Ok(())
}

fn response_count(name: &str, value: usize) -> Result<u32, WasmError> {
    u32::try_from(value)
        .map_err(|_| serialization_error(format!("{name} does not fit the response schema")))
}

pub(crate) fn parse_optional_product<T>(source: Option<&str>) -> Result<Option<T>, WasmError>
where
    T: for<'de> Deserialize<'de>,
{
    source
        .map(serde_json::from_str)
        .transpose()
        .map_err(serialization_error)
}

fn wasm_document_attribute_occurrence(
    occurrence: &adocweave::semantic::DocumentAttributeOccurrence,
) -> WasmDocumentAttributeOccurrence {
    WasmDocumentAttributeOccurrence {
        range: wasm_text_range(occurrence.range),
        name_range: wasm_text_range(occurrence.name_range),
        name: occurrence.name.clone(),
        value: WasmDocumentAttributeValue {
            source_range: wasm_text_range(occurrence.value.source_range),
            source_text: occurrence.value.source_text.clone(),
            folded_text: occurrence.value.folded_text.clone(),
            lines: occurrence
                .value
                .lines
                .iter()
                .map(|line| WasmDocumentAttributeValueLine {
                    range: wasm_text_range(line.range),
                    indent_range: wasm_text_range(line.indent_range),
                    content_range: wasm_text_range(line.content_range),
                    ending_range: wasm_text_range(line.ending_range),
                    continuation: line.continuation.map(|continuation| {
                        WasmDocumentAttributeContinuation {
                            kind: match continuation.kind {
                                adocweave::semantic::AttributeValueContinuation::Soft => {
                                    WasmAttributeValueContinuation::Soft
                                }
                                adocweave::semantic::AttributeValueContinuation::Hard => {
                                    WasmAttributeValueContinuation::Hard
                                }
                            },
                            range: wasm_text_range(continuation.range),
                        }
                    }),
                })
                .collect(),
        },
        operation: match occurrence.operation {
            adocweave::semantic::DocumentAttributeOperation::Set => {
                WasmDocumentAttributeOperation::Set
            }
            adocweave::semantic::DocumentAttributeOperation::Unset => {
                WasmDocumentAttributeOperation::Unset
            }
        },
        valid: occurrence.valid,
    }
}

fn wasm_attribute_query_product(
    product: &adocweave::semantic::AttributeQueryProduct,
    source_id: Option<&SourceId>,
    projection: Option<&adocweave::preprocess::AnalysisProjection>,
) -> WasmAttributeQueryProduct {
    let source_id = source_id.map(|source_id| source_id.as_str().to_owned());
    WasmAttributeQueryProduct {
        bindings: product
            .bindings
            .iter()
            .map(|binding| {
                let (effective_value, error) = wasm_attribute_resolution(binding.value());
                let projected = projection.and_then(|projection| {
                    projection
                        .attribute_bindings
                        .iter()
                        .find(|candidate| candidate.value.id() == binding.id())
                });
                let mut occurrence = wasm_document_attribute_occurrence(binding.occurrence());
                if let Some(projected_occurrence) = projection.and_then(|projection| {
                    projection
                        .attribute_occurrences
                        .iter()
                        .find(|candidate| candidate.value.range == binding.occurrence().range)
                }) {
                    wasm_project_document_attribute_occurrence(
                        &mut occurrence,
                        projected_occurrence,
                    );
                }
                let (binding_source_id, range) = wasm_projected_range(
                    projected.map(|value| value.origins.as_slice()),
                    source_id.clone(),
                    binding.occurrence().range,
                );
                occurrence.range = range;
                occurrence.name_range = wasm_projected_range(
                    projected.map(|value| value.name_origins.as_slice()),
                    binding_source_id.clone(),
                    binding.occurrence().name_range,
                )
                .1;
                occurrence.value.source_range = wasm_projected_range(
                    projected.map(|value| value.value_origins.as_slice()),
                    binding_source_id.clone(),
                    binding.occurrence().value.source_range,
                )
                .1;
                WasmAttributeBindingQuery {
                    id: binding.id().get(),
                    source_id: binding_source_id,
                    operation: match binding.operation() {
                        adocweave::semantic::DocumentAttributeOperation::Set => {
                            WasmDocumentAttributeOperation::Set
                        }
                        adocweave::semantic::DocumentAttributeOperation::Unset => {
                            WasmDocumentAttributeOperation::Unset
                        }
                    },
                    effective_value,
                    error,
                    occurrence,
                }
            })
            .collect(),
        references: product
            .references
            .iter()
            .enumerate()
            .map(|(index, reference)| {
                let (effective_value, error) = wasm_attribute_resolution(
                    reference
                        .value
                        .as_ref()
                        .map(|value| value.as_deref())
                        .map_err(|error| *error),
                );
                let projected =
                    projection.and_then(|projection| projection.attribute_references.get(index));
                let (reference_source_id, range) = wasm_projected_range(
                    projected.map(|value| value.origins.as_slice()),
                    source_id.clone(),
                    reference.range,
                );
                WasmAttributeReferenceQuery {
                    source_id: reference_source_id.clone(),
                    range,
                    name_range: wasm_projected_range(
                        projected.map(|value| value.name_origins.as_slice()),
                        reference_source_id,
                        reference.name_range,
                    )
                    .1,
                    name: reference.name.clone(),
                    binding_id: reference.binding_id.map(|id| id.get()),
                    effective_value,
                    error,
                }
            })
            .collect(),
    }
}

fn wasm_project_document_attribute_occurrence(
    output: &mut WasmDocumentAttributeOccurrence,
    projected: &adocweave::preprocess::ProjectedDocumentAttribute,
) {
    output.range = wasm_first_origin_range(&projected.origins, output.range);
    output.name_range = wasm_first_origin_range(&projected.name_origins, output.name_range);
    output.value.source_range =
        wasm_first_origin_range(&projected.value_origins, output.value.source_range);
    for (line, projected_line) in output.value.lines.iter_mut().zip(&projected.value_lines) {
        line.range = wasm_first_origin_range(&projected_line.origins, line.range);
        line.indent_range =
            wasm_first_origin_range(&projected_line.indent_origins, line.indent_range);
        line.content_range =
            wasm_first_origin_range(&projected_line.content_origins, line.content_range);
        line.ending_range =
            wasm_first_origin_range(&projected_line.ending_origins, line.ending_range);
        if let Some(continuation) = &mut line.continuation {
            continuation.range =
                wasm_first_origin_range(&projected_line.continuation_origins, continuation.range);
        }
    }
}

fn wasm_first_origin_range(
    origins: &[adocweave::preprocess::SourceOrigin],
    fallback: WasmTextRange,
) -> WasmTextRange {
    origins.first().map_or(fallback, |origin| WasmTextRange {
        start: origin.range.start().to_u32(),
        end: origin.range.end().to_u32(),
    })
}

fn wasm_projected_range(
    origins: Option<&[adocweave::preprocess::SourceOrigin]>,
    fallback_source_id: Option<String>,
    fallback_range: adocweave::text::TextRange,
) -> (Option<String>, WasmTextRange) {
    origins.and_then(|origins| origins.first()).map_or_else(
        || (fallback_source_id, wasm_text_range(fallback_range)),
        |origin| {
            (
                origin
                    .source_id
                    .as_ref()
                    .map(|source_id| source_id.as_str().to_owned()),
                wasm_text_range(origin.range.text_range()),
            )
        },
    )
}

fn wasm_attribute_resolution(
    value: Result<Option<&str>, adocweave::semantic::AttributeExpansionError>,
) -> (Option<String>, Option<WasmAttributeExpansionError>) {
    match value {
        Ok(value) => (value.map(str::to_owned), None),
        Err(error) => (
            None,
            Some(match error {
                adocweave::semantic::AttributeExpansionError::Undefined => {
                    WasmAttributeExpansionError::Undefined
                }
                adocweave::semantic::AttributeExpansionError::Cycle => {
                    WasmAttributeExpansionError::Cycle
                }
                adocweave::semantic::AttributeExpansionError::DepthLimitExceeded => {
                    WasmAttributeExpansionError::DepthLimitExceeded
                }
                adocweave::semantic::AttributeExpansionError::SizeLimitExceeded => {
                    WasmAttributeExpansionError::SizeLimitExceeded
                }
            }),
        ),
    }
}

fn wasm_text_range(range: adocweave::text::TextRange) -> WasmTextRange {
    WasmTextRange {
        start: range.start().to_u32(),
        end: range.end().to_u32(),
    }
}

#[cfg(test)]
mod tests {
    use super::response_count;

    #[test]
    #[cfg(target_pointer_width = "64")]
    fn response_count_rejects_values_beyond_the_schema_range() {
        let value = usize::try_from(u64::from(u32::MAX) + 1).expect("64-bit usize");
        let error = response_count("nodeCount", value).expect_err("out-of-range count");
        assert_eq!(error.code, "serialization-failed");
        assert!(error.message.contains("nodeCount"));
    }
}
