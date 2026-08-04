//! Node.js-only WebAssembly boundary for source-backed text projections.

use adocweave::output::text::{TextNode, TextNodeKind, project_text};
use adocweave::{AnalysisInputs, AnalysisOptions, Engine, SourceId, VERSION};
use serde::{Deserialize, Serialize};

/// Maximum accepted AsciiDoc input size.
pub const MAX_INPUT_BYTES: usize = 10 * 1024 * 1024;
/// Maximum serialized projection size.
pub const MAX_OUTPUT_BYTES: usize = 50 * 1024 * 1024;
/// Maximum number of nodes in the text projection response.
pub const MAX_PROJECTION_NODES: usize = 1_000_000;
/// Maximum accepted logical source identifier size.
pub const MAX_SOURCE_ID_BYTES: usize = 4 * 1024;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TextProjectionRequest {
    pub package_version: String,
    pub source_id: Option<String>,
    pub source: String,
}

#[derive(Clone, Debug, Serialize, Eq, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct TextProjectionResponse {
    pub package_version: String,
    pub source_id: Option<String>,
    pub source_range: [u32; 2],
    pub children: Vec<TextProjectionNode>,
}

#[derive(Clone, Debug, Serialize, Eq, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct TextProjectionNode {
    pub kind: TextProjectionNodeKind,
    pub source_range: [u32; 2],
    pub content_range: Option<[u32; 2]>,
    pub level: Option<u8>,
    pub url: Option<String>,
    pub ordered: Option<bool>,
    pub language: Option<String>,
    pub children: Vec<TextProjectionNode>,
}

#[derive(Clone, Copy, Debug, Serialize, Eq, PartialEq)]
#[serde(rename_all = "kebab-case")]
pub enum TextProjectionNodeKind {
    BlockTitle,
    Heading,
    Paragraph,
    List,
    ListItem,
    DescriptionTerm,
    BlockQuote,
    Table,
    TableRow,
    TableCell,
    CodeBlock,
    Comment,
    Text,
    Code,
    Strong,
    Emphasis,
    Link,
    Reference,
    HardBreak,
    Container,
    Excluded,
}

#[derive(Clone, Debug, Serialize, Eq, PartialEq)]
pub struct TextProjectionError {
    pub code: String,
    pub message: String,
}

impl TextProjectionError {
    fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
        }
    }
}

pub fn project_text_request(
    request: TextProjectionRequest,
) -> Result<TextProjectionResponse, TextProjectionError> {
    project_text_request_with_limits(
        request,
        MAX_INPUT_BYTES,
        MAX_OUTPUT_BYTES,
        MAX_PROJECTION_NODES,
    )
}

fn project_text_request_with_limits(
    request: TextProjectionRequest,
    max_input_bytes: usize,
    max_output_bytes: usize,
    max_projection_nodes: usize,
) -> Result<TextProjectionResponse, TextProjectionError> {
    if request.package_version != VERSION {
        return Err(TextProjectionError::new(
            "unsupported-api-version",
            format!(
                "unsupported package version {} (expected {VERSION})",
                request.package_version
            ),
        ));
    }
    if request.source.len() > max_input_bytes {
        return Err(TextProjectionError::new(
            "input-too-large",
            format!("input exceeds the {max_input_bytes} byte limit"),
        ));
    }
    if request
        .source_id
        .as_ref()
        .is_some_and(|source_id| source_id.len() > MAX_SOURCE_ID_BYTES)
    {
        return Err(TextProjectionError::new(
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
        .map_err(|error| TextProjectionError::new(error.code().as_str(), error.to_string()))?;
    let projection = project_text(&analysis);
    let projection_nodes = projection.children.iter().map(count_nodes).sum::<usize>();
    if projection_nodes > max_projection_nodes {
        return Err(TextProjectionError::new(
            "node-limit",
            format!("projection exceeds the {max_projection_nodes} node limit"),
        ));
    }
    let response = TextProjectionResponse {
        package_version: projection.package_version.to_owned(),
        source_id: projection
            .source_id
            .map(|source_id| source_id.as_str().to_owned()),
        source_range: range(projection.source_range),
        children: projection.children.into_iter().map(project_node).collect(),
    };
    let output_bytes = serde_json::to_vec(&response)
        .map_err(|error| TextProjectionError::new("serialization-failed", error.to_string()))?
        .len();
    if output_bytes > max_output_bytes {
        return Err(TextProjectionError::new(
            "output-too-large",
            format!("output exceeds the {max_output_bytes} byte limit"),
        ));
    }
    Ok(response)
}

fn count_nodes(node: &TextNode) -> usize {
    node.children.iter().fold(1usize, |count, child| {
        count.saturating_add(count_nodes(child))
    })
}

fn project_node(node: TextNode) -> TextProjectionNode {
    TextProjectionNode {
        kind: node.kind.into(),
        source_range: range(node.source_range),
        content_range: node.content_range.map(range),
        level: node.level,
        url: node.url,
        ordered: node.ordered,
        language: node.language,
        children: node.children.into_iter().map(project_node).collect(),
    }
}

fn range(range: adocweave::text::TextRange) -> [u32; 2] {
    [range.start().to_u32(), range.end().to_u32()]
}

impl From<TextNodeKind> for TextProjectionNodeKind {
    fn from(value: TextNodeKind) -> Self {
        match value {
            TextNodeKind::BlockTitle => Self::BlockTitle,
            TextNodeKind::Heading => Self::Heading,
            TextNodeKind::Paragraph => Self::Paragraph,
            TextNodeKind::List => Self::List,
            TextNodeKind::ListItem => Self::ListItem,
            TextNodeKind::DescriptionTerm => Self::DescriptionTerm,
            TextNodeKind::BlockQuote => Self::BlockQuote,
            TextNodeKind::Table => Self::Table,
            TextNodeKind::TableRow => Self::TableRow,
            TextNodeKind::TableCell => Self::TableCell,
            TextNodeKind::CodeBlock => Self::CodeBlock,
            TextNodeKind::Comment => Self::Comment,
            TextNodeKind::Text => Self::Text,
            TextNodeKind::Code => Self::Code,
            TextNodeKind::Strong => Self::Strong,
            TextNodeKind::Emphasis => Self::Emphasis,
            TextNodeKind::Link => Self::Link,
            TextNodeKind::Reference => Self::Reference,
            TextNodeKind::HardBreak => Self::HardBreak,
            TextNodeKind::Container => Self::Container,
            TextNodeKind::Excluded => Self::Excluded,
        }
    }
}

#[cfg(any(test, target_arch = "wasm32"))]
fn serialize_error(error: &TextProjectionError) -> String {
    serde_json::to_string(error).unwrap_or_else(|_| {
        "{\"code\":\"serialization-failed\",\"message\":\"failed to serialize error\"}".to_owned()
    })
}

#[cfg(target_arch = "wasm32")]
mod bindings {
    use serde::de::DeserializeOwned;
    use wasm_bindgen::prelude::*;

    use super::*;

    #[wasm_bindgen(js_name = projectText)]
    pub fn project_text_js(request: JsValue) -> Result<JsValue, JsValue> {
        let request = deserialize_request(request)?;
        let response = project_text_request(request)
            .map_err(|error| JsValue::from_str(&serialize_error(&error)))?;
        response
            .serialize(&serde_wasm_bindgen::Serializer::json_compatible())
            .map_err(|error| {
                let error = TextProjectionError::new("serialization-failed", error.to_string());
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
        JsValue::from_str(&serialize_error(&TextProjectionError::new(
            "invalid-request",
            message,
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(source: &str) -> TextProjectionRequest {
        TextProjectionRequest {
            package_version: VERSION.to_owned(),
            source_id: Some("docs:test.adoc".to_owned()),
            source: source.to_owned(),
        }
    }

    #[test]
    fn projects_a_versioned_source_backed_response() {
        let source = "= 文書\n\n本文です。\n";
        let response = project_text_request(request(source)).expect("projection");
        assert_eq!(response.package_version, VERSION);
        assert_eq!(response.source_id.as_deref(), Some("docs:test.adoc"));
        assert_eq!(response.source_range, [0, source.len() as u32]);
        assert_eq!(response.children[0].kind, TextProjectionNodeKind::Heading);
        assert_eq!(response.children[1].kind, TextProjectionNodeKind::Paragraph);
    }

    #[test]
    fn rejects_a_different_package_version() {
        let mut request = request("");
        request.package_version = "0.0.0".to_owned();
        let error = project_text_request(request).expect_err("version mismatch");
        assert_eq!(error.code, "unsupported-api-version");
    }

    #[test]
    fn rejects_input_beyond_the_limit() {
        let error = project_text_request_with_limits(
            request("x"),
            0,
            MAX_OUTPUT_BYTES,
            MAX_PROJECTION_NODES,
        )
        .expect_err("input limit");
        assert_eq!(error.code, "input-too-large");
    }

    #[test]
    fn rejects_output_beyond_the_limit() {
        let error = project_text_request_with_limits(
            request("x"),
            MAX_INPUT_BYTES,
            0,
            MAX_PROJECTION_NODES,
        )
        .expect_err("output limit");
        assert_eq!(error.code, "output-too-large");
    }

    #[test]
    fn rejects_a_projection_beyond_the_node_limit() {
        let error = project_text_request_with_limits(
            request("本文です。"),
            MAX_INPUT_BYTES,
            MAX_OUTPUT_BYTES,
            0,
        )
        .expect_err("node limit");
        assert_eq!(error.code, "node-limit");
    }

    #[test]
    fn rejects_an_oversized_source_identifier() {
        let mut request = request("");
        request.source_id = Some("x".repeat(MAX_SOURCE_ID_BYTES + 1));
        let error = project_text_request(request).expect_err("source identifier limit");
        assert_eq!(error.code, "invalid-request");
    }

    #[test]
    fn request_rejects_unknown_fields() {
        let error = serde_json::from_value::<TextProjectionRequest>(serde_json::json!({
            "packageVersion": VERSION,
            "sourceId": null,
            "source": "",
            "unknown": true
        }))
        .expect_err("unknown field");
        assert!(error.to_string().contains("unknown field"));
    }

    #[test]
    fn errors_have_a_stable_serializable_shape() {
        let error = TextProjectionError::new("invalid-request", "invalid request");
        assert_eq!(
            serialize_error(&error),
            "{\"code\":\"invalid-request\",\"message\":\"invalid request\"}"
        );
    }

    #[test]
    fn preserves_node_specific_textlint_properties() {
        fn find(
            nodes: &[TextProjectionNode],
            kind: TextProjectionNodeKind,
        ) -> Option<&TextProjectionNode> {
            nodes.iter().find_map(|node| {
                (node.kind == kind)
                    .then_some(node)
                    .or_else(|| find(&node.children, kind))
            })
        }

        let source = ":site: https://example.com\n\nlink:{site}[表示]\n\n. 項目\n\n[source,rust]\n----\nfn main() {}\n----\n";
        let response = project_text_request(request(source)).expect("projection");
        assert_eq!(
            find(&response.children, TextProjectionNodeKind::Link)
                .and_then(|node| node.url.as_deref()),
            Some("https://example.com")
        );
        assert_eq!(
            find(&response.children, TextProjectionNodeKind::List).and_then(|node| node.ordered),
            Some(true)
        );
        assert_eq!(
            find(&response.children, TextProjectionNodeKind::CodeBlock)
                .and_then(|node| node.language.as_deref()),
            Some("rust")
        );
    }
}
