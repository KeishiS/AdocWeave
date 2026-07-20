use adocweave::document::{generate_heading_ids, reference_targets};
use adocweave::formatter::{FormatConfig, format_analysis};
use adocweave::html::{RenderPolicy, render};
use adocweave::parser::FormattingPolicy;
use adocweave::projection::{project, searchable_text};
use adocweave::reference::ReferenceKey;
use adocweave::source::{PositionEncoding, SourceDocument, TextSize};
use adocweave::url::{UrlDecision, UrlPolicy};
use adocweave::{Engine, ParseOptions};

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
    let engine = Engine::new(ParseOptions::default());
    for source in corpus() {
        let analysis = engine
            .analyze(&source)
            .expect("bounded UTF-8 input analyzes");
        assert_eq!(analysis.cst.reconstruct(), source);
        for token in analysis.cst.tokens() {
            let start = token.range.start().to_usize();
            let end = token.range.end().to_usize();
            assert!(start <= end && end <= source.len());
            assert!(source.is_char_boundary(start));
            assert!(source.is_char_boundary(end));
        }
        for block in &analysis.ast.blocks {
            let range = block.range();
            assert!(range.start() <= range.end());
            assert!(range.end().to_usize() <= source.len());
        }
    }
}

#[test]
fn formatter_is_idempotent_over_generated_corpus() {
    let engine = Engine::new(ParseOptions::default());
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
    let engine = Engine::new(ParseOptions::default());
    for source in corpus() {
        let before = engine.analyze(&source).expect("analysis before format");
        let formatted =
            format_analysis(&before, &FormatConfig::default()).expect("format generated input");

        for block in before
            .cst
            .blocks()
            .iter()
            .filter(|block| block.kind.formatting_policy() == FormattingPolicy::PreserveBytes)
        {
            assert!(formatted.edits.iter().all(|edit| {
                edit.range.end() <= block.range.start() || block.range.end() <= edit.range.start()
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
    let engine = Engine::new(ParseOptions::default());
    for source in corpus() {
        let analysis = engine.analyze(&source).expect("analysis");
        let first_html = render(&analysis.ast, &RenderPolicy::default());
        let second_html = render(&analysis.ast, &RenderPolicy::default());
        assert_eq!(first_html, second_html);
        assert_eq!(project(&analysis, &[]), project(&analysis, &[]));
        assert_eq!(searchable_text(&analysis), searchable_text(&analysis));
        assert!(first_html.html.len() <= source.len().saturating_mul(32).max(64));
    }
}

#[test]
fn generated_reference_keys_and_targets_are_stable_and_bounded() {
    let engine = Engine::new(ParseOptions::default());
    for source in corpus() {
        let analysis = engine.analyze(&source).expect("analysis");
        assert_eq!(
            generate_heading_ids(&analysis.ast),
            generate_heading_ids(&analysis.ast)
        );
        assert_eq!(reference_targets(&analysis.ast), analysis.reference_targets);
        for reference in &analysis.references {
            if let Some(key) = ReferenceKey::from_destination(&reference.destination) {
                assert_eq!(
                    Some(key.clone()),
                    ReferenceKey::from_destination(&reference.destination)
                );
                assert!(reference.range.end().to_usize() <= source.len());
            }
        }
    }
}

#[test]
fn url_classification_is_case_stable_and_rejects_obfuscated_controls() {
    let policy = UrlPolicy::default();
    let safe = [
        "https://example.com",
        "HTTP://example.com",
        "https://例.example/道",
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
        "../outside.adoc",
        "/absolute/path",
        "\\\\server\\share",
    ];
    for value in unsafe_values {
        assert_eq!(policy.classify(value), UrlDecision::Rejected, "{value}");
    }
}

fn semantic_signature(analysis: &adocweave::Analysis) -> (String, Vec<String>, Vec<ReferenceKey>) {
    (
        searchable_text(analysis).text,
        reference_targets(&analysis.ast)
            .into_iter()
            .map(|target| format!("{:?}:{}:{}", target.kind, target.id, target.label))
            .collect(),
        analysis
            .references
            .iter()
            .filter_map(|reference| ReferenceKey::from_destination(&reference.destination))
            .collect(),
    )
}
