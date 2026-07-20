use adocweave::formatter::{FormatConfig, format_analysis};
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
