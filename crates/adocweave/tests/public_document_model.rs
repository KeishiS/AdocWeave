use adocweave::semantic::{Block, DocumentAttributeOperation};
use adocweave::{Engine, ParseOptions};

#[test]
fn public_document_model_exposes_semantic_facts_without_parser_types() {
    let analysis = Engine::new(ParseOptions::default())
        .analyze(
            "\
= Guide
:edition: first
:edition!:

[[intro]]
== Intro

Text.",
        )
        .expect("analysis succeeds");
    let document = analysis.document();

    assert!(matches!(document.blocks()[0], Block::Heading(_)));
    assert_eq!(document.attribute_occurrences().len(), 2);
    assert_eq!(
        document.attribute_occurrences()[1].operation,
        DocumentAttributeOperation::Unset
    );
    assert_eq!(document.anchors()[0].id, "intro");
    assert!(document.header().range.is_some());
    assert_eq!(document.heading_ids()[1].id, "intro");
}
