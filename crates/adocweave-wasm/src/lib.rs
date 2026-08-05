//! Versioned, allocation-owning WASM boundary over the deterministic core.

use adocweave::preprocess::{
    PreprocessInputs, PreprocessedAnalysisError, ProjectionFailure, ProjectionLimits, preprocess,
};
use adocweave::{CancellationCheck, NeverCancel, ParseError, SourceId, VERSION};

mod preprocess_projection;
mod preprocess_wire;
mod preprocess_wire_generated;
mod protocol_generated;
mod render_input_conversion;
mod render_input_normalization;
mod render_input_wire;
mod render_input_wire_generated;
mod request_conversion;
mod request_enum_generated;
mod request_normalization;
mod request_wire;
mod request_wire_generated;
mod response_projection;
mod response_wire;
mod response_wire_generated;
mod shared_wire_generated;
pub use preprocess_wire::{
    WasmAnalysisPreprocessInput, WasmError, WasmPreprocessOptions, WasmPreprocessRequest,
    WasmPreprocessResponse, WasmResource, WasmSafeMode, WasmSourceMapSegment, WasmSourceMapping,
};
pub use protocol_generated::{PROTOCOL_SCHEMA_VERSION, WORKER_PROTOCOL_VERSION, WasmProductSet};
pub use render_input_wire::{
    WasmCitationOutcome, WasmCitationSegment, WasmGeneratedBibliography,
    WasmGeneratedBibliographyEntry, WasmReferenceFailureKind, WasmReferenceNotice,
    WasmReferenceOutcome, WasmRenderInputs, WasmResolvedCitation, WasmResolvedReference,
    WasmResolvedResource, WasmResourceFailureKind, WasmResourceOutcome,
};
pub use request_enum_generated::{
    WasmDocumentMode, WasmSyntaxMode, WasmUnknownSourceLanguage,
    WasmUnresolvedReferencePresentation,
};
pub use request_wire::{
    WasmActiveUrlPolicy, WasmAnalysisOptions, WasmAuthoredUrlPolicy, WasmDiagnosticProfile,
    WasmExternalLinkPolicy, WasmLimits, WasmOutputLimits, WasmRenderPolicy, WasmRequest,
    WasmResourceCapabilities, WasmRuleSettings, WasmSourceLanguagePolicy, WasmStylesheet,
    WasmSyntaxOptions,
};
#[cfg(test)]
use response_projection::parse_optional_product;
use response_projection::{enforce_output_limit, project_response};
pub use response_wire::*;
pub use shared_wire_generated::{WasmMathLanguage, WasmSeverity};

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
    let snapshot = preprocess_wire::resource_snapshot(request.resources);
    let options =
        preprocess_wire::to_core_options(request.source_id.map(SourceId::new), request.options);
    let document = preprocess(&request.source, &snapshot, &options).map_err(|error| WasmError {
        code: error.kind.as_str().to_owned(),
        message: error.to_string(),
    })?;
    Ok(preprocess_projection::project(document))
}

pub fn process_request(
    request: WasmRequest,
    cancellation: &dyn CancellationCheck,
) -> Result<WasmResponse, WasmError> {
    let request = request_normalization::normalize(request)?;
    let request = request_conversion::convert(request)?;
    execute_request(request, cancellation)
}

fn execute_request(
    request: request_conversion::ExecutionRequest,
    cancellation: &dyn CancellationCheck,
) -> Result<WasmResponse, WasmError> {
    let (preprocessed_analysis, standalone_analysis) = match request.processing {
        request_conversion::ProcessingExecution::Combined { snapshot, options } => (
            Some(
                options
                    .preprocess_and_analyze(
                        &request.source,
                        &snapshot,
                        PreprocessInputs {
                            cancellation: Some(cancellation),
                        },
                    )
                    .map_err(preprocessed_analysis_error)?,
            ),
            None,
        ),
        request_conversion::ProcessingExecution::Standalone { engine } => (
            None,
            Some(
                engine
                    .analyze_with(
                        &request.source,
                        adocweave::AnalysisInputs {
                            source_id: request.source_id.as_ref(),
                            cancellation: Some(cancellation),
                        },
                    )
                    .map_err(wasm_error)?,
            ),
        ),
    };
    let analysis = preprocessed_analysis
        .as_ref()
        .map(|analysis| &analysis.analysis)
        .or(standalone_analysis.as_ref())
        .expect("exactly one processing variant is assigned");
    let attribute_projection = if request.requested_products.attribute_queries {
        preprocessed_analysis
            .as_ref()
            .map(|analysis| {
                analysis
                    .project_origins_cancellable(ProjectionLimits::default(), cancellation)
                    .map_err(|error| match error {
                        ProjectionFailure::LimitExceeded(error) => WasmError {
                            code: "limit-exceeded".to_owned(),
                            message: error.to_string(),
                        },
                        ProjectionFailure::Cancelled => cancelled_error(),
                    })
            })
            .transpose()?
    } else {
        None
    };
    if cancellation.is_cancelled() {
        return Err(cancelled_error());
    }

    let render_inputs = render_input_conversion::convert(request.render_inputs, analysis)?;
    let products = adocweave::output::conformance::products(
        analysis,
        &request.render_policy,
        &render_inputs,
        request.products,
    );
    if cancellation.is_cancelled() {
        return Err(cancelled_error());
    }
    let response = project_response(
        products,
        request.requested_products,
        request.version,
        request.generation,
        analysis,
        request.source_id.as_ref(),
        attribute_projection.as_ref(),
    )?;
    enforce_output_limit(&response, request.max_output_bytes)?;
    Ok(response)
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

fn preprocessed_analysis_error(error: PreprocessedAnalysisError) -> WasmError {
    match error {
        PreprocessedAnalysisError::Options(error) => WasmError {
            code: "invalid-options".to_owned(),
            message: error.to_string(),
        },
        PreprocessedAnalysisError::Preprocess(error) => WasmError {
            code: error.kind.as_str().to_owned(),
            message: error.to_string(),
        },
        PreprocessedAnalysisError::Parse(error) => wasm_error(error),
        PreprocessedAnalysisError::Cancelled => cancelled_error(),
    }
}

fn cancelled_error() -> WasmError {
    WasmError {
        code: "cancelled".to_owned(),
        message: "operation was cancelled".to_owned(),
    }
}

pub(crate) fn serialization_error(error: impl ToString) -> WasmError {
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
    use serde::Serialize;
    use serde::de::DeserializeOwned;
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
        let request = deserialize_request(request)?;
        let response = process_request(request, &JsCancellation(cancellation))
            .map_err(|error| JsValue::from_str(&serialize_error(&error)))?;
        response
            .serialize(&serde_wasm_bindgen::Serializer::json_compatible())
            .map_err(|error| JsValue::from_str(&serialize_error(&serialization_error(error))))
    }

    #[wasm_bindgen(js_name = preprocess)]
    pub fn preprocess_js(request: JsValue) -> Result<JsValue, JsValue> {
        let request = deserialize_request(request)?;
        let response = preprocess_request(request)
            .map_err(|error| JsValue::from_str(&serialize_error(&error)))?;
        response
            .serialize(&serde_wasm_bindgen::Serializer::json_compatible())
            .map_err(|error| JsValue::from_str(&serialize_error(&serialization_error(error))))
    }

    fn deserialize_request<T: DeserializeOwned>(request: JsValue) -> Result<T, JsValue> {
        let value: serde_json::Value =
            serde_wasm_bindgen::from_value(request).map_err(invalid_request)?;
        serde_json::from_value(value).map_err(invalid_request)
    }

    fn invalid_request(error: impl std::fmt::Display) -> JsValue {
        JsValue::from_str(&serialize_error(&WasmError {
            code: "invalid-request".to_owned(),
            message: error.to_string(),
        }))
    }
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, BTreeSet};

    use adocweave::output::diagnostics::{LintConfig, RuleSettings, Severity};
    use adocweave::preprocess::{PreprocessOptions, ResourceDocument, ResourceSnapshot};
    use adocweave::{AnalysisOptions, CancellationToken, DiagnosticProfile, Engine};
    use serde::de::DeserializeOwned;
    use serde_json::json;

    use super::*;
    use crate::preprocess_wire::{resource_snapshot, to_core_options as preprocess_options};

    fn request(source: &str) -> WasmRequest {
        WasmRequest {
            package_version: VERSION.to_owned(),
            source_id: Some("web:document".to_owned()),
            version: 3,
            generation: 7,
            source: source.to_owned(),
            preprocess: None,
            products: WasmProductSet {
                syntax: true,
                canonical_ast: true,
                html: true,
                attribute_occurrences: true,
                attribute_queries: true,
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
    fn generated_preprocess_defaults_match_the_core_adapter() {
        let core = preprocess_wire::to_core_options(None, WasmPreprocessOptions::default());
        let expected = PreprocessOptions::default();

        assert_eq!(core.source_id, expected.source_id);
        assert_eq!(core.base_uri, expected.base_uri);
        assert_eq!(core.safe_mode, expected.safe_mode);
        assert_eq!(core.allowed_schemes, expected.allowed_schemes);
        assert_eq!(core.attributes, expected.attributes);
        assert_eq!(core.enable_includes, expected.enable_includes);
        assert_eq!(core.max_include_depth, expected.max_include_depth);
        assert_eq!(core.max_includes, expected.max_includes);
        assert_eq!(core.max_total_bytes, expected.max_total_bytes);
        assert_eq!(core.max_expanded_nodes, expected.max_expanded_nodes);
        assert_eq!(
            core.max_source_map_segments,
            expected.max_source_map_segments
        );
        assert_eq!(
            core.max_attribute_expansion_depth,
            expected.max_attribute_expansion_depth
        );
        assert_eq!(
            core.max_attribute_expansion_bytes,
            expected.max_attribute_expansion_bytes
        );
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
        assert_eq!(response.symbols[0].name, "Title");
        assert_eq!(
            response
                .projection
                .as_ref()
                .expect("projection")
                .package_version,
            VERSION
        );
        assert_eq!(response.parse.reference_count, 0);
    }

    #[test]
    fn core_json_products_reject_unknown_fields_at_every_object_boundary() {
        let diagnostics = json!([{
            "id": "diagnostic",
            "code": "example",
            "severity": "warning",
            "message": "message",
            "range": { "start": 0, "end": 1 },
            "related": [{
                "range": { "start": 1, "end": 2 },
                "message": "related"
            }],
            "fixes": [{
                "title": "fix",
                "applicability": "always",
                "edits": [{
                    "range": { "start": 0, "end": 1 },
                    "replacement": "replacement"
                }]
            }]
        }]);
        assert_product_rejects_unknown_fields::<Vec<WasmDiagnostic>>("diagnostics", &diagnostics);

        let symbols: serde_json::Value = serde_json::from_str(include_str!(
            "../../../fixtures/conformance/full.symbols.json"
        ))
        .expect("symbol fixture");
        assert_product_rejects_unknown_fields::<Vec<WasmDocumentSymbol>>("symbols", &symbols);

        let mut projection: serde_json::Value = serde_json::from_str(include_str!(
            "../../../fixtures/conformance/full.projection.json"
        ))
        .expect("projection fixture");
        projection["sourceBlocks"] = json!([{
            "sourceRange": { "start": 0, "end": 4 },
            "contentRange": { "start": 1, "end": 3 },
            "title": {
                "sourceRange": { "start": 0, "end": 1 },
                "text": "source"
            },
            "languageRange": { "start": 1, "end": 2 },
            "language": "rust",
            "lineNumbers": true,
            "startLine": 3,
            "source": "fn main() {}"
        }]);
        projection["orderedLists"] = json!([{
            "sourceRange": { "start": 0, "end": 4 },
            "start": 1,
            "reversed": false,
            "style": "arabic"
        }]);
        projection["blockPresentations"] = json!([{
            "kind": "admonition",
            "sourceRange": { "start": 0, "end": 4 },
            "contentRange": { "start": 1, "end": 3 },
            "title": "Note",
            "attribution": null,
            "citation": null
        }]);
        projection["structure"]["manpage"] = json!({
            "name": "tool",
            "section": "1",
            "purpose": "purpose",
            "titleRange": { "start": 0, "end": 4 },
            "nameRange": { "start": 0, "end": 1 },
            "purposeRange": { "start": 2, "end": 4 }
        });
        projection["catalogs"]["footnotes"] = json!([{
            "number": 1,
            "id": "note",
            "definitionRange": { "start": 0, "end": 4 },
            "contentRange": { "start": 1, "end": 3 },
            "text": "note",
            "occurrences": [{ "start": 0, "end": 4 }]
        }]);
        projection["catalogs"]["bibliography"] = json!([{
            "id": "reference",
            "definitionRange": { "start": 0, "end": 4 },
            "references": [{ "start": 1, "end": 2 }]
        }]);
        projection["catalogs"]["index"] = json!([{
            "terms": ["term"],
            "display": "term",
            "occurrences": [{ "start": 1, "end": 2 }]
        }]);
        projection["referenceEdges"][0]["resolution"] = json!({
            "status": "resolved",
            "href": "#target",
            "displayText": "target",
            "notices": ["reference-resolution-fallback"]
        });
        projection["referenceEdges"][1]["resolution"] = json!({
            "status": "failed",
            "kind": "missing-reference-target"
        });

        let target_kinds = projection["referenceEdges"]
            .as_array()
            .expect("reference edges")
            .iter()
            .map(|edge| edge["target"]["kind"].as_str().expect("target kind"))
            .collect::<BTreeSet<_>>();
        assert_eq!(
            target_kinds,
            BTreeSet::from(["document", "local", "scheme"])
        );
        let resolution_statuses = projection["referenceEdges"]
            .as_array()
            .expect("reference edges")
            .iter()
            .filter_map(|edge| edge["resolution"]["status"].as_str())
            .collect::<BTreeSet<_>>();
        assert_eq!(resolution_statuses, BTreeSet::from(["failed", "resolved"]));
        assert_product_rejects_unknown_fields::<WasmDocumentProjection>("projection", &projection);
    }

    fn assert_product_rejects_unknown_fields<T>(name: &str, product: &serde_json::Value)
    where
        T: DeserializeOwned,
    {
        let source = serde_json::to_string(product).expect("product JSON");
        parse_optional_product::<T>(Some(&source))
            .unwrap_or_else(|error| panic!("{name} fixture must be valid: {}", error.message));

        let mut pointers = Vec::new();
        collect_object_pointers(product, "", &mut pointers);
        assert!(
            !pointers.is_empty(),
            "{name} must contain object boundaries"
        );
        for pointer in pointers {
            let mut mutated = product.clone();
            mutated
                .pointer_mut(&pointer)
                .and_then(serde_json::Value::as_object_mut)
                .expect("object boundary")
                .insert("unknownField".to_owned(), json!(true));
            let source = serde_json::to_string(&mutated).expect("mutated product JSON");

            let error = match parse_optional_product::<T>(Some(&source)) {
                Ok(_) => panic!("{name}{pointer}: unknown field must fail"),
                Err(error) => error,
            };
            assert_eq!(error.code, "serialization-failed", "{name}{pointer}");
            assert!(
                error.message.contains("unknown field `unknownField`"),
                "{name}{pointer}: {}",
                error.message
            );
        }
    }

    fn collect_object_pointers(value: &serde_json::Value, pointer: &str, output: &mut Vec<String>) {
        match value {
            serde_json::Value::Object(object) => {
                output.push(pointer.to_owned());
                for (field, value) in object {
                    collect_object_pointers(value, &format!("{pointer}/{field}"), output);
                }
            }
            serde_json::Value::Array(array) => {
                for (index, value) in array.iter().enumerate() {
                    collect_object_pointers(value, &format!("{pointer}/{index}"), output);
                }
            }
            _ => {}
        }
    }

    #[test]
    fn wasm_default_product_set_omits_unused_canonical_products() {
        let mut request = request("= Title\n\nText");
        request.products = WasmProductSet::default();
        let response = process_request(request, &NeverCancel).expect("response");

        assert!(response.syntax.is_empty());
        assert!(response.ast.is_empty());
        assert!(response.attribute_occurrences.is_empty());
        assert!(response.symbols.is_empty());
        assert!(!response.html.is_empty());
        assert!(response.projection.is_some());
    }

    #[test]
    fn wasm_api_exposes_source_preserving_document_attribute_occurrences() {
        let source = include_str!("../../../fixtures/attributes/public-occurrences.adoc");
        let response = process_request(request(source), &NeverCancel).expect("response");

        assert_eq!(response.attribute_occurrences.len(), 5);
        assert_eq!(response.attribute_occurrences[0].name, "duplicate");
        assert_eq!(response.attribute_occurrences[0].value.source_text, "first");
        assert_eq!(
            response.attribute_occurrences[1].operation,
            WasmDocumentAttributeOperation::Set
        );
        assert_eq!(response.attribute_occurrences[2].value.source_text, "");
        assert_eq!(
            response.attribute_occurrences[3].operation,
            WasmDocumentAttributeOperation::Unset
        );
        assert_eq!(
            response.attribute_occurrences[4].operation,
            WasmDocumentAttributeOperation::Unset
        );
        assert!(
            response.attribute_occurrences[2].value.source_range.start
                == response.attribute_occurrences[2].value.source_range.end
        );
        assert_eq!(
            &source[usize::try_from(response.attribute_occurrences[0].range.start).expect("offset")
                ..usize::try_from(response.attribute_occurrences[0].range.end).expect("offset")],
            ":duplicate: first\n"
        );

        let multiline = process_request(
            request(include_str!(
                "../../../fixtures/attributes/multiline-soft-hard.adoc"
            )),
            &NeverCancel,
        )
        .expect("multiline response");
        assert_eq!(
            multiline.attribute_occurrences[1].value.folded_text,
            "first line +\nsecond line +\nthird line"
        );
        assert_eq!(multiline.attribute_occurrences[1].value.lines.len(), 3);
        assert_eq!(
            multiline.attribute_occurrences[1].value.lines[0]
                .continuation
                .expect("continuation")
                .kind,
            WasmAttributeValueContinuation::Hard
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
        assert!(response.render_diagnostics.is_empty());

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
            unsafe_response.render_diagnostics[0].code,
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
        assert!(root_relative.render_diagnostics.is_empty());

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
    fn wasm_render_inputs_preserve_missing_failed_and_duplicate_semantics() {
        let reference_source = "xref:target[ref]";
        let missing_reference =
            process_request(request(reference_source), &NeverCancel).expect("missing reference");
        assert!(
            missing_reference
                .render_diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code == "unresolved-cross-reference")
        );

        let failed_reference = WasmResolvedReference {
            source_start: 0,
            source_end: reference_source.len() as u32,
            outcome: WasmReferenceOutcome::Failed {
                kind: WasmReferenceFailureKind::MissingTarget,
            },
        };
        let mut failed_reference_request = request(reference_source);
        failed_reference_request
            .render_inputs
            .references
            .push(failed_reference.clone());
        let failed_reference_response =
            process_request(failed_reference_request, &NeverCancel).expect("failed reference");
        assert!(
            failed_reference_response
                .render_diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code == "missing-reference-target")
        );

        let mut duplicate_reference_request = request(reference_source);
        duplicate_reference_request.render_inputs.references =
            vec![failed_reference.clone(), failed_reference];
        let duplicate_reference = process_request(duplicate_reference_request, &NeverCancel)
            .expect("duplicate reference");
        assert!(
            duplicate_reference
                .render_diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code == "duplicate-render-input")
        );

        let resource_source = "image:asset.png[alt]";
        let missing_resource =
            process_request(request(resource_source), &NeverCancel).expect("missing resource");
        assert!(
            missing_resource
                .render_diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code == "unresolved-resource")
        );

        let failed_resource = WasmResolvedResource {
            source_start: 0,
            source_end: resource_source.len() as u32,
            outcome: WasmResourceOutcome::Failed {
                kind: WasmResourceFailureKind::Missing,
            },
        };
        let mut failed_resource_request = request(resource_source);
        failed_resource_request
            .render_inputs
            .resources
            .push(failed_resource.clone());
        let failed_resource_response =
            process_request(failed_resource_request, &NeverCancel).expect("failed resource");
        assert!(
            failed_resource_response
                .render_diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code == "missing-resource")
        );

        let mut duplicate_resource_request = request(resource_source);
        duplicate_resource_request.render_inputs.resources =
            vec![failed_resource.clone(), failed_resource];
        let duplicate_resource =
            process_request(duplicate_resource_request, &NeverCancel).expect("duplicate resource");
        assert!(
            duplicate_resource
                .render_diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code == "duplicate-render-input")
        );
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
                    .iter()
                    .any(|diagnostic| diagnostic.code == "invalid-url-scheme"),
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
        let Some(WasmProjectedResolutionOutcome::Resolved {
            display_text: Some(display_text),
            ..
        }) = &response
            .projection
            .as_ref()
            .expect("projection")
            .reference_edges[0]
            .resolution
        else {
            panic!("resolved projection");
        };
        assert_eq!(display_text, "公開 <タイトル> & *not markup*");

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
        assert_eq!(
            response.projection.as_ref().expect("projection").formulas[0].source,
            "x"
        );
        let codes = response
            .render_diagnostics
            .iter()
            .map(|diagnostic| diagnostic.code.as_str())
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
        assert!(response.render_diagnostics.is_empty());

        let mut fragment = request("paragraph");
        fragment.render_policy.stylesheets = vec![WasmStylesheet::Inline {
            css: "p {}".to_owned(),
        }];
        let response = process_request(fragment, &NeverCancel).expect("response");
        assert_eq!(response.html, "<p>paragraph</p>\n");
        assert_eq!(
            response.render_diagnostics[0].code,
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
            .iter()
            .map(|diagnostic| diagnostic.code.as_str())
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
    fn wasm_combined_processing_propagates_preprocessing_cancellation() {
        let mut combined = request("include::part.adoc[]\n");
        combined.preprocess = Some(WasmAnalysisPreprocessInput {
            resources: BTreeMap::from([(
                "part.adoc".to_owned(),
                WasmResource {
                    source_id: "part".to_owned(),
                    source: "included\n".to_owned(),
                },
            )]),
            options: WasmPreprocessOptions::default(),
        });
        let cancellation = CancellationToken::new();
        cancellation.cancel();

        let error = process_request(combined, &cancellation).expect_err("cancelled");

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
        assert_eq!(response.diagnostics[0].code, "trailing-whitespace");
        assert_eq!(response.diagnostics[0].severity, WasmSeverity::Error);

        let mut protected = request(":locked: changed\n");
        protected
            .analysis_options
            .diagnostics
            .protected_attributes
            .insert("locked".to_owned(), Some("expected".to_owned()));
        let response = process_request(protected, &NeverCancel).expect("protected attribute");
        let diagnostic = response
            .diagnostics
            .iter()
            .find(|diagnostic| diagnostic.code == "protected-attribute")
            .expect("protected attribute diagnostic");
        assert_eq!(diagnostic.severity, WasmSeverity::Error);

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
    fn wasm_diagnostic_limit_is_applied_before_wire_projection() {
        let mut configured = request("trailing \n*x\n");
        configured.analysis_options.diagnostics.max_diagnostics = 1;

        let response = process_request(configured, &NeverCancel).expect("bounded diagnostics");
        assert_eq!(response.diagnostics.len(), 1);
        assert_eq!(response.diagnostics[0].code, "trailing-whitespace");
    }

    #[test]
    fn opt_in_macro_boundary_matches_the_native_diagnostic_contract() {
        let source = "本文xref:guide.adoc[Guide]\n";
        let default_response =
            process_request(request(source), &NeverCancel).expect("default diagnostics");
        assert!(
            default_response
                .diagnostics
                .iter()
                .all(|diagnostic| diagnostic.code != "macro-boundary")
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
            .iter()
            .find(|diagnostic| diagnostic.code == "macro-boundary")
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

        assert_eq!(wasm.code, native.code.as_str());
        assert_eq!(wasm.severity, WasmSeverity::Warning);
        assert_eq!(wasm.range.start, native.range.start().to_u32());
        assert_eq!(wasm.range.end, native.range.end().to_u32());
    }

    #[test]
    fn preprocessing_uses_the_same_snapshot_model_as_the_native_core() {
        let resources = BTreeMap::from([(
            "parts/intro.adoc".to_owned(),
            WasmResource {
                source_id: "intro".to_owned(),
                source: "== Intro\n".into(),
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
        assert_eq!(
            response.source_map[0].mapping,
            WasmSourceMapping::WholeOrigin
        );

        let mut native_snapshot = ResourceSnapshot::default();
        native_snapshot.insert(
            "parts/intro.adoc",
            ResourceDocument {
                source_id: SourceId::new("intro"),
                source: "== Intro\n".into(),
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

    #[test]
    fn public_preprocess_fixture_is_identical_in_native_and_wasm_adapters() {
        let fixture: serde_json::Value = serde_json::from_str(include_str!(
            "../../../fixtures/preprocessor/public-v1.json"
        ))
        .expect("public preprocess fixture");
        let request: WasmPreprocessRequest = serde_json::from_value(json!({
            "packageVersion": VERSION,
            "sourceId": fixture["sourceId"],
            "source": fixture["source"],
            "resources": fixture["resources"],
            "options": fixture["options"],
        }))
        .expect("fixture request");
        let native_snapshot = resource_snapshot(request.resources.clone());
        let native_options = preprocess_options(
            request.source_id.clone().map(SourceId::new),
            request.options.clone(),
        );
        let native = preprocess(&request.source, &native_snapshot, &native_options)
            .expect("native preprocessing");
        let wasm = preprocess_request(request).expect("WASM preprocessing");

        assert_eq!(wasm.source, native.source);
        assert_eq!(wasm.source_map.len(), native.source_map().len());
        for (wasm, native) in wasm.source_map.iter().zip(native.source_map()) {
            assert_eq!(wasm.output_start, native.output_range.start().to_u32());
            assert_eq!(wasm.output_end, native.output_range.end().to_u32());
            assert_eq!(
                wasm.source_id.as_deref(),
                native.origin.source_id.as_ref().map(SourceId::as_str)
            );
            assert_eq!(wasm.source_start, native.origin.range.start().to_u32());
            assert_eq!(wasm.source_end, native.origin.range.end().to_u32());
            assert_eq!(
                wasm.mapping,
                match native.mapping {
                    adocweave::preprocess::SourceMapping::Identity => WasmSourceMapping::Identity,
                    adocweave::preprocess::SourceMapping::WholeOrigin => {
                        WasmSourceMapping::WholeOrigin
                    }
                }
            );
        }
    }

    #[test]
    fn every_preprocess_limit_has_the_same_native_and_wasm_error_code() {
        type LimitCase = (&'static str, fn(&mut WasmPreprocessOptions), &'static str);

        let resource = WasmResource {
            source_id: "part".to_owned(),
            source: "text".into(),
        };
        let cases: [LimitCase; 7] = [
            (
                "include::part.adoc[]\n",
                |options| options.max_include_depth = 0,
                "depth-limit",
            ),
            (
                "include::part.adoc[]\n",
                |options| options.max_includes = 0,
                "include-limit",
            ),
            ("text", |options| options.max_total_bytes = 3, "byte-limit"),
            (
                "text",
                |options| options.max_expanded_nodes = 0,
                "node-limit",
            ),
            (
                "text",
                |options| options.max_source_map_segments = 0,
                "source-map-limit",
            ),
            (
                ":base: 12345\n:expanded: {base}\ninclude::{expanded}.adoc[]\n",
                |options| options.max_attribute_expansion_depth = 0,
                "missing-resource",
            ),
            (
                ":base: 12345\n:expanded: {base}\ninclude::{expanded}.adoc[]\n",
                |options| options.max_attribute_expansion_bytes = 4,
                "missing-resource",
            ),
        ];
        for (source, configure, expected) in cases {
            let mut options = WasmPreprocessOptions::default();
            configure(&mut options);
            let resources = BTreeMap::from([("part.adoc".to_owned(), resource.clone())]);
            let native_snapshot = resource_snapshot(resources.clone());
            let native_options = preprocess_options(None, options.clone());
            let native =
                preprocess(source, &native_snapshot, &native_options).expect_err("native limit");
            let wasm = preprocess_request(WasmPreprocessRequest {
                package_version: VERSION.to_owned(),
                source_id: None,
                source: source.to_owned(),
                resources,
                options,
            })
            .expect_err("WASM limit");

            assert_eq!(native.kind.as_str(), expected);
            assert_eq!(wasm.code, expected);
        }
    }

    #[test]
    fn analysis_and_preprocess_attribute_inputs_cannot_diverge() {
        for mismatch in 0..3 {
            let mut request = request("include::missing.adoc[]\n");
            request.preprocess = Some(WasmAnalysisPreprocessInput {
                resources: BTreeMap::new(),
                options: WasmPreprocessOptions::default(),
            });
            match mismatch {
                0 => {
                    request
                        .analysis_options
                        .attributes
                        .insert("locked".to_owned(), Some("analysis".to_owned()));
                }
                1 => {
                    request
                        .analysis_options
                        .syntax
                        .limits
                        .max_attribute_expansion_depth += 1;
                }
                2 => {
                    request
                        .analysis_options
                        .syntax
                        .limits
                        .max_attribute_expansion_bytes += 1;
                }
                _ => unreachable!(),
            }

            let error =
                process_request(request, &NeverCancel).expect_err("conflicting processing options");
            assert_eq!(error.code, "invalid-options");
        }
    }

    #[test]
    fn wasm_combined_processing_uses_one_external_attribute_contract() {
        let attributes = BTreeMap::from([("selected".to_owned(), Some("part".to_owned()))]);
        let mut request = request("ifdef::selected[]\ninclude::{selected}.adoc[]\nendif::[]\n");
        request.analysis_options.attributes.clone_from(&attributes);
        request.preprocess = Some(WasmAnalysisPreprocessInput {
            resources: BTreeMap::from([(
                "part.adoc".to_owned(),
                WasmResource {
                    source_id: "part".to_owned(),
                    source: ":selected: other\nincluded {selected}\n".to_owned(),
                },
            )]),
            options: WasmPreprocessOptions {
                attributes,
                ..WasmPreprocessOptions::default()
            },
        });

        let response = process_request(request, &NeverCancel).expect("combined processing");

        assert!(response.html.contains("included part"));
        assert!(
            response
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code == "protected-attribute")
        );
    }

    #[test]
    fn wasm_combined_processing_applies_matching_non_default_attribute_expansion_limits() {
        let source = ":base: 12345\n:expanded: {base}\ninclude::{expanded}.adoc[]\n";
        for (depth, bytes, accepted) in [(1, 5, true), (0, 5, false), (1, 4, false)] {
            let mut request = request(source);
            request
                .analysis_options
                .syntax
                .limits
                .max_attribute_expansion_depth = depth;
            request
                .analysis_options
                .syntax
                .limits
                .max_attribute_expansion_bytes = bytes;
            request.preprocess = Some(WasmAnalysisPreprocessInput {
                resources: BTreeMap::from([(
                    "12345.adoc".to_owned(),
                    WasmResource {
                        source_id: "included".to_owned(),
                        source: "analysis {expanded}\n".to_owned(),
                    },
                )]),
                options: WasmPreprocessOptions {
                    max_attribute_expansion_depth: depth,
                    max_attribute_expansion_bytes: bytes,
                    ..WasmPreprocessOptions::default()
                },
            });

            let result = process_request(request, &NeverCancel);
            if accepted {
                let response = result.expect("matching boundary is accepted");
                assert!(
                    response.html.contains("analysis 12345"),
                    "analysis must use the same non-default expansion limits"
                );
            } else {
                assert_eq!(
                    result
                        .expect_err("matching strict boundary is enforced")
                        .code,
                    "missing-resource"
                );
            }
        }
    }
}
