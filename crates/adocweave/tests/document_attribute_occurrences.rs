use adocweave::semantic::{DocumentAttributeOccurrence, DocumentAttributeOperation};
use adocweave::{Engine, ParseOptions};

#[test]
fn public_occurrences_preserve_standard_attribute_source_facts() {
    let source = include_str!("../../../fixtures/attributes/public-occurrences.adoc");
    let analysis = Engine::new(ParseOptions::default())
        .analyze(source)
        .expect("analysis");

    let occurrences: &[DocumentAttributeOccurrence] = analysis.document_attribute_occurrences();
    assert_eq!(occurrences.len(), 5);
    assert_eq!(
        occurrences
            .iter()
            .map(|occurrence| occurrence.name.as_str())
            .collect::<Vec<_>>(),
        ["duplicate", "duplicate", "empty", "removed", "alternate"]
    );
    assert_eq!(
        occurrences
            .iter()
            .map(|occurrence| occurrence.operation)
            .collect::<Vec<_>>(),
        [
            DocumentAttributeOperation::Set,
            DocumentAttributeOperation::Set,
            DocumentAttributeOperation::Set,
            DocumentAttributeOperation::Unset,
            DocumentAttributeOperation::Unset,
        ]
    );
    assert_eq!(occurrences[0].raw_value, "first");
    assert_eq!(occurrences[1].raw_value, "second");
    assert!(occurrences[2].value_range.is_empty());
    assert!(occurrences[3].value_range.is_empty());
    assert!(occurrences[4].value_range.is_empty());

    for occurrence in occurrences {
        assert_eq!(
            slice(source, occurrence.range),
            match occurrence.name.as_str() {
                "duplicate" if occurrence.raw_value == "first" => ":duplicate: first\n",
                "duplicate" => ":duplicate: second\n",
                "empty" => ":empty:\n",
                "removed" => ":removed!:\n",
                "alternate" => ":!alternate:\n",
                unexpected => panic!("unexpected attribute {unexpected}"),
            }
        );
        assert_eq!(slice(source, occurrence.name_range), occurrence.name);
        assert_eq!(slice(source, occurrence.value_range), occurrence.raw_value);
    }

    let attributes = analysis.presentation().attributes();
    assert_eq!(attributes.get("duplicate"), Some("second"));
    assert_eq!(attributes.get("empty"), Some(""));
    assert_eq!(attributes.get("removed"), None);
    assert_eq!(attributes.get("alternate"), None);
}

fn slice(source: &str, range: adocweave::text::TextRange) -> &str {
    &source[range.start().to_usize()..range.end().to_usize()]
}
