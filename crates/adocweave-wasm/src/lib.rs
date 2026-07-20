//! Versioned, allocation-owning WASM boundary over the deterministic core.

use std::collections::{BTreeMap, BTreeSet};

use adocweave::conformance::{CONFORMANCE_CONTRACT_VERSION, snapshot};
use adocweave::html::RenderPolicy;
use adocweave::limits::{ProcessingLimits, SyntaxMode};
use adocweave::url::UrlPolicy;
use adocweave::{
    CORE_PROFILE_VERSION, CancellationCheck, Engine, NeverCancel, ParseError, ParseOptions,
    SourceId, SyntaxProfile,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const WASM_API_VERSION: u16 = 2;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WasmRequest {
    pub api_version: u16,
    pub source_id: Option<String>,
    pub version: u32,
    pub generation: u32,
    pub source: String,
    #[serde(default)]
    pub options: WasmOptions,
}

#[derive(Clone, Debug, Deserialize, Serialize, Eq, PartialEq)]
#[serde(default, rename_all = "camelCase", deny_unknown_fields)]
pub struct WasmOptions {
    pub profile_version: u16,
    pub syntax_mode: WasmSyntaxMode,
    pub limits: WasmLimits,
    pub protected_attributes: BTreeMap<String, String>,
    pub url_policy: WasmUrlPolicy,
}

impl Default for WasmOptions {
    fn default() -> Self {
        Self {
            profile_version: CORE_PROFILE_VERSION,
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
    pub max_inline_depth: u32,
    pub max_formula_bytes: u32,
    pub max_blocks: u32,
    pub max_nodes: u32,
    pub max_references: u32,
    pub max_attributes: u32,
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
            max_inline_depth: value.max_inline_depth,
            max_formula_bytes: value.max_formula_bytes,
            max_blocks: value.max_blocks,
            max_nodes: value.max_nodes,
            max_references: value.max_references,
            max_attributes: value.max_attributes,
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
            max_inline_depth: value.max_inline_depth,
            max_formula_bytes: value.max_formula_bytes,
            max_blocks: value.max_blocks,
            max_nodes: value.max_nodes,
            max_references: value.max_references,
            max_attributes: value.max_attributes,
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
    pub cst: String,
    pub ast: String,
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
    let options = request.options;
    let max_output_bytes = usize::try_from(options.limits.max_output_bytes)
        .expect("u32 fits usize on supported targets");
    let analysis = Engine::new(ParseOptions {
        source_id: request.source_id.map(SourceId::new),
        profile: SyntaxProfile {
            version: options.profile_version,
            mode: match options.syntax_mode {
                WasmSyntaxMode::Permissive => SyntaxMode::Permissive,
                WasmSyntaxMode::Strict => SyntaxMode::Strict,
            },
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

    let products = snapshot(&analysis, &RenderPolicy::default(), &[]);
    let diagnostics =
        serde_json::from_str(&products.diagnostics_json).map_err(serialization_error)?;
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
            block_count: analysis.ast.blocks.len(),
            node_count: analysis.ast.node_count(),
            reference_count: analysis.references.len(),
        },
        cst: products.cst,
        ast: products.ast,
        html: products.html,
        diagnostics,
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
            options: WasmOptions::default(),
        }
    }

    #[test]
    fn wasm_api_returns_all_products_from_one_versioned_request() {
        let response =
            process_request(request("= Title\n\n== Section\n"), &NeverCancel).expect("response");

        assert_eq!(response.version, 3);
        assert_eq!(response.generation, 7);
        assert_eq!(response.conformance_contract_version, 3);
        assert!(response.cst.contains("Document@"));
        assert!(response.ast.contains("\"blocks\""));
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
        assert_eq!(request.options.profile_version, CORE_PROFILE_VERSION);
        assert_eq!(request.options.limits.max_input_bytes, 10 * 1024 * 1024);
        let error = process_request(request, &NeverCancel).expect_err("output limit");
        assert_eq!(error.code, "limit-exceeded");
    }
}
