use super::*;

#[test]
fn diagnostics_use_current_version_codes_and_unicode_positions() {
    let text = "日😀e\u{301} ";
    for (encoding, expected_start, expected_end) in [
        (PositionEncoding::Utf8, 10, 11),
        (PositionEncoding::Utf16, 5, 6),
    ] {
        let mut service = LanguageService::default();
        service.position_encoding = encoding;
        open(&mut service, "file:///unicode.adoc", 3, text);
        let diagnostics = service
            .diagnostics(&uri("file:///unicode.adoc"))
            .expect("diagnostics");
        assert_eq!(diagnostics.version, Some(3));
        assert_eq!(
            diagnostics.diagnostics[0].code,
            Some(lsp::NumberOrString::String(
                "trailing-whitespace".to_owned()
            ))
        );
        assert_eq!(
            diagnostics.diagnostics[0].range.start.character,
            expected_start
        );
        assert_eq!(diagnostics.diagnostics[0].range.end.character, expected_end);
    }
}

#[test]
fn link_and_xref_diagnostics_share_ranges_and_quick_fixes() {
    let mut service = LanguageService::default();
    initialize(&mut service, &["utf-16"]);
    open(
        &mut service,
        "file:///references.adoc",
        2,
        "日😀 link:guide.adoc[Guide]\nxref:data.json[Data]\n",
    );

    let diagnostics = service
        .diagnostics(&uri("file:///references.adoc"))
        .expect("diagnostics");
    assert_eq!(diagnostics.version, Some(2));
    assert_eq!(
        diagnostics.diagnostics.len(),
        2,
        "{:#?}",
        diagnostics.diagnostics
    );
    assert_eq!(
        diagnostics.diagnostics[0].code,
        Some(lsp::NumberOrString::String("asciidoc-file-link".to_owned()))
    );
    assert_eq!(
        diagnostics.diagnostics[0].severity,
        Some(lsp::DiagnosticSeverity::WARNING)
    );
    assert_eq!(
        diagnostics.diagnostics[0].range,
        lsp::Range::new(lsp::Position::new(0, 4), lsp::Position::new(0, 8))
    );
    assert_eq!(
        diagnostics.diagnostics[1].code,
        Some(lsp::NumberOrString::String("non-asciidoc-xref".to_owned()))
    );
    assert_eq!(
        diagnostics.diagnostics[1].range,
        lsp::Range::new(lsp::Position::new(1, 0), lsp::Position::new(1, 4))
    );

    let actions = serde_json::to_value(
        all_code_actions(&service, &uri("file:///references.adoc"))
            .expect("actions")
            .expect("response"),
    )
    .expect("serialize");
    let replacements = actions
        .as_array()
        .expect("actions")
        .iter()
        .filter_map(|action| action["edit"]["documentChanges"][0]["edits"][0]["newText"].as_str())
        .collect::<Vec<_>>();
    assert!(replacements.contains(&"xref"));
    assert!(replacements.contains(&"link"));
}

#[test]
fn opt_in_macro_boundary_diagnostic_uses_lsp_positions() {
    let mut service = LanguageService::default();
    service.position_encoding = PositionEncoding::Utf16;
    service
        .update_configuration(json!({"enabledRules": ["macro-boundary"]}))
        .expect("configuration");
    let mut jobs = service.begin_open(typed(json!({
        "textDocument": {
            "uri": "file:///macro-boundary.adoc",
            "languageId": "asciidoc",
            "version": 4,
            "text": "日😀xref:guide.adoc[Guide]\n"
        }
    })));
    let job = jobs.pop().expect("analysis job");
    adopt(&mut service, job);

    let diagnostics = service
        .diagnostics(&uri("file:///macro-boundary.adoc"))
        .expect("diagnostics");
    let boundary = diagnostics
        .diagnostics
        .iter()
        .find(|diagnostic| {
            diagnostic.code == Some(lsp::NumberOrString::String("macro-boundary".to_owned()))
        })
        .expect("macro-boundary diagnostic");
    assert_eq!(boundary.severity, Some(lsp::DiagnosticSeverity::WARNING));
    assert_eq!(
        boundary.range,
        lsp::Range::new(lsp::Position::new(0, 3), lsp::Position::new(0, 7))
    );
}

#[test]
fn diagnostics_preserve_invalid_explicit_ordered_number_ranges() {
    let mut service = LanguageService::default();
    open(
        &mut service,
        "file:///ordered-list.adoc",
        1,
        "4294967296. overflow\n0. zero\n",
    );
    let diagnostics = service
        .diagnostics(&uri("file:///ordered-list.adoc"))
        .expect("diagnostics");

    assert_eq!(diagnostics.diagnostics.len(), 2);
    assert!(diagnostics.diagnostics.iter().all(|diagnostic| {
        diagnostic.code
            == Some(lsp::NumberOrString::String(
                "invalid-list-presentation".to_owned(),
            ))
            && diagnostic.range.start.line <= 1
    }));
}

#[test]
fn diagnostics_preserve_invalid_table_presentation_ranges() {
    let mut service = LanguageService::default();
    open(
        &mut service,
        "file:///table.adoc",
        1,
        "[frame=ends,frame=sides,grid=diagonal,width=75%,options=autowidth]\n|===\n|cell\n|===\n",
    );
    let diagnostics = service
        .diagnostics(&uri("file:///table.adoc"))
        .expect("diagnostics");

    assert_eq!(diagnostics.diagnostics.len(), 3);
    assert!(diagnostics.diagnostics.iter().all(|diagnostic| {
        diagnostic.code == Some(lsp::NumberOrString::String("invalid-table".to_owned()))
            && diagnostic.range.start.line == 0
    }));
}

#[test]
fn close_clears_diagnostics() {
    let mut service = LanguageService::default();
    let document_uri = uri("file:///a.adoc");
    open(&mut service, document_uri.as_str(), 1, "bad ");
    assert!(service.close(&document_uri).0);
    let diagnostics = service.diagnostics(&document_uri).expect("clear");
    assert!(diagnostics.diagnostics.is_empty());
    assert_eq!(diagnostics.version, None);
}
