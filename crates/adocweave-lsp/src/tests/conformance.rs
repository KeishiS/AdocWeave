use super::*;

#[test]
fn release_fixture_is_accepted_by_all_existing_features() {
    let source = include_str!("../../../../fixtures/release/core.adoc");
    let mut service = LanguageService::default();
    let document_uri = uri("file:///release.adoc");
    open(&mut service, document_uri.as_str(), 1, source);
    assert!(
        service
            .diagnostics(&document_uri)
            .expect("diagnostics")
            .diagnostics
            .is_empty()
    );
    assert!(
        service
            .formatting(&document_uri)
            .expect("format")
            .expect("response")
            .is_empty()
    );
    let symbols = service
        .document_symbols(&document_uri)
        .expect("symbols")
        .expect("response");
    let symbols = serde_json::to_value(symbols).expect("serialize");
    assert_eq!(symbols[0]["name"], "AdocWeave 初期リリース");
}

#[test]
fn conformance_fixture_is_reused_by_editor_projections() {
    let source = adocweave::output::conformance::fixture_source("bibliography-consumer-coverage")
        .expect("shared inline conformance fixture");
    let mut service = LanguageService::default();
    let document_uri = uri("file:///conformance.adoc");
    open(&mut service, document_uri.as_str(), 1, &source);

    let symbols = service
        .document_symbols(&document_uri)
        .expect("symbols")
        .expect("response");
    let symbols = serde_json::to_value(symbols).expect("serialize symbols");
    assert_eq!(symbols.as_array().expect("symbol array").len(), 1);
    assert_eq!(symbols[0]["name"], "References");

    let links = service
        .document_links(&document_uri)
        .expect("document links")
        .expect("response");
    assert_eq!(links.len(), 3);

    let tokens = service
        .semantic_tokens(&document_uri)
        .expect("semantic tokens")
        .expect("response");
    let tokens = serde_json::to_value(tokens).expect("serialize tokens");
    assert!(!tokens["data"].as_array().expect("token data").is_empty());
}
