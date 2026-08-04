use adocweave::output::text::{TextNode, TextNodeKind, project_text};
use adocweave::{AnalysisInputs, AnalysisOptions, Engine, SourceId, VERSION};
use serde::{Deserialize, Serialize};

use crate::{WasmError, wasm_error};

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WasmTextProjectionRequest {
    pub package_version: String,
    pub source_id: Option<String>,
    pub source: String,
}

#[derive(Clone, Debug, Serialize, Eq, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct WasmTextProjectionResponse {
    pub package_version: String,
    pub source_id: Option<String>,
    pub source_range: [u32; 2],
    pub children: Vec<WasmTextNode>,
}

#[derive(Clone, Debug, Serialize, Eq, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct WasmTextNode {
    pub kind: WasmTextNodeKind,
    pub source_range: [u32; 2],
    pub content_range: Option<[u32; 2]>,
    pub level: Option<u8>,
    pub children: Vec<WasmTextNode>,
}

#[derive(Clone, Copy, Debug, Serialize, Eq, PartialEq)]
#[serde(rename_all = "kebab-case")]
pub enum WasmTextNodeKind {
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

pub fn project_text_request(
    request: WasmTextProjectionRequest,
) -> Result<WasmTextProjectionResponse, WasmError> {
    if request.package_version != VERSION {
        return Err(WasmError {
            code: "unsupported-api-version".to_owned(),
            message: format!(
                "unsupported package version {} (expected {VERSION})",
                request.package_version
            ),
        });
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
        .map_err(wasm_error)?;
    let projection = project_text(&analysis);
    Ok(WasmTextProjectionResponse {
        package_version: projection.package_version.to_owned(),
        source_id: projection
            .source_id
            .map(|source_id| source_id.as_str().to_owned()),
        source_range: range(projection.source_range),
        children: projection.children.into_iter().map(project_node).collect(),
    })
}

fn project_node(node: TextNode) -> WasmTextNode {
    WasmTextNode {
        kind: node.kind.into(),
        source_range: range(node.source_range),
        content_range: node.content_range.map(range),
        level: node.level,
        children: node.children.into_iter().map(project_node).collect(),
    }
}

fn range(range: adocweave::text::TextRange) -> [u32; 2] {
    [range.start().to_u32(), range.end().to_u32()]
}

impl From<TextNodeKind> for WasmTextNodeKind {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn projects_a_versioned_source_backed_response() {
        let source = "= 文書\n\n本文です。\n";
        let response = project_text_request(WasmTextProjectionRequest {
            package_version: VERSION.to_owned(),
            source_id: Some("docs:test.adoc".to_owned()),
            source: source.to_owned(),
        })
        .expect("projection");
        assert_eq!(response.package_version, VERSION);
        assert_eq!(response.source_id.as_deref(), Some("docs:test.adoc"));
        assert_eq!(response.source_range, [0, source.len() as u32]);
        assert_eq!(response.children[0].kind, WasmTextNodeKind::Heading);
        assert_eq!(response.children[1].kind, WasmTextNodeKind::Paragraph);
    }

    #[test]
    fn rejects_a_different_package_version() {
        let error = project_text_request(WasmTextProjectionRequest {
            package_version: "0.0.0".to_owned(),
            source_id: None,
            source: String::new(),
        })
        .expect_err("version mismatch");
        assert_eq!(error.code, "unsupported-api-version");
    }
}
