//! Node.js-only WebAssembly request, resource-limit, and binding boundary.

use adocweave::{AnalysisInputs, AnalysisOptions, Engine, SourceId, VERSION};
use adocweave_textlint::{PlanError, PlanLimits, TxtAstPlan, plan};
use serde::{Deserialize, Serialize};

/// Maximum accepted AsciiDoc input size.
pub const MAX_INPUT_BYTES: usize = 10 * 1024 * 1024;
/// Maximum serialized plan size.
pub const MAX_OUTPUT_BYTES: usize = 50 * 1024 * 1024;
/// Maximum number of TxtAST nodes, including the document root.
pub const MAX_PLAN_NODES: usize = 1_000_000;
/// Maximum accepted logical source identifier size.
pub const MAX_SOURCE_ID_BYTES: usize = 4 * 1024;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ParseTextRequest {
    pub component_version: String,
    pub source_id: Option<String>,
    pub source: String,
}

#[derive(Clone, Debug, Serialize, Eq, PartialEq)]
pub struct ParseTextError {
    pub code: String,
    pub message: String,
}

impl ParseTextError {
    fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
        }
    }
}

pub fn parse_text_request(request: ParseTextRequest) -> Result<TxtAstPlan, ParseTextError> {
    parse_text_request_with_limits(request, MAX_INPUT_BYTES, MAX_OUTPUT_BYTES, MAX_PLAN_NODES)
}

fn parse_text_request_with_limits(
    request: ParseTextRequest,
    max_input_bytes: usize,
    max_output_bytes: usize,
    max_plan_nodes: usize,
) -> Result<TxtAstPlan, ParseTextError> {
    if request.component_version != VERSION {
        return Err(ParseTextError::new(
            "component-version-mismatch",
            format!(
                "component version {} does not match {VERSION}",
                request.component_version
            ),
        ));
    }
    if request.source.len() > max_input_bytes {
        return Err(ParseTextError::new(
            "input-too-large",
            format!("input exceeds the {max_input_bytes} byte limit"),
        ));
    }
    if request
        .source_id
        .as_ref()
        .is_some_and(|source_id| source_id.len() > MAX_SOURCE_ID_BYTES)
    {
        return Err(ParseTextError::new(
            "invalid-request",
            format!("sourceId exceeds the {MAX_SOURCE_ID_BYTES} byte limit"),
        ));
    }

    let source_id = request.source_id.map(SourceId::new);
    let analysis = Engine::new(AnalysisOptions::default())
        .analyze_with(
            &request.source,
            AnalysisInputs {
                source_id: source_id.as_ref(),
                cancellation: None,
            },
        )
        .map_err(|error| ParseTextError::new(error.code().as_str(), error.to_string()))?;
    let result = plan(
        &analysis,
        PlanLimits {
            max_nodes: max_plan_nodes,
        },
    )
    .map_err(plan_error)?;
    let mut output = LimitedWriter::new(max_output_bytes);
    if let Err(error) = serde_json::to_writer(&mut output, &result)
        && !output.exceeded
    {
        return Err(ParseTextError::new(
            "serialization-failed",
            error.to_string(),
        ));
    }
    if output.exceeded {
        return Err(ParseTextError::new(
            "output-too-large",
            format!("output exceeds the {max_output_bytes} byte limit"),
        ));
    }
    Ok(result)
}

struct LimitedWriter {
    written: usize,
    max: usize,
    exceeded: bool,
}

impl LimitedWriter {
    const fn new(max: usize) -> Self {
        Self {
            written: 0,
            max,
            exceeded: false,
        }
    }
}

impl std::io::Write for LimitedWriter {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        let Some(next) = self.written.checked_add(bytes.len()) else {
            self.exceeded = true;
            return Err(std::io::Error::other("serialized output limit exceeded"));
        };
        if next > self.max {
            self.exceeded = true;
            return Err(std::io::Error::other("serialized output limit exceeded"));
        }
        self.written = next;
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn plan_error(error: PlanError) -> ParseTextError {
    let code = match error {
        PlanError::NodeLimitExceeded { .. } => "node-limit",
        PlanError::InvalidSourceRange
        | PlanError::OverlappingSiblings
        | PlanError::InvalidNodeHierarchy => "invalid-plan",
    };
    ParseTextError::new(code, error.to_string())
}

#[cfg(any(test, target_arch = "wasm32"))]
fn serialize_error(error: &ParseTextError) -> String {
    serde_json::to_string(error).unwrap_or_else(|_| {
        "{\"code\":\"serialization-failed\",\"message\":\"failed to serialize error\"}".to_owned()
    })
}

#[cfg(target_arch = "wasm32")]
mod bindings {
    use serde::de::DeserializeOwned;
    use wasm_bindgen::prelude::*;

    use super::*;

    #[wasm_bindgen(js_name = parseText)]
    pub fn parse_text_js(request: JsValue) -> Result<JsValue, JsValue> {
        let request = deserialize_request(request)?;
        let response = parse_text_request(request)
            .map_err(|error| JsValue::from_str(&serialize_error(&error)))?;
        response
            .serialize(&serde_wasm_bindgen::Serializer::json_compatible())
            .map_err(|error| {
                let error = ParseTextError::new("serialization-failed", error.to_string());
                JsValue::from_str(&serialize_error(&error))
            })
    }

    fn deserialize_request<T: DeserializeOwned>(request: JsValue) -> Result<T, JsValue> {
        let value: serde_json::Value =
            serde_wasm_bindgen::from_value(request).map_err(|error| {
                invalid_request(format!("request is not a JSON-compatible value: {error}"))
            })?;
        serde_json::from_value(value).map_err(|error| invalid_request(error.to_string()))
    }

    fn invalid_request(message: String) -> JsValue {
        JsValue::from_str(&serialize_error(&ParseTextError::new(
            "invalid-request",
            message,
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use adocweave_textlint::{DocumentType, TxtAstNode, Utf16Range};

    fn request(source: &str) -> ParseTextRequest {
        ParseTextRequest {
            component_version: VERSION.to_owned(),
            source_id: Some("docs:test.adoc".to_owned()),
            source: source.to_owned(),
        }
    }

    #[test]
    fn returns_a_typed_txtast_plan() {
        let source = "= 文書\n\n本文です。\n";
        let response = parse_text_request(request(source)).expect("plan");
        assert_eq!(response.node_type, DocumentType::Document);
        assert_eq!(
            response.range,
            Utf16Range(0, source.encode_utf16().count() as u32)
        );
        assert!(matches!(
            response.children[0],
            TxtAstNode::Header { depth: 1, .. }
        ));
        assert!(matches!(response.children[1], TxtAstNode::Paragraph { .. }));
        let json = serde_json::to_value(&response).expect("serialized plan");
        assert_eq!(json["type"], "Document");
        assert!(json.get("componentVersion").is_none());
        assert!(json.get("sourceId").is_none());
        assert_eq!(json["children"][0]["type"], "Header");
        assert!(json["children"][0].get("depth").is_some());
    }

    #[test]
    fn rejects_a_different_component_version() {
        let mut request = request("");
        request.component_version = "0.0.0".to_owned();
        let error = parse_text_request(request).expect_err("version mismatch");
        assert_eq!(error.code, "component-version-mismatch");
    }

    #[test]
    fn applies_all_boundary_limits() {
        let input =
            parse_text_request_with_limits(request("x"), 0, MAX_OUTPUT_BYTES, MAX_PLAN_NODES)
                .expect_err("input limit");
        assert_eq!(input.code, "input-too-large");

        let output =
            parse_text_request_with_limits(request("x"), MAX_INPUT_BYTES, 0, MAX_PLAN_NODES)
                .expect_err("output limit");
        assert_eq!(output.code, "output-too-large");

        let nodes =
            parse_text_request_with_limits(request("本文"), MAX_INPUT_BYTES, MAX_OUTPUT_BYTES, 0)
                .expect_err("node limit");
        assert_eq!(nodes.code, "node-limit");

        let mut source_id = request("");
        source_id.source_id = Some("x".repeat(MAX_SOURCE_ID_BYTES + 1));
        let source_id = parse_text_request(source_id).expect_err("source identifier limit");
        assert_eq!(source_id.code, "invalid-request");
    }

    #[test]
    fn output_limit_accepts_the_exact_serialized_size() {
        let expected = parse_text_request(request("本文")).expect("plan");
        let exact = serde_json::to_vec(&expected)
            .expect("serialized plan")
            .len();
        parse_text_request_with_limits(request("本文"), MAX_INPUT_BYTES, exact, MAX_PLAN_NODES)
            .expect("exact output limit");
        let error = parse_text_request_with_limits(
            request("本文"),
            MAX_INPUT_BYTES,
            exact - 1,
            MAX_PLAN_NODES,
        )
        .expect_err("one byte below output size");
        assert_eq!(error.code, "output-too-large");
    }

    #[test]
    fn request_rejects_unknown_fields() {
        let error = serde_json::from_value::<ParseTextRequest>(serde_json::json!({
            "componentVersion": VERSION,
            "sourceId": null,
            "source": "",
            "unknown": true
        }))
        .expect_err("unknown field");
        assert!(error.to_string().contains("unknown field"));
    }

    #[test]
    fn errors_have_a_stable_serializable_shape() {
        let error = ParseTextError::new("invalid-request", "invalid request");
        assert_eq!(
            serialize_error(&error),
            "{\"code\":\"invalid-request\",\"message\":\"invalid request\"}"
        );
    }
}
