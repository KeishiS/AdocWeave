use adocweave::output::formatter::{FormatConfig, format_analysis};
use adocweave::output::html::{RenderPolicy, render};
use adocweave::output::projection::{project, searchable_text};
use adocweave::resolution::ReferenceKey;
use adocweave::resolution::{AuthoredUrlPolicy, UrlDecision};
use adocweave::semantic::{
    Block, DelimitedContent, TableCellContent, TableCellStyle, TableFormat, TableSection,
    SemanticNode, generate_heading_ids, reference_targets, walk as walk_semantic,
};
use adocweave::text::{PositionEncoding, SourceDocument, SyntaxKind, TextSize};
use adocweave::{AnalysisOptions, Engine};

fn corpus() -> Vec<String> {
    let alphabet = [
        "",
        "a",
        " ",
        "\n",
        "\r\n",
        "日本語",
        "🙂",
        "\0",
        "*",
        "_",
        "`",
        "[",
        "]",
        "{",
        "}",
        "xref:",
        "stem:",
        "++++",
    ];
    let mut values = alphabet.iter().map(ToString::to_string).collect::<Vec<_>>();
    let mut state = 0x6d5a_56da_u32;
    for length in 0..128 {
        let mut value = String::new();
        for _ in 0..length {
            state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            value.push_str(alphabet[state as usize % alphabet.len()]);
        }
        values.push(value);
    }
    values
}

#[test]
fn arbitrary_utf8_like_corpus_is_lossless_and_has_valid_ranges() {
    let engine = Engine::new(AnalysisOptions::default());
    for source in corpus() {
        let analysis = engine
            .analyze(&source)
            .expect("bounded UTF-8 input analyzes");
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
            let start = token.range.start().to_usize();
            let end = token.range.end().to_usize();
            assert!(start <= end && end <= source.len());
            assert!(source.is_char_boundary(start));
            assert!(source.is_char_boundary(end));
        }
        for block in analysis.document().blocks() {
            let range = block.range();
            assert!(range.start() <= range.end());
            assert!(range.end().to_usize() <= source.len());
        }
    }
}

#[test]
fn formatter_is_idempotent_over_generated_corpus() {
    let engine = Engine::new(AnalysisOptions::default());
    for source in corpus() {
        let first_analysis = engine.analyze(&source).expect("first analysis");
        let first = format_analysis(&first_analysis, &FormatConfig::default()).expect("format");
        let second_analysis = engine
            .analyze(&first.formatted)
            .expect("formatted analysis");
        let second = format_analysis(&second_analysis, &FormatConfig::default()).expect("format");
        assert_eq!(first.formatted, second.formatted);
    }
}

#[test]
fn formatter_preserves_semantics_and_protected_source_regions() {
    let engine = Engine::new(AnalysisOptions::default());
    for source in corpus() {
        let before = engine.analyze(&source).expect("analysis before format");
        let formatted =
            format_analysis(&before, &FormatConfig::default()).expect("format generated input");

        for range in before.syntax().formatting_protected_ranges() {
            assert!(formatted.edits.iter().all(|edit| {
                edit.range.end() <= range.start() || range.end() <= edit.range.start()
            }));
        }

        let after = engine
            .analyze(&formatted.formatted)
            .expect("analysis after format");
        assert_eq!(semantic_signature(&before), semantic_signature(&after));
    }
}

#[test]
fn positions_round_trip_at_every_character_boundary() {
    for source in corpus() {
        let index = SourceDocument::new(&source).expect("bounded generated source");
        for offset in (0..=source.len()).filter(|offset| source.is_char_boundary(*offset)) {
            let offset = TextSize::new(offset).expect("small corpus offset");
            for encoding in [PositionEncoding::Utf8, PositionEncoding::Utf16] {
                if let Ok(position) = index.offset_to_position(offset, encoding) {
                    assert_eq!(index.position_to_offset(position, encoding), Ok(offset));
                }
            }
        }
    }
}

#[test]
fn renderer_and_projections_are_deterministic_for_generated_input() {
    let engine = Engine::new(AnalysisOptions::default());
    for source in corpus() {
        let analysis = engine.analyze(&source).expect("analysis");
        let first_html = render(analysis.document(), &RenderPolicy::default());
        let second_html = render(analysis.document(), &RenderPolicy::default());
        assert_eq!(first_html, second_html);
        assert_eq!(
            project(&analysis, &adocweave::resolution::RenderInputs::default()),
            project(&analysis, &adocweave::resolution::RenderInputs::default())
        );
        assert_eq!(searchable_text(&analysis), searchable_text(&analysis));
        assert!(first_html.html.len() <= source.len().saturating_mul(32).max(64));
    }
}

#[test]
fn generated_reference_keys_and_targets_are_stable_and_bounded() {
    let engine = Engine::new(AnalysisOptions::default());
    for source in corpus() {
        let analysis = engine.analyze(&source).expect("analysis");
        assert_eq!(
            generate_heading_ids(analysis.document()),
            generate_heading_ids(analysis.document())
        );
        assert_eq!(
            reference_targets(analysis.document()),
            analysis.reference_targets()
        );
        for reference in analysis.references() {
            if let Some(key) = reference.target.clone() {
                assert_eq!(Some(key.clone()), reference.target.clone());
                assert!(reference.range.end().to_usize() <= source.len());
            }
        }
    }
}

#[test]
fn generated_semantic_topology_is_deterministic_and_visits_each_node_once() {
    fn identity(node: SemanticNode<'_>) -> (&'static str, usize) {
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

    let engine = Engine::new(AnalysisOptions::default());
    for source in corpus() {
        let analysis = engine.analyze(&source).expect("analysis");
        let mut first = Vec::new();
        walk_semantic(analysis.document(), |node| first.push(identity(node)));
        let mut second = Vec::new();
        walk_semantic(analysis.document(), |node| second.push(identity(node)));

        assert_eq!(first, second, "{source:?}");
        assert_eq!(
            first
                .iter()
                .copied()
                .collect::<std::collections::HashSet<_>>()
                .len(),
            first.len(),
            "{source:?}"
        );
    }
}

#[test]
fn url_classification_is_case_stable_and_rejects_obfuscated_controls() {
    let policy = AuthoredUrlPolicy::default();
    let safe = [
        "https://example.com",
        "HTTP://example.com",
        "https://例.example/道",
        "../outside.adoc",
    ];
    for value in safe {
        assert_eq!(policy.classify(value), UrlDecision::Allowed);
        assert_eq!(policy.classify(value), policy.classify(value));
    }

    let unsafe_values = [
        "javascript:alert(1)",
        "JaVaScRiPt:alert(1)",
        "javascript%0a:alert(1)",
        "https://example.com/%00x",
        "https://example.com/ x",
        "data:text/html,<script>alert(1)</script>",
        "/absolute/path",
        "\\\\server\\share",
    ];
    for value in unsafe_values {
        assert_eq!(policy.classify(value), UrlDecision::Rejected, "{value}");
    }
}

#[test]
fn table_phase_fixture_is_lossless_deterministic_and_laid_out_once() {
    let source = include_str!("../../../fixtures/tables/phase-pipeline.adoc");
    let engine = Engine::new(AnalysisOptions::default());
    let first = engine.analyze(source).expect("table phase fixture");
    let second = engine.analyze(source).expect("repeat table phase fixture");
    assert_eq!(first.syntax().reconstruct(), source);
    assert_eq!(first.document(), second.document());
    assert_eq!(
        render(first.document(), &RenderPolicy::default()),
        render(second.document(), &RenderPolicy::default())
    );

    let tables = first
        .document()
        .blocks()
        .iter()
        .filter_map(|block| match block {
            Block::Delimited(block) => match &block.content {
                DelimitedContent::Table(table) => Some(table),
                _ => None,
            },
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(tables.len(), 4);
    assert_eq!(
        tables.iter().map(|table| table.format).collect::<Vec<_>>(),
        [
            TableFormat::Psv,
            TableFormat::Csv,
            TableFormat::Dsv,
            TableFormat::Tsv,
        ]
    );

    for table in &tables {
        assert_eq!(table.rows[0].section, TableSection::Header);
        assert_eq!(table.columns[0].style, TableCellStyle::AsciiDoc);
        assert!(matches!(
            table.rows[0].cells[0].content,
            TableCellContent::AsciiDoc(_)
        ));
        let mut previous_end = table.content_range.start().to_usize();
        for row in &table.rows {
            let mut occupied = Vec::new();
            for cell in &row.cells {
                let start = cell.content_range.start().to_usize();
                let end = cell.content_range.end().to_usize();
                assert!(source.is_char_boundary(start));
                assert!(source.is_char_boundary(end));
                assert_eq!(source.get(start..end), Some(cell.raw.as_str()));
                assert!(cell.range.start().to_usize() >= previous_end);
                previous_end = cell.range.end().to_usize();
                for column in cell.column_index..cell.column_index.saturating_add(cell.column_span)
                {
                    assert!(!occupied.contains(&column));
                    occupied.push(column);
                }
            }
        }
    }
    assert_eq!(tables[0].rows[1].cells[0].column_span, 2);
    assert!(matches!(
        tables[0].rows[1].cells[0].content,
        TableCellContent::AsciiDoc(_)
    ));
}

fn semantic_signature(analysis: &adocweave::Analysis) -> (String, Vec<String>, Vec<ReferenceKey>) {
    (
        searchable_text(analysis).text,
        reference_targets(analysis.document())
            .iter()
            .map(|target| format!("{:?}:{}:{}", target.kind, target.id, target.label))
            .collect(),
        analysis
            .references()
            .iter()
            .filter_map(|reference| reference.target.clone())
            .collect(),
    )
}
