//! Versioned, allocation-owning WASM boundary over the deterministic core.

use adocweave::diagnostic::render_json as render_diagnostics_json;
use adocweave::document::{document_symbols, render_symbols_json};
use adocweave::html::{RenderPolicy, render};
use adocweave::projection::project;
use adocweave::{CancellationCheck, Engine, NeverCancel, ParseError, ParseOptions, SourceId};
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const WASM_API_VERSION: u16 = 1;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WasmRequest {
    pub api_version: u16,
    pub source_id: Option<String>,
    pub version: u32,
    pub generation: u32,
    pub source: String,
}

#[derive(Clone, Debug, Serialize, Eq, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct WasmResponse {
    pub api_version: u16,
    pub version: u32,
    pub generation: u32,
    pub parse: ParseSummary,
    pub html: String,
    pub diagnostics: Value,
    pub symbols: Value,
    pub projection: Value,
}

#[derive(Clone, Debug, Serialize, Eq, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ParseSummary {
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
    let analysis = Engine::new(ParseOptions {
        source_id: request.source_id.map(SourceId::new),
        ..ParseOptions::default()
    })
    .analyze_cancellable(&request.source, cancellation)
    .map_err(wasm_error)?;
    if cancellation.is_cancelled() {
        return Err(cancelled_error());
    }

    let html = render(&analysis.ast, &RenderPolicy::default()).html;
    let diagnostics = serde_json::from_str(&render_diagnostics_json(&analysis.diagnostics))
        .map_err(serialization_error)?;
    let symbols = serde_json::from_str(&render_symbols_json(&document_symbols(&analysis.ast)))
        .map_err(serialization_error)?;
    let projection = serde_json::from_str(&project(&analysis, &[]).render_json())
        .map_err(serialization_error)?;
    if cancellation.is_cancelled() {
        return Err(cancelled_error());
    }

    Ok(WasmResponse {
        api_version: WASM_API_VERSION,
        version: request.version,
        generation: request.generation,
        parse: ParseSummary {
            block_count: analysis.ast.blocks.len(),
            node_count: analysis.ast.node_count(),
            reference_count: analysis.references.len(),
        },
        html,
        diagnostics,
        symbols,
        projection,
    })
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
        serde_wasm_bindgen::to_value(&response)
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
        }
    }

    #[test]
    fn wasm_api_returns_all_products_from_one_versioned_request() {
        let response =
            process_request(request("= Title\n\n== Section\n"), &NeverCancel).expect("response");

        assert_eq!(response.version, 3);
        assert_eq!(response.generation, 7);
        assert!(response.html.contains("<h1"));
        assert_eq!(response.symbols[0]["name"], "Title");
        assert_eq!(response.projection["contractVersion"], 1);
        assert_eq!(response.parse.reference_count, 0);
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
        let source = "x".repeat(ParseOptions::default().limits.max_input_bytes + 1);
        let error = process_request(request(&source), &NeverCancel).expect_err("limit");
        assert_eq!(error.code, "limit-exceeded");
    }
}
