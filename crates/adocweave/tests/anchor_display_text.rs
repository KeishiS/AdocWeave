//! Display text written after the comma in an inline anchor.
//!
//! AsciiDoc gives `[[id,xreftext]]` and `[[[id,xreftext]]]` their display text
//! after a comma, and the language specification names the bibliography form
//! when describing numbered citation styles. Before this split, the comma and
//! everything after it belonged to the identifier, so a document written to the
//! specification broke: `<<id>>` pointed at an identifier nobody wrote and the
//! reader was told the target did not exist.

use adocweave::resolution::RenderInputs;
use adocweave::{AnalysisOptions, Engine, output};

fn analyze(source: &str) -> adocweave::Analysis {
    Engine::new(AnalysisOptions::default())
        .analyze(source)
        .expect("analysis")
}

const NUMBERED: &str = "= Note\n\nBody <<smith2024>>\n\n[bibliography]\n== Sources\n\n* [[[smith2024,1]]] Smith, A. 2024.\n";

#[test]
fn a_bibliography_anchor_keeps_only_the_identifier() {
    let analysis = analyze(NUMBERED);
    let projection = output::projection::project(&analysis, &RenderInputs::default());

    let entry = &projection.catalogs.bibliography()[0];
    assert_eq!(entry.id, "smith2024");
    assert_eq!(entry.label.as_deref(), Some("1"));
}

#[test]
fn a_reference_to_a_numbered_entry_resolves_and_shows_the_number() {
    let analysis = analyze(NUMBERED);

    assert!(
        analysis.diagnostics().is_empty(),
        "a document written to the specification must not be reported as broken: {:?}",
        analysis
            .diagnostics()
            .iter()
            .map(|diagnostic| diagnostic.code.as_str())
            .collect::<Vec<_>>()
    );
    let target = analysis
        .document()
        .reference_targets()
        .iter()
        .find(|target| target.id == "smith2024")
        .expect("bibliography target");
    assert_eq!(target.label, "1");
}

#[test]
fn an_anchor_without_display_text_keeps_the_identifier_as_its_label() {
    let analysis = analyze("= Note\n\nBody <<smith2024>>\n\n* [[[smith2024]]] Smith, A. 2024.\n");

    let target = analysis
        .document()
        .reference_targets()
        .iter()
        .find(|target| target.id == "smith2024")
        .expect("bibliography target");
    assert_eq!(target.label, "smith2024");
    let projection = output::projection::project(&analysis, &RenderInputs::default());
    assert_eq!(projection.catalogs.bibliography()[0].label, None);
}

#[test]
fn an_inline_anchor_splits_at_the_comma_like_a_block_anchor() {
    // A block anchor already split here, so the two forms disagreed on where
    // the identifier ended.
    let analysis = analyze("= Note\n\nBody [[inline,Shown]] and <<inline>>.\n");

    let target = analysis
        .document()
        .reference_targets()
        .iter()
        .find(|target| target.id == "inline")
        .expect("inline anchor target");
    assert_eq!(target.label, "Shown");
}

#[test]
fn only_the_first_comma_separates_the_identifier() {
    let analysis = analyze("= Note\n\nBody [[id,a,b]] and <<id>>.\n");

    let target = analysis
        .document()
        .reference_targets()
        .iter()
        .find(|target| target.id == "id")
        .expect("inline anchor target");
    assert_eq!(target.label, "a,b");
}
