//! Stable, backend-neutral products used by cross-runtime conformance tests.

use std::fmt::Write as _;

use crate::Analysis;
use crate::diagnostic::render_json as render_diagnostics_json;
use crate::document::{document_symbols, render_symbols_json};
use crate::html::{RenderPolicy, render_with_inputs};
use crate::inline::{Inline, ReferenceDestination};
use crate::parser::{AstBlock, AstDocument, BlockMetadata, ListBlock, ListItem};
use crate::projection::project;
use crate::render::RenderInputs;
use crate::source::TextRange;

pub const CONFORMANCE_CONTRACT_VERSION: u16 = 10;

/// Canonical products derived from exactly one owned analysis snapshot.
///
/// Strings are used at this boundary so native, WASM, and non-Rust hosts compare
/// the same bytes without depending on host object-key ordering.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ConformanceSnapshot {
    pub contract_version: u16,
    pub syntax: String,
    pub ast: String,
    pub diagnostics_json: String,
    pub render_diagnostics_json: String,
    pub symbols_json: String,
    pub projection_json: String,
    pub html: String,
}

pub fn snapshot(
    analysis: &Analysis,
    policy: &RenderPolicy,
    inputs: &RenderInputs,
) -> ConformanceSnapshot {
    let html = render_with_inputs(analysis.ast(), policy, inputs);
    ConformanceSnapshot {
        contract_version: CONFORMANCE_CONTRACT_VERSION,
        syntax: canonical_syntax(analysis),
        ast: canonical_ast(analysis.ast()),
        diagnostics_json: render_diagnostics_json(analysis.diagnostics()),
        render_diagnostics_json: render_diagnostics_json(&html.diagnostics),
        symbols_json: render_symbols_json(&document_symbols(analysis.ast())),
        projection_json: project(analysis, inputs).render_json(),
        html: html.html,
    }
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct CanonicalAst {
    schema_version: u16,
    blocks: Vec<CanonicalNode>,
    attributes: Vec<CanonicalNode>,
    anchors: Vec<CanonicalNode>,
}

#[derive(serde::Serialize)]
struct CanonicalNode {
    kind: &'static str,
    range: [u32; 2],
    #[serde(skip_serializing_if = "Option::is_none")]
    value: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    children: Vec<CanonicalNode>,
}

fn canonical_ast(document: &AstDocument) -> String {
    let dto = CanonicalAst {
        schema_version: 2,
        blocks: document.blocks().iter().map(block_node).collect(),
        attributes: document
            .attributes()
            .iter()
            .map(|attribute| CanonicalNode {
                kind: "attribute",
                range: range(attribute.range),
                value: Some(format!("{}={}", attribute.name, attribute.raw_value)),
                children: Vec::new(),
            })
            .collect(),
        anchors: document
            .anchors()
            .iter()
            .map(|anchor| CanonicalNode {
                kind: "anchor",
                range: range(anchor.range),
                value: Some(anchor.id.clone()),
                children: Vec::new(),
            })
            .collect(),
    };
    serde_json::to_string(&dto).expect("canonical DTO contains owned serializable values")
}

fn block_node(block: &AstBlock) -> CanonicalNode {
    let mut node = match block {
        AstBlock::Heading(node) => CanonicalNode {
            kind: match node.kind {
                crate::parser::HeadingKind::DocumentTitle => "document-title",
                crate::parser::HeadingKind::Part => "part",
                crate::parser::HeadingKind::Section { .. } => "section",
                crate::parser::HeadingKind::Discrete { .. } => "discrete-heading",
            },
            range: range(node.range),
            value: Some(node.text.clone()),
            children: inline_nodes(&node.inlines),
        },
        AstBlock::Paragraph(node) => CanonicalNode {
            kind: "paragraph",
            range: range(node.range),
            value: Some(node.value.clone()),
            children: inline_nodes(&node.inlines),
        },
        AstBlock::LiteralParagraph(node) => leaf("literal-paragraph", node.range, &node.value),
        AstBlock::Break(node) => CanonicalNode {
            kind: match node.kind {
                crate::parser::BreakKind::Thematic => "thematic-break",
                crate::parser::BreakKind::Page => "page-break",
            },
            range: range(node.range),
            value: None,
            children: Vec::new(),
        },
        AstBlock::Source(node) => CanonicalNode {
            kind: "source-block",
            range: range(node.range),
            value: Some(format!(
                "{}:{}",
                node.language.as_deref().unwrap_or(""),
                node.value
            )),
            children: Vec::new(),
        },
        AstBlock::Verbatim(node) => CanonicalNode {
            kind: match node.kind {
                crate::parser::VerbatimKind::Listing => "listing-block",
                crate::parser::VerbatimKind::Literal => "literal-block",
                crate::parser::VerbatimKind::Source(_) => "source-block",
            },
            range: range(node.range),
            value: Some(match &node.kind {
                crate::parser::VerbatimKind::Source(source) => format!(
                    "{}:{}",
                    source.language.as_deref().unwrap_or(""),
                    node.value
                ),
                crate::parser::VerbatimKind::Listing | crate::parser::VerbatimKind::Literal => {
                    node.value.clone()
                }
            }),
            children: Vec::new(),
        },
        AstBlock::List(node) => list_node(node),
        AstBlock::Math(node) => leaf("math-block", node.range, &node.value),
        AstBlock::Delimited(node) => {
            let (value, children) = match &node.content {
                crate::parser::DelimitedContent::Compound(children) => (
                    Some(node.delimiter.clone()),
                    children.iter().map(block_node).collect(),
                ),
                crate::parser::DelimitedContent::Verbatim(value)
                | crate::parser::DelimitedContent::Passthrough(value) => {
                    (Some(value.clone()), Vec::new())
                }
                crate::parser::DelimitedContent::Table(table) => (
                    Some(format!("{:?}", table.format).to_ascii_lowercase()),
                    table
                        .rows
                        .iter()
                        .map(|row| CanonicalNode {
                            kind: "table-row",
                            range: range(row.range),
                            value: Some(format!("{:?}", row.section).to_ascii_lowercase()),
                            children: row
                                .cells
                                .iter()
                                .map(|cell| {
                                    let children = match &cell.content {
                                        crate::table::TableCellContent::Inlines(inlines) => {
                                            inline_nodes(inlines)
                                        }
                                        crate::table::TableCellContent::AsciiDoc(blocks) => {
                                            blocks.iter().map(block_node).collect()
                                        }
                                        crate::table::TableCellContent::Verbatim(_) => Vec::new(),
                                    };
                                    CanonicalNode {
                                        kind: "table-cell",
                                        range: range(cell.range),
                                        value: Some(cell.raw.clone()),
                                        children,
                                    }
                                })
                                .collect(),
                        })
                        .collect(),
                ),
            };
            CanonicalNode {
                kind: match node.kind {
                    crate::parser::DelimitedBlockKind::Comment => "comment-block",
                    crate::parser::DelimitedBlockKind::Example => "example-block",
                    crate::parser::DelimitedBlockKind::Listing => "listing-block",
                    crate::parser::DelimitedBlockKind::Literal => "literal-block",
                    crate::parser::DelimitedBlockKind::Open => "open-block",
                    crate::parser::DelimitedBlockKind::Sidebar => "sidebar-block",
                    crate::parser::DelimitedBlockKind::Pass => "pass-block",
                    crate::parser::DelimitedBlockKind::Quote => "quote-block",
                    crate::parser::DelimitedBlockKind::Table => "table-block",
                },
                range: range(node.range),
                value,
                children,
            }
        }
        AstBlock::Unsupported(node) => leaf("unsupported", node.range, &node.raw),
    };
    let mut children = metadata_nodes(block.metadata());
    children.append(&mut node.children);
    node.children = children;
    node
}

fn metadata_nodes(metadata: &BlockMetadata) -> Vec<CanonicalNode> {
    let mut nodes = Vec::new();
    if let Some(title) = &metadata.title {
        nodes.push(leaf("block-title", title.range, &title.value));
    }
    if let Some(id) = &metadata.id {
        nodes.push(leaf("block-id", id.range, &id.value));
    }
    nodes.extend(
        metadata
            .roles
            .iter()
            .map(|role| leaf("block-role", role.range, &role.value)),
    );
    nodes.extend(
        metadata
            .options
            .iter()
            .map(|option| leaf("block-option", option.range, &option.value)),
    );
    nodes.extend(metadata.attributes.iter().map(|attribute| CanonicalNode {
        kind: "element-attribute",
        range: range(attribute.range),
        value: Some(attribute.name.as_ref().map_or_else(
            || attribute.value.clone(),
            |name| format!("{name}={}", attribute.value),
        )),
        children: Vec::new(),
    }));
    nodes
}

fn list_node(list: &ListBlock) -> CanonicalNode {
    CanonicalNode {
        kind: match list.kind {
            crate::parser::ListKind::Unordered => "unordered-list",
            crate::parser::ListKind::Ordered => "ordered-list",
            crate::parser::ListKind::Description => "description-list",
            crate::parser::ListKind::Callout => "callout-list",
        },
        range: range(list.range),
        value: None,
        children: list.items.iter().map(list_item_node).collect(),
    }
}

fn list_item_node(item: &ListItem) -> CanonicalNode {
    let mut children = item
        .terms
        .iter()
        .map(|term| CanonicalNode {
            kind: "description-term",
            range: range(term.range),
            value: Some(term.text.clone()),
            children: inline_nodes(&term.inlines),
        })
        .collect::<Vec<_>>();
    children.extend(inline_nodes(&item.inlines));
    children.extend(item.children.iter().map(list_node));
    children.extend(item.continuations.iter().map(block_node));
    CanonicalNode {
        kind: "list-item",
        range: range(item.range),
        value: Some(match (item.checklist, item.callout_id) {
            (Some(crate::parser::ChecklistState::Checked), _) => format!("checked:{}", item.text),
            (Some(crate::parser::ChecklistState::Unchecked), _) => {
                format!("unchecked:{}", item.text)
            }
            (_, Some(id)) => format!("callout-{id}:{}", item.text),
            _ => item.text.clone(),
        }),
        children,
    }
}

fn inline_nodes(inlines: &[Inline]) -> Vec<CanonicalNode> {
    inlines.iter().map(inline_node).collect()
}

fn inline_node(inline: &Inline) -> CanonicalNode {
    match inline {
        Inline::Text(node) => leaf("text", node.range, &node.value),
        Inline::Literal {
            range: node_range,
            value,
            ..
        } => leaf("monospace", *node_range, value),
        Inline::Styled {
            style,
            range: node_range,
            children,
            ..
        } => CanonicalNode {
            kind: match style {
                crate::inline::InlineStyle::Strong => "strong",
                crate::inline::InlineStyle::Emphasis => "emphasis",
                crate::inline::InlineStyle::Highlight => "highlight",
                crate::inline::InlineStyle::Subscript => "subscript",
                crate::inline::InlineStyle::Superscript => "superscript",
                crate::inline::InlineStyle::CurvedDoubleQuote => "curved-double-quote",
                crate::inline::InlineStyle::CurvedSingleQuote => "curved-single-quote",
            },
            range: range(*node_range),
            value: None,
            children: inline_nodes(children),
        },
        Inline::AttributeReference {
            range: node_range,
            name,
            ..
        } => leaf("attribute-reference", *node_range, name),
        Inline::Link(node) => CanonicalNode {
            kind: "link",
            range: range(node.range),
            value: Some(node.target.clone()),
            children: inline_nodes(&node.label),
        },
        Inline::Reference(node) => CanonicalNode {
            kind: match node.destination {
                ReferenceDestination::Local { .. } => "local-reference",
                ReferenceDestination::Document { .. } => "document-reference",
                ReferenceDestination::Scheme { .. } => "scheme-reference",
                ReferenceDestination::Invalid => "invalid-reference",
            },
            range: range(node.range),
            value: Some(node.target_source.clone()),
            children: inline_nodes(&node.label),
        },
        Inline::Formula(node) => leaf("inline-math", node.range, &node.value),
        Inline::Macro(node) => CanonicalNode {
            kind: "standard-macro",
            range: range(node.range),
            value: Some(format!("{:?}:{}", node.kind, node.target)),
            children: Vec::new(),
        },
        Inline::Passthrough { range, value, .. } => leaf("passthrough", *range, value),
        Inline::HardBreak { range: node_range } => CanonicalNode {
            kind: "hard-break",
            range: range(*node_range),
            value: None,
            children: Vec::new(),
        },
    }
}

fn leaf(kind: &'static str, node_range: TextRange, value: &str) -> CanonicalNode {
    CanonicalNode {
        kind,
        range: range(node_range),
        value: Some(value.to_owned()),
        children: Vec::new(),
    }
}

fn range(value: TextRange) -> [u32; 2] {
    [value.start().to_u32(), value.end().to_u32()]
}

fn canonical_syntax(analysis: &Analysis) -> String {
    let mut output = analysis.syntax().snapshot();
    output.push_str("Tokens\n");
    for token in analysis.syntax().tokens() {
        writeln!(
            output,
            "  {:?}@{}..{}",
            token.kind,
            token.range.start().to_u32(),
            token.range.end().to_u32()
        )
        .expect("writing to a String cannot fail");
    }
    output
}

#[cfg(test)]
mod tests {
    use crate::{Engine, ParseOptions};

    use super::*;

    #[test]
    fn snapshot_is_deterministic_and_owns_every_product() {
        let analysis = Engine::new(ParseOptions::default())
            .analyze("= Title\n\n[[target]]\n== Section\n\n<<target,Here>>\n")
            .expect("analysis");
        let first = snapshot(
            &analysis,
            &RenderPolicy::default(),
            &RenderInputs::default(),
        );
        let second = snapshot(
            &analysis,
            &RenderPolicy::default(),
            &RenderInputs::default(),
        );

        assert_eq!(first, second);
        assert_eq!(first.contract_version, CONFORMANCE_CONTRACT_VERSION);
        assert!(first.syntax.contains("Document@"));
        assert!(first.ast.contains("\"schemaVersion\":2"));
        assert!(first.ast.contains("local-reference"));
        assert!(first.projection_json.contains("referenceEdges"));
        assert!(first.html.contains("href=\"#target\""));
    }

    #[test]
    fn canonical_ast_exposes_backend_neutral_block_metadata() {
        let analysis = Engine::new(ParseOptions::default())
            .analyze(".Title\n[#item.role%collapsible,kind=demo]\nText\n")
            .expect("analysis");
        let value: serde_json::Value =
            serde_json::from_str(&canonical_ast(analysis.ast())).expect("canonical JSON");
        let children = value["blocks"][0]["children"].as_array().expect("children");
        assert_eq!(value["schemaVersion"], 2);
        assert_eq!(children[0]["kind"], "block-title");
        assert_eq!(children[1]["kind"], "block-id");
        assert_eq!(children[2]["kind"], "block-role");
        assert_eq!(children[3]["kind"], "block-option");
        assert_eq!(children[4]["value"], "kind=demo");
    }

    #[test]
    fn canonical_ast_distinguishes_delimited_content_models() {
        let analysis = Engine::new(ParseOptions::default())
            .analyze("====\ninside\n====\n\n++++\n<tag>\n++++\n")
            .expect("analysis");
        let value: serde_json::Value =
            serde_json::from_str(&canonical_ast(analysis.ast())).expect("canonical JSON");
        assert_eq!(value["blocks"][0]["kind"], "example-block");
        assert_eq!(value["blocks"][0]["children"][0]["kind"], "paragraph");
        assert_eq!(value["blocks"][1]["kind"], "pass-block");
        assert_eq!(value["blocks"][1]["value"], "<tag>\n");
    }
}
