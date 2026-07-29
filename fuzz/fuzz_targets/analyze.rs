#![no_main]

use adocweave::text::SyntaxKind;
use adocweave::{AnalysisOptions, Engine, ParseError, semantic::SemanticNode};
use libfuzzer_sys::fuzz_target;

fn semantic_identity(node: SemanticNode<'_>) -> (&'static str, usize) {
    fn address<T>(value: &T) -> usize {
        value as *const T as usize
    }

    match node {
        SemanticNode::Block(value) => ("block", address(value)),
        SemanticNode::List(value) => ("list", address(value)),
        SemanticNode::ListItem(value) => ("list-item", address(value)),
        SemanticNode::Table(value) => ("table", address(value)),
        SemanticNode::TableRow(value) => ("table-row", address(value)),
        SemanticNode::TableCell(value) => ("table-cell", address(value)),
        SemanticNode::Inline(value) => ("inline", address(value)),
        SemanticNode::Attribute(value) => ("attribute", address(value)),
        SemanticNode::Anchor(value) => ("anchor", address(value)),
        SemanticNode::Metadata(value) => ("metadata", address(value)),
        SemanticNode::MetadataTitle(value) => ("metadata-title", address(value)),
        SemanticNode::MetadataId(value) => ("metadata-id", address(value)),
        SemanticNode::MetadataRole(value) => ("metadata-role", address(value)),
        SemanticNode::MetadataOption(value) => ("metadata-option", address(value)),
        SemanticNode::ElementAttribute(value) => ("element-attribute", address(value)),
    }
}

fuzz_target!(|source: &str| {
    match Engine::new(AnalysisOptions::default()).analyze(source) {
        Ok(analysis) => {
            assert_eq!(analysis.syntax().reconstruct(), source);
            let mut syntax_cursor = 0;
            for node in analysis.syntax().root().descendants() {
                if !matches!(node.kind(), SyntaxKind::Token(_)) {
                    continue;
                }
                let start = node.range().start().to_usize();
                let end = node.range().end().to_usize();
                assert_eq!(start, syntax_cursor);
                assert!(start < end && end <= source.len());
                assert!(source.is_char_boundary(start));
                assert!(source.is_char_boundary(end));
                syntax_cursor = end;
            }
            assert_eq!(syntax_cursor, source.len());
            for token in analysis.syntax().tokens() {
                let range = token.range;
                assert!(range.start() <= range.end());
                assert!(range.end().to_usize() <= source.len());
                assert!(source.is_char_boundary(range.start().to_usize()));
                assert!(source.is_char_boundary(range.end().to_usize()));
            }
            let mut semantic_nodes = std::collections::HashSet::new();
            adocweave::semantic::walk(analysis.document(), |node| {
                assert!(
                    semantic_nodes.insert(semantic_identity(node)),
                    "semantic node visited more than once"
                );
            });
        }
        Err(ParseError::InternalInvariant) => {
            panic!("syntax construction invariant failed");
        }
        Err(_) => {}
    }
});
