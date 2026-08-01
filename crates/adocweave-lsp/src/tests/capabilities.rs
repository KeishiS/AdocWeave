use super::*;

#[test]
fn initialize_negotiates_encoding_and_advertises_existing_features() {
    let mut service = LanguageService::default();
    let result = initialize(&mut service, &["utf-8", "utf-16"]);
    let value = serde_json::to_value(result).expect("serialize");

    assert_eq!(service.position_encoding, PositionEncoding::Utf8);
    assert_eq!(value["capabilities"]["positionEncoding"], "utf-8");
    assert_eq!(value["capabilities"]["textDocumentSync"]["change"], 2);
    assert_eq!(value["capabilities"]["documentSymbolProvider"], true);
    assert_eq!(value["capabilities"]["definitionProvider"], true);
    assert_eq!(value["capabilities"]["referencesProvider"], true);
    assert!(value["capabilities"]["documentLinkProvider"].is_object());
    assert!(value["capabilities"]["semanticTokensProvider"].is_object());
    assert_eq!(
        value["capabilities"]["renameProvider"]["prepareProvider"],
        true
    );
    assert_eq!(value["serverInfo"]["name"], "adocweave-lsp");
}

#[test]
fn minimal_client_never_receives_capability_gated_response_shapes() {
    let mut service = LanguageService::default();
    let params = typed(json!({
        "processId": null,
        "rootUri": null,
        "capabilities": {}
    }));
    let result = initialize_with_params(&mut service, params);
    let capabilities = serde_json::to_value(result.capabilities).expect("capabilities");

    assert_eq!(capabilities["positionEncoding"], "utf-16");
    assert!(capabilities.get("codeActionProvider").is_none());
    assert!(capabilities.get("semanticTokensProvider").is_none());
    assert!(capabilities.get("workspace").is_none());
    // A client that cannot ask before renaming is not told to ask.
    assert_eq!(capabilities["renameProvider"], true);

    open(
        &mut service,
        "file:///minimal.adoc",
        7,
        "= Minimal\n\n== Child\n\nhttps://example.com\n\ntext  \n",
    );
    let symbols = serde_json::to_value(
        service
            .document_symbols(&uri("file:///minimal.adoc"))
            .expect("symbols")
            .expect("response"),
    )
    .expect("serialize");
    assert_eq!(symbols[0]["name"], "Minimal");
    assert!(symbols[0].get("location").is_some());
    assert!(symbols[0].get("children").is_none());

    let hover = serde_json::to_value(
        service
            .hover(&uri("file:///minimal.adoc"), lsp::Position::new(0, 3))
            .expect("hover")
            .expect("response"),
    )
    .expect("serialize");
    assert!(hover["contents"].is_string());
    assert!(
        !hover["contents"]
            .as_str()
            .expect("hover text")
            .contains("**")
    );

    let diagnostics = service
        .diagnostics(&uri("file:///minimal.adoc"))
        .expect("diagnostics");
    assert_eq!(diagnostics.version, None);
    assert!(
        all_code_actions(&service, &uri("file:///minimal.adoc"))
            .expect("actions")
            .expect("response")
            .is_empty()
    );
    let links = service
        .document_links(&uri("file:///minimal.adoc"))
        .expect("links")
        .expect("response");
    assert!(links.iter().all(|link| link.tooltip.is_none()));
    assert_eq!(
        service
            .semantic_tokens(&uri("file:///minimal.adoc"))
            .expect("semantic tokens"),
        None
    );
}

#[test]
fn client_preferences_select_plaintext_hover_and_unversioned_code_action_edits() {
    let mut service = LanguageService::default();
    let params = typed(json!({
        "processId": null,
        "rootUri": null,
        "capabilities": {
            "workspace": {"workspaceEdit": {"documentChanges": false}},
            "textDocument": {
                "hover": {"contentFormat": ["plaintext", "markdown"]},
                "codeAction": {
                    "codeActionLiteralSupport": {
                        "codeActionKind": {"valueSet": ["quickfix"]}
                    },
                    "isPreferredSupport": false
                }
            }
        }
    }));
    initialize_with_params(&mut service, params);
    open(&mut service, "file:///mixed.adoc", 4, "= Mixed\n\ntext  \n");

    let hover = serde_json::to_value(
        service
            .hover(&uri("file:///mixed.adoc"), lsp::Position::new(0, 3))
            .expect("hover")
            .expect("response"),
    )
    .expect("serialize");
    assert_eq!(hover["contents"]["kind"], "plaintext");
    assert!(
        !hover["contents"]["value"]
            .as_str()
            .expect("hover text")
            .contains("**")
    );

    let actions = serde_json::to_value(
        all_code_actions(&service, &uri("file:///mixed.adoc"))
            .expect("actions")
            .expect("response"),
    )
    .expect("serialize");
    assert!(!actions.as_array().expect("actions").is_empty());
    assert!(actions[0]["edit"]["changes"]["file:///mixed.adoc"].is_array());
    assert!(actions[0]["edit"].get("documentChanges").is_none());
    assert!(actions[0].get("isPreferred").is_none());
}
