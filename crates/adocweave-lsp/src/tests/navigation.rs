use super::*;

#[test]
fn definition_resolves_local_and_open_document_targets() {
    let mut service = LanguageService::default();
    open_reference_workspace(&mut service);
    let document_uri = uri("file:///a.adoc");

    let local = service
        .definition(&document_uri, lsp::Position::new(3, 7))
        .expect("definition")
        .expect("local definition");
    let local = serde_json::to_value(local).expect("serialize");
    assert_eq!(local["uri"], "file:///a.adoc");
    assert_eq!(local["range"]["start"]["line"], 1);

    let external = service
        .definition(&document_uri, lsp::Position::new(3, 28))
        .expect("definition")
        .expect("document definition");
    let external = serde_json::to_value(external).expect("serialize");
    assert_eq!(external["uri"], "file:///b.adoc");
    assert_eq!(external["range"]["start"]["line"], 1);
}

#[test]
fn references_use_one_workspace_identity_for_local_and_document_xrefs() {
    let mut service = LanguageService::default();
    open_reference_workspace(&mut service);
    let locations = service
        .references(&uri("file:///a.adoc"), lsp::Position::new(0, 3), true)
        .expect("references")
        .expect("locations");
    let values = serde_json::to_value(locations).expect("serialize");

    assert_eq!(values.as_array().expect("locations").len(), 3);
    assert!(
        values
            .as_array()
            .expect("locations")
            .iter()
            .any(|location| location["uri"] == "file:///b.adoc")
    );
}

#[test]
fn references_report_unicode_ranges_in_utf8_and_utf16() {
    let source = "[[節😀]]\n== 見出し\n\n<<節😀>>\n";
    for (encoding, expected_end) in [(PositionEncoding::Utf8, 9), (PositionEncoding::Utf16, 5)] {
        let mut service = LanguageService::default();
        service.position_encoding = encoding;
        open(&mut service, "file:///unicode-ref.adoc", 1, source);
        let references = service
            .references(
                &uri("file:///unicode-ref.adoc"),
                lsp::Position::new(0, 2),
                false,
            )
            .expect("references")
            .expect("locations");

        assert_eq!(references.len(), 1);
        assert_eq!(references[0].range.start.character, 2);
        assert_eq!(references[0].range.end.character, expected_end);
    }
}

#[test]
fn document_links_keep_safe_urls_and_xrefs_separate_but_navigable() {
    let mut service = LanguageService::default();
    open_reference_workspace(&mut service);
    let links = service
        .document_links(&uri("file:///a.adoc"))
        .expect("document links")
        .expect("links");
    let values = serde_json::to_value(links).expect("serialize");
    let targets = values
        .as_array()
        .expect("links")
        .iter()
        .map(|link| link["target"].as_str().expect("target"))
        .collect::<Vec<_>>();

    assert_eq!(targets.len(), 3);
    assert!(targets.contains(&"https://example.com/"));
    assert!(targets.contains(&"file:///a.adoc#target"));
    assert!(targets.contains(&"file:///b.adoc#other"));
}

#[derive(Debug)]
struct TestHostIndex {
    complete: bool,
    fail: bool,
}

impl HostReferenceIndex for TestHostIndex {
    fn definition(&self, request: &HostReferenceRequest) -> Result<Option<lsp::Location>, String> {
        assert!(request.source_generation > 0);
        if self.fail {
            return Err("host index unavailable".to_owned());
        }
        Ok(
            matches!(request.target, ReferenceKey::Scheme { .. }).then(|| {
                lsp::Location::new(
                    uri("file:///resolved-note.adoc"),
                    lsp::Range::new(lsp::Position::new(2, 0), lsp::Position::new(2, 5)),
                )
            }),
        )
    }

    fn references(
        &self,
        request: &HostReferenceRequest,
        _include_declaration: bool,
    ) -> Result<Option<Vec<lsp::Location>>, String> {
        assert!(request.source_generation > 0);
        if self.fail {
            return Err("host index unavailable".to_owned());
        }
        Ok(self.complete.then(|| {
            vec![
                lsp::Location::new(
                    request.source.clone(),
                    lsp::Range::new(lsp::Position::new(0, 2), lsp::Position::new(0, 8)),
                ),
                lsp::Location::new(
                    uri("file:///b.adoc"),
                    lsp::Range::new(lsp::Position::new(3, 7), lsp::Position::new(3, 13)),
                ),
            ]
        }))
    }

    fn is_complete(&self) -> bool {
        self.complete
    }
}

#[test]
fn definition_uses_injected_host_index_for_scheme_references() {
    let mut service = LanguageService::with_host_index(Arc::new(TestHostIndex {
        complete: true,
        fail: false,
    }));
    open(&mut service, "file:///a.adoc", 1, "xref:note:42[Note]\n");
    let definition = service
        .definition(&uri("file:///a.adoc"), lsp::Position::new(0, 8))
        .expect("definition")
        .expect("resolved");
    let value = serde_json::to_value(definition).expect("serialize");
    assert_eq!(value["uri"], "file:///resolved-note.adoc");
    let links = service
        .document_links(&uri("file:///a.adoc"))
        .expect("document links")
        .expect("links");
    assert_eq!(
        links[0].target.as_ref().map(lsp::Url::as_str),
        Some("file:///resolved-note.adoc")
    );
    let references = service
        .references(&uri("file:///a.adoc"), lsp::Position::new(0, 8), true)
        .expect("references")
        .expect("resolved references");
    assert_eq!(references.len(), 2);
}

#[test]
fn rename_uses_workspace_index_and_prefers_a_complete_host_index() {
    let mut incomplete = LanguageService::default();
    open(&mut incomplete, "file:///a.adoc", 1, "[[target]]\n== A\n");
    let local_edit = incomplete
        .rename(&uri("file:///a.adoc"), lsp::Position::new(0, 3), "renamed")
        .expect("rename")
        .expect("workspace edit");
    assert_eq!(local_edit.changes.expect("changes").len(), 1);

    let mut complete = LanguageService::with_host_index(Arc::new(TestHostIndex {
        complete: true,
        fail: false,
    }));
    open(&mut complete, "file:///a.adoc", 1, "[[target]]\n== A\n");
    let edit = complete
        .rename(&uri("file:///a.adoc"), lsp::Position::new(0, 3), "renamed")
        .expect("rename")
        .expect("complete edit");
    assert_eq!(edit.changes.expect("changes").len(), 2);
}

#[test]
fn prepare_rename_answers_only_where_rename_produces_an_edit() {
    let mut service = LanguageService::default();
    initialize(&mut service, &["utf-16"]);
    open(
        &mut service,
        "file:///a.adoc",
        1,
        "[[target]]\n== A\n\nplain text\n",
    );
    let document = uri("file:///a.adoc");

    let anchor = lsp::Position::new(0, 3);
    let response = service
        .prepare_rename(&document, anchor)
        .expect("prepare rename")
        .expect("renameable position");
    assert_eq!(
        response,
        lsp::PrepareRenameResponse::RangeWithPlaceholder {
            range: lsp::Range::new(lsp::Position::new(0, 2), lsp::Position::new(0, 8)),
            placeholder: "target".to_owned(),
        }
    );
    assert!(
        service
            .rename(&document, anchor, "renamed")
            .expect("rename")
            .is_some(),
        "a position prepareRename accepts must produce an edit",
    );

    // Body text holds no anchor. Before prepareRename the editor started a
    // rename here and received nothing.
    let prose = lsp::Position::new(3, 2);
    assert!(
        service
            .prepare_rename(&document, prose)
            .expect("prepare rename")
            .is_none(),
    );
    assert!(
        service
            .rename(&document, prose, "renamed")
            .expect("rename")
            .is_none(),
        "a position prepareRename rejects must produce no edit",
    );
}

#[test]
fn prepare_rename_is_silent_for_clients_that_do_not_declare_support() {
    let mut service = LanguageService::default();
    let params = typed(json!({
        "processId": null,
        "rootUri": null,
        "capabilities": {
            "general": {"positionEncodings": ["utf-16"]},
            "textDocument": {"rename": {"dynamicRegistration": false}}
        }
    }));
    let result = initialize_with_params(&mut service, params);
    assert_eq!(
        result.capabilities.rename_provider,
        Some(lsp::OneOf::Left(true)),
        "a client without prepare support keeps the plain declaration",
    );

    open(&mut service, "file:///a.adoc", 1, "[[target]]\n== A\n");
    assert!(
        service
            .prepare_rename(&uri("file:///a.adoc"), lsp::Position::new(0, 3))
            .expect("prepare rename")
            .is_none(),
    );
    assert!(
        service
            .rename(&uri("file:///a.adoc"), lsp::Position::new(0, 3), "renamed")
            .expect("rename")
            .is_some(),
        "rename itself keeps working without prepare support",
    );
}

#[test]
fn host_index_failure_does_not_disable_core_language_features() {
    let mut service = LanguageService::with_host_index(Arc::new(TestHostIndex {
        complete: false,
        fail: true,
    }));
    open(
        &mut service,
        "file:///a.adoc",
        1,
        "= Title\n\nxref:note:42[Note]\n",
    );
    assert!(
        service
            .definition(&uri("file:///a.adoc"), lsp::Position::new(2, 8))
            .is_err()
    );
    assert!(
        service
            .document_symbols(&uri("file:///a.adoc"))
            .expect("symbols remain available")
            .is_some()
    );
}
