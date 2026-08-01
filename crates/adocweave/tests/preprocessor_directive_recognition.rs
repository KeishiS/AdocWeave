//! Preprocessor directives reaching an analysis that did not preprocess.
//!
//! The preprocessor consumes `ifdef`, `ifndef`, `ifeval`, `endif` and `include`
//! before parsing. An analysis that skips preprocessing still receives the
//! lines, and before this recognition existed the block grammar handed them to
//! the inline parser: `ifeval::["a" == "b"]` was read as a macro whose leading
//! `ifeval:` looked like a URL scheme, so the reader was told the URL was
//! rejected. These tests fix what the reader is told instead.

use adocweave::semantic::{Block, UnsupportedKind};
use adocweave::text::SyntaxIssueClass;
use adocweave::{AnalysisOptions, Engine, ParseError, SyntaxMode};

fn analyze(source: &str) -> adocweave::Analysis {
    Engine::new(AnalysisOptions::default())
        .analyze(source)
        .expect("analysis")
}

fn diagnostic_codes(analysis: &adocweave::Analysis) -> Vec<String> {
    analysis
        .diagnostics()
        .iter()
        .map(|diagnostic| diagnostic.code.as_str().to_owned())
        .collect()
}

#[test]
fn conditional_directives_report_their_own_diagnostic() {
    let source = "= Note\n:source-language: rust\n\nifeval::[\"{source-language}\" == \"rust\"]\nBody\nendif::[]\n";
    let analysis = analyze(source);

    let codes = diagnostic_codes(&analysis);
    assert_eq!(
        codes,
        vec!["unprocessed-directive", "unprocessed-directive"],
        "conditional directives must not be reported as rejected URLs"
    );
    assert!(
        !codes.iter().any(|code| code == "invalid-url-scheme"),
        "the reader must not be sent to check a URL that was never written"
    );
}

#[test]
fn include_directives_report_the_same_diagnostic() {
    let analysis = analyze("= Note\n\ninclude::part.adoc[]\n");

    assert_eq!(diagnostic_codes(&analysis), vec!["unprocessed-directive"]);
}

#[test]
fn directives_are_recorded_as_unsupported_syntax_issues() {
    let analysis = analyze("= Note\n\nifdef::web[]\nBody\nendif::[]\n");

    let directives = analysis
        .syntax()
        .issues()
        .iter()
        .filter(|issue| issue.class == SyntaxIssueClass::UnprocessedDirective)
        .count();
    assert_eq!(directives, 2);
}

#[test]
fn an_escaped_or_indented_directive_is_not_a_directive() {
    let analysis = analyze("= Note\n\n\\ifdef::web[]\n\n  ifdef::web[]\n");

    assert!(
        diagnostic_codes(&analysis)
            .iter()
            .all(|code| code != "unprocessed-directive"),
        "an escaped directive is written to be read, and AsciiDoc only reads a \
         directive that starts at the first column"
    );
}

#[test]
fn strict_mode_accepts_a_document_that_writes_directives() {
    // A directive is supported syntax that this analysis did not evaluate, not
    // a construct this version cannot read. Refusing it would refuse every
    // document that writes `include::` and is analyzed on its own, including
    // this repository's own documents.
    let options = AnalysisOptions {
        syntax: adocweave::SyntaxOptions {
            syntax_mode: SyntaxMode::Strict,
            ..adocweave::SyntaxOptions::default()
        },
        ..AnalysisOptions::default()
    };
    let analysis = Engine::new(options)
        .analyze("= Note\n\ninclude::part.adoc[]\n\nifdef::web[]\nBody\nendif::[]\n")
        .expect("strict analysis accepts unevaluated directives");

    assert_eq!(
        diagnostic_codes(&analysis),
        vec![
            "unprocessed-directive",
            "unprocessed-directive",
            "unprocessed-directive"
        ]
    );
}

#[test]
fn strict_mode_still_refuses_syntax_this_version_cannot_read() {
    let options = AnalysisOptions {
        syntax: adocweave::SyntaxOptions {
            syntax_mode: SyntaxMode::Strict,
            ..adocweave::SyntaxOptions::default()
        },
        ..AnalysisOptions::default()
    };
    let error = Engine::new(options)
        .analyze("= Note\n\n[source,rust,unknown]\n----\nfn main() {}\n----\n")
        .expect_err("strict analysis rejects unsupported syntax");

    assert!(matches!(error, ParseError::UnsupportedSyntax), "{error:?}");
}

#[test]
fn a_directive_block_is_marked_as_unevaluated_rather_than_unsupported() {
    let analysis = analyze("= Note\n\ninclude::part.adoc[]\n");

    let block = analysis
        .document()
        .blocks()
        .iter()
        .find_map(|block| match block {
            Block::Unsupported(unsupported) => Some(unsupported),
            _ => None,
        })
        .expect("directive block");
    assert_eq!(block.kind, UnsupportedKind::UnprocessedDirective);
    assert_eq!(block.raw, "include::part.adoc[]");
}
