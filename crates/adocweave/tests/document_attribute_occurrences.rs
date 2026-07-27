use adocweave::output::formatter::{FormatConfig, NewlineStyle, format_analysis};
use adocweave::semantic::{DocumentAttributeOccurrence, DocumentAttributeOperation};
use adocweave::text::SyntaxIssueClass;
use adocweave::{AnalysisOptions, Engine};

#[test]
fn public_occurrences_preserve_standard_attribute_source_facts() {
    let source = include_str!("../../../fixtures/attributes/public-occurrences.adoc");
    let analysis = Engine::new(AnalysisOptions::default())
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

    let attributes = analysis.attribute_environment().final_values();
    assert_eq!(
        attributes.get("duplicate").map(String::as_str),
        Some("second")
    );
    assert_eq!(attributes.get("empty").map(String::as_str), Some(""));
    assert_eq!(attributes.get("removed"), None);
    assert_eq!(attributes.get("alternate"), None);
}

fn slice(source: &str, range: adocweave::text::TextRange) -> &str {
    &source[range.start().to_usize()..range.end().to_usize()]
}

#[test]
fn root_body_occurrences_cover_set_unset_and_adjacent_blocks() {
    let source = include_str!("../../../fixtures/attributes/body-set-unset.adoc");
    let analysis = analyze(source);
    let occurrences = analysis.document_attribute_occurrences();

    assert_eq!(occurrences.len(), 2);
    assert_eq!(occurrences[0].name, "theme");
    assert_eq!(occurrences[0].raw_value, "dark");
    assert_eq!(occurrences[0].operation, DocumentAttributeOperation::Set);
    assert_eq!(occurrences[1].name, "feature");
    assert_eq!(occurrences[1].operation, DocumentAttributeOperation::Unset);
    assert!(analysis.header_attribute_occurrences().is_empty());
    assert_eq!(
        analysis
            .attribute_environment()
            .final_values()
            .get("theme")
            .map(String::as_str),
        Some("dark")
    );
    assert!(
        analysis
            .document()
            .header()
            .range
            .is_some_and(|range| range.end() <= occurrences[0].range.start())
    );
    assert_eq!(analysis.document().blocks().len(), 3);
}

#[test]
fn invalid_body_occurrences_remain_lossless_and_report_problems() {
    let source = include_str!("../../../fixtures/attributes/body-invalid.adoc");
    let analysis = analyze(source);
    let occurrences = analysis.document_attribute_occurrences();

    assert_eq!(occurrences.len(), 7);
    assert_eq!(occurrences[0].name, "bad name");
    assert_eq!(occurrences[1].name, "bad.name");
    assert_eq!(occurrences[2].name, "-bad");
    assert_eq!(occurrences[3].name, "--");
    assert_eq!(occurrences[4].name, "foo");
    assert_eq!(occurrences[4].raw_value, "value");
    assert!(!occurrences[4].valid);
    assert_eq!(occurrences[5].name, "CamelCase");
    assert_eq!(occurrences[5].raw_value, "ok");
    assert!(occurrences[5].valid);
    assert_eq!(occurrences[6].operation, DocumentAttributeOperation::Unset);
    assert_eq!(occurrences[6].raw_value, "unexpected");
    assert_eq!(
        analysis
            .syntax()
            .issues()
            .iter()
            .filter(|issue| issue.class == SyntaxIssueClass::InvalidAttribute)
            .count(),
        6
    );
    assert_eq!(analysis.syntax().reconstruct(), source);
}

#[test]
fn unicode_and_crlf_body_occurrences_have_byte_accurate_ranges() {
    for source in [
        include_str!("../../../fixtures/attributes/body-unicode.adoc"),
        include_str!("../../../fixtures/attributes/body-crlf.adoc"),
    ] {
        let analysis = analyze(source);
        let occurrence = &analysis.document_attribute_occurrences()[0];
        assert_eq!(slice(source, occurrence.name_range), occurrence.name);
        assert_eq!(slice(source, occurrence.value_range), occurrence.raw_value);
        assert_eq!(
            slice(source, occurrence.range).trim_end(),
            format!(":{}: {}", occurrence.name, occurrence.raw_value)
        );
        assert_eq!(analysis.syntax().reconstruct(), source);
    }
}

#[test]
fn delimited_block_attributes_are_not_promoted_to_document_occurrences() {
    for source in [
        "----\n:inside: value\n----\n\n:outside: value\n",
        "--\n:inside: value\n--\n\n:outside: value\n",
        "****\n:inside: value\n****\n\n:outside: value\n",
        "====\n:inside: value\n====\n\n:outside: value\n",
        "////\n:inside: value\n////\n\n:outside: value\n",
        "....\n:inside: value\n....\n\n:outside: value\n",
        "++++\n:inside: value\n++++\n\n:outside: value\n",
        "____\n:inside: value\n____\n\n:outside: value\n",
        "|===\na|\n:inside: value\n|===\n\n:outside: value\n",
        "* item\n+\n--\n:inside: value\n--\n\n:outside: value\n",
    ] {
        let analysis = analyze(source);
        assert_eq!(analysis.document_attribute_occurrences().len(), 1);
        assert_eq!(analysis.document_attribute_occurrences()[0].name, "outside");
    }
}

#[test]
fn body_attribute_without_blank_offset_remains_paragraph_text() {
    let source = include_str!("../../../fixtures/attributes/body-adjacent-blocks.adoc");
    let analysis = analyze(source);

    assert!(analysis.document_attribute_occurrences().is_empty());
    assert_eq!(analysis.syntax().reconstruct(), source);
}

#[test]
fn leading_attributes_form_the_header_with_or_without_a_title() {
    for source in [
        include_str!("../../../fixtures/attributes/header-without-title.adoc"),
        include_str!("../../../fixtures/attributes/header-before-title.adoc"),
    ] {
        let analysis = analyze(source);
        assert_eq!(analysis.document_attribute_occurrences().len(), 1);
        assert_eq!(analysis.header_attribute_occurrences().len(), 1);
        assert_eq!(
            analysis
                .attribute_environment()
                .values_at(analysis.document().header().end)
                .get("foo")
                .map(String::as_str),
            Some("bar")
        );
        let header_range = analysis.document().header().range.expect("header range");
        assert_eq!(header_range.start().to_usize(), 0);
        assert_eq!(
            slice(source, header_range),
            source
                .split_once("\n\n")
                .expect("header boundary")
                .0
                .to_owned()
                + "\n"
        );
    }
}

#[test]
fn leading_blank_lines_and_comments_do_not_close_an_empty_header() {
    for source in [
        include_str!("../../../fixtures/attributes/header-after-leading-blank.adoc"),
        include_str!("../../../fixtures/attributes/header-after-leading-comments.adoc"),
    ] {
        let analysis = analyze(source);
        let occurrence = &analysis.header_attribute_occurrences()[0];
        assert_eq!(occurrence.name, "foo");
        assert_eq!(
            analysis.document_attribute_occurrences(),
            std::slice::from_ref(occurrence)
        );
        let header_range = analysis.document().header().range.expect("header range");
        assert_eq!(header_range, occurrence.range);
        assert_eq!(slice(source, header_range), ":foo: bar\n");
        assert_eq!(analysis.syntax().reconstruct(), source);
    }
}

#[test]
fn body_attribute_offset_may_contain_line_comments() {
    let source = include_str!("../../../fixtures/attributes/body-after-comment.adoc");
    let analysis = analyze(source);

    assert_eq!(analysis.document_attribute_occurrences().len(), 2);
    assert!(analysis.header_attribute_occurrences().is_empty());
    assert_eq!(
        analysis
            .document_attribute_occurrences()
            .iter()
            .map(|attribute| attribute.name.as_str())
            .collect::<Vec<_>>(),
        ["foo", "bar"]
    );
    assert_eq!(
        analysis
            .syntax()
            .nodes(adocweave::text::SyntaxKind::CommentLine)
            .count(),
        2
    );
    assert_eq!(analysis.document().blocks().len(), 4);
    assert_eq!(analysis.syntax().reconstruct(), source);
}

#[test]
fn body_may_start_with_an_attribute_after_the_header_boundary() {
    let source = include_str!("../../../fixtures/attributes/header-then-body-attribute.adoc");
    let analysis = analyze(source);

    assert_eq!(
        analysis
            .document_attribute_occurrences()
            .iter()
            .map(|attribute| attribute.name.as_str())
            .collect::<Vec<_>>(),
        ["header", "body"]
    );
    assert_eq!(
        analysis
            .header_attribute_occurrences()
            .iter()
            .map(|attribute| attribute.name.as_str())
            .collect::<Vec<_>>(),
        ["header"]
    );
    assert_eq!(analysis.document().blocks().len(), 1);
    assert_eq!(
        analysis.document().blocks()[0].range().start().to_usize(),
        source.find("Body").unwrap()
    );
}

#[test]
fn formatter_preserves_attribute_bytes_and_is_idempotent() {
    let source = include_str!("../../../fixtures/attributes/body-crlf.adoc");
    let analysis = analyze(source);
    let config = FormatConfig {
        newline: NewlineStyle::CrLf,
        final_newline: true,
        ..FormatConfig::default()
    };
    let first = format_analysis(&analysis, &config).expect("format");
    let second_analysis = analyze(&first.formatted);
    let second = format_analysis(&second_analysis, &config).expect("format");

    assert_eq!(first.formatted, source);
    assert!(first.edits.is_empty());
    assert_eq!(second.formatted, first.formatted);
    assert!(second.edits.is_empty());
    assert_eq!(
        analysis.document_attribute_occurrences(),
        second_analysis.document_attribute_occurrences()
    );
}

#[test]
fn formatter_preserves_valid_lf_body_attribute_fixture() {
    let source = include_str!("../../../fixtures/attributes/body-set-unset.adoc");
    let first = format_analysis(&analyze(source), &FormatConfig::default()).expect("format");
    let second =
        format_analysis(&analyze(&first.formatted), &FormatConfig::default()).expect("format");

    assert_eq!(first.formatted, source);
    assert_eq!(second.formatted, source);
}

#[test]
fn formatter_keeps_the_required_body_attribute_offset_when_blank_limit_is_zero() {
    let source = include_str!("../../../fixtures/attributes/formatter-body-offset.adoc");
    let before = analyze(source);
    let formatted = format_analysis(
        &before,
        &FormatConfig {
            max_consecutive_blank_lines: 0,
            ..FormatConfig::default()
        },
    )
    .expect("format");
    let after = analyze(&formatted.formatted);

    assert_eq!(formatted.formatted, source);
    assert_eq!(
        before.document_attribute_occurrences(),
        after.document_attribute_occurrences()
    );
    assert_eq!(after.document_attribute_occurrences()[0].name, "foo");
}

fn analyze(source: &str) -> adocweave::Analysis {
    Engine::new(AnalysisOptions::default())
        .analyze(source)
        .expect("fixture analyzes")
}
