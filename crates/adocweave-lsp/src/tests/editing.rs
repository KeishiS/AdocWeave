use super::*;

#[test]
fn code_actions_use_typed_versioned_workspace_edits() {
    let mut service = LanguageService::default();
    open(&mut service, "file:///fix.adoc", 4, "==Title\ntext  \n");
    let actions = all_code_actions(&service, &uri("file:///fix.adoc"))
        .expect("actions")
        .expect("response");
    let value = serde_json::to_value(actions).expect("serialize");

    assert_eq!(value.as_array().expect("actions").len(), 2);
    assert!(
        value
            .as_array()
            .expect("actions")
            .iter()
            .all(|action| { action["edit"]["documentChanges"][0]["textDocument"]["version"] == 4 })
    );
}

#[test]
fn code_actions_respect_the_requested_range_and_kind() {
    let mut service = LanguageService::default();
    let document_uri = uri("file:///scoped-fix.adoc");
    open(&mut service, document_uri.as_str(), 1, "==Title\ntext  \n");

    let line_two = service
        .code_actions(
            &document_uri,
            lsp::Range::new(lsp::Position::new(1, 0), lsp::Position::new(1, 6)),
            &lsp::CodeActionContext {
                diagnostics: Vec::new(),
                only: Some(vec![lsp::CodeActionKind::QUICKFIX]),
                trigger_kind: Some(lsp::CodeActionTriggerKind::INVOKED),
            },
        )
        .expect("actions")
        .expect("response");
    assert_eq!(line_two.len(), 1);

    let wrong_kind = service
        .code_actions(
            &document_uri,
            lsp::Range::new(lsp::Position::new(0, 0), lsp::Position::new(1, 6)),
            &lsp::CodeActionContext {
                diagnostics: Vec::new(),
                only: Some(vec![lsp::CodeActionKind::SOURCE]),
                trigger_kind: Some(lsp::CodeActionTriggerKind::INVOKED),
            },
        )
        .expect("actions")
        .expect("response");
    assert!(wrong_kind.is_empty());
}

#[test]
fn formatting_is_idempotent_and_preserves_literal_bodies() {
    let source = "before  \n\n....\ncode  \n....\n\nafter  ";
    let mut service = LanguageService::default();
    open(&mut service, "file:///format.adoc", 1, source);
    let edits = service
        .formatting(&uri("file:///format.adoc"))
        .expect("format")
        .expect("response");
    assert!(edits.iter().all(|edit| edit.range.start.line != 3));
    let formatted = apply_edits(source, &edits);
    assert!(formatted.contains("....\ncode  \n....\n"));

    assert!(
        change(
            &mut service,
            "file:///format.adoc",
            2,
            json!([{"text": formatted}])
        )
        .expect("change")
    );
    assert!(
        service
            .formatting(&uri("file:///format.adoc"))
            .expect("format")
            .expect("response")
            .is_empty()
    );
}
