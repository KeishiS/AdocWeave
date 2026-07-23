//! Typed Language Server service and transport tests.

use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use std::{fs, path::PathBuf};

use adocweave::Engine;
use adocweave::preprocessor::{PreprocessedAnalysis, ProjectionLimits, preprocess};
use adocweave::reference::ReferenceKey;
use async_lsp::lsp_types as lsp;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};

use super::{HostReferenceIndex, HostReferenceRequest, PositionEncoding, run};
use crate::service::LanguageService;
use crate::state::{Adoption, AnalysisJob, WorkspaceAnalysis, WorkspaceProblem};

fn typed<T: DeserializeOwned>(value: Value) -> T {
    serde_json::from_value(value).expect("valid LSP value")
}

fn uri(value: &str) -> lsp::Url {
    value.parse().expect("valid URI")
}

async fn write_message(output: &mut (impl AsyncWriteExt + Unpin), message: &Value) {
    let body = serde_json::to_vec(message).expect("serialize");
    output
        .write_all(format!("Content-Length: {}\r\n\r\n", body.len()).as_bytes())
        .await
        .expect("header");
    output.write_all(&body).await.expect("body");
    output.flush().await.expect("flush");
}

async fn read_message(input: &mut BufReader<impl tokio::io::AsyncRead + Unpin>) -> Value {
    let mut content_length = None;
    loop {
        let mut header = String::new();
        input.read_line(&mut header).await.expect("header");
        if header == "\r\n" {
            break;
        }
        if let Some(value) = header.strip_prefix("Content-Length:") {
            content_length = Some(value.trim().parse::<usize>().expect("length"));
        }
    }
    let mut body = vec![0; content_length.expect("content length")];
    input.read_exact(&mut body).await.expect("body");
    serde_json::from_slice(&body).expect("json")
}

fn initialize(service: &mut LanguageService, encodings: &[&str]) -> lsp::InitializeResult {
    let params = typed(json!({
        "processId": null,
        "rootUri": null,
        "capabilities": {"general": {"positionEncodings": encodings}}
    }));
    service.initialize(&params)
}

fn open(service: &mut LanguageService, uri: &str, version: i32, text: &str) {
    let jobs = service.begin_open(typed(json!({
        "textDocument": {
            "uri": uri,
            "languageId": "asciidoc",
            "version": version,
            "text": text
        }
    })));
    for job in jobs {
        adopt(service, job);
    }
}

fn change(
    service: &mut LanguageService,
    uri: &str,
    version: i32,
    changes: Value,
) -> Result<bool, String> {
    let jobs = service.begin_change(typed(json!({
        "textDocument": {"uri": uri, "version": version},
        "contentChanges": changes
    })))?;
    if jobs.is_empty() {
        return Ok(false);
    }
    for job in jobs {
        adopt(service, job);
    }
    Ok(true)
}

fn adopt(service: &mut LanguageService, job: AnalysisJob) {
    let analysis = job
        .request
        .analyze(job.cancellation.as_ref())
        .expect("analysis");
    assert_eq!(service.adopt(&job, analysis), Adoption::Adopted);
    if let Some(input) = &job.workspace {
        let document = match preprocess(&input.root.text, &input.snapshot, &input.options) {
            Ok(document) => document,
            Err(error) => {
                assert_eq!(
                    service.adopt_workspace_problem(
                        &job,
                        WorkspaceProblem {
                            source_id: error
                                .source_id
                                .as_ref()
                                .map(|source_id| source_id.as_str().to_owned()),
                            range: error.range,
                            code: error.kind.as_str().to_owned(),
                            message: error.to_string(),
                        }
                    ),
                    Adoption::Adopted
                );
                return;
            }
        };
        let analysis = Engine::new(job.request.options.clone())
            .analyze(&document.source)
            .expect("workspace analysis");
        let preprocessed = PreprocessedAnalysis { document, analysis };
        let projection = preprocessed
            .project_origins(ProjectionLimits::default())
            .expect("workspace projection");
        assert_eq!(
            service.adopt_workspace(
                &job,
                WorkspaceAnalysis {
                    document: Arc::new(preprocessed.document),
                    analysis: Arc::new(preprocessed.analysis),
                    projection: Arc::new(projection),
                    resource_versions: input.resource_versions.clone(),
                }
            ),
            Adoption::Adopted
        );
    }
}

#[test]
fn analysis_adoption_rejects_a_stale_workspace_generation() {
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let root: PathBuf = std::env::temp_dir().join(format!("adocweave-stale-workspace-{unique}"));
    let root_path = root.join("root.adoc");
    let part_path = root.join("part.adoc");
    fs::create_dir_all(&root).expect("workspace");
    fs::write(&root_path, "include::part.adoc[]\n").expect("root document");
    fs::write(&part_path, "old\n").expect("part document");
    let root_uri = lsp::Url::from_directory_path(&root).expect("root URI");
    let document_uri = lsp::Url::from_file_path(&root_path).expect("document URI");
    let part_uri = lsp::Url::from_file_path(&part_path).expect("part URI");

    let mut service = LanguageService::default();
    let params = typed(json!({
        "processId": null,
        "rootUri": root_uri,
        "capabilities": {}
    }));
    service.initialize(&params);
    let job = service
        .begin_open(typed(json!({
            "textDocument": {
                "uri": document_uri,
                "languageId": "asciidoc",
                "version": 1,
                "text": "include::part.adoc[]\n"
            }
        })))
        .into_iter()
        .next()
        .expect("analysis job");
    let analysis = job
        .request
        .analyze(job.cancellation.as_ref())
        .expect("analysis");

    fs::write(&part_path, "new\n").expect("changed part");
    service.workspace_files_changed(typed(json!({
        "changes": [{"uri": part_uri, "type": 2}]
    })));

    assert_eq!(service.adopt(&job, analysis), Adoption::Stale);
    fs::remove_dir_all(root).expect("cleanup");
}

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
    assert_eq!(value["serverInfo"]["name"], "adocweave-lsp");
}

#[test]
fn workspace_configuration_updates_and_caps_debounce() {
    let mut service = LanguageService::default();
    service
        .update_configuration(json!({"adocweave": {"debounceMs": 25}}))
        .expect("configuration");
    assert_eq!(service.debounce_ms(), 25);

    service
        .update_configuration(json!({"debounceMs": 50_000}))
        .expect("configuration");
    assert_eq!(service.debounce_ms(), 1_000);
    assert!(
        service
            .update_configuration(json!({"unknown": true}))
            .is_err()
    );
}

#[test]
fn workspace_include_analysis_uses_versioned_resources_and_projects_diagnostics() {
    let mut service = LanguageService::default();
    open(&mut service, "file:///book/part.adoc", 3, "==Part\n");
    open(
        &mut service,
        "file:///book/root.adoc",
        1,
        "= Root\n\ninclude::part.adoc[]\n",
    );

    let root = service
        .documents
        .get("file:///book/root.adoc")
        .expect("root");
    let workspace = root.workspace_analysis().expect("workspace analysis");
    assert!(workspace.analysis.source().contains("==Part"));
    assert_eq!(
        workspace.resource_versions.get("file:///book/part.adoc"),
        Some(&3)
    );
    let links = service
        .document_links(&uri("file:///book/root.adoc"))
        .expect("document links")
        .expect("links");
    assert!(links.iter().any(|link| {
        link.target.as_ref().map(lsp::Url::as_str) == Some("file:///book/part.adoc")
            && link.range.start == lsp::Position::new(2, 9)
    }));
    let definition = service
        .definition(&uri("file:///book/root.adoc"), lsp::Position::new(2, 10))
        .expect("definition")
        .expect("include definition");
    let lsp::GotoDefinitionResponse::Scalar(definition) = definition else {
        panic!("scalar include definition");
    };
    assert_eq!(definition.uri.as_str(), "file:///book/part.adoc");

    let diagnostics = service
        .diagnostics(&uri("file:///book/part.adoc"))
        .expect("diagnostics");
    assert_eq!(
        diagnostics
            .diagnostics
            .iter()
            .filter(|diagnostic| diagnostic.code
                == Some(lsp::NumberOrString::String(
                    "heading-marker-space".to_owned()
                )))
            .count(),
        1,
        "direct and projected diagnostics are deduplicated: {:#?}",
        diagnostics.diagnostics
    );

    let root_generation = service
        .documents
        .get("file:///book/root.adoc")
        .expect("root")
        .request
        .revision
        .generation;
    assert!(
        change(
            &mut service,
            "file:///book/part.adoc",
            4,
            json!([{"text": "== Part\n"}]),
        )
        .expect("change")
    );
    let reanalyzed = service
        .documents
        .get("file:///book/root.adoc")
        .expect("root");
    assert!(reanalyzed.request.revision.generation > root_generation);
    assert!(reanalyzed.workspace_analysis().is_some());
}

#[test]
fn missing_include_is_reported_as_a_project_diagnostic_at_the_directive() {
    let mut service = LanguageService::default();
    open(
        &mut service,
        "file:///book/root.adoc",
        1,
        "= Root\n\ninclude::missing.adoc[]\n",
    );

    let diagnostics = service
        .diagnostics(&uri("file:///book/root.adoc"))
        .expect("diagnostics");
    let problem = diagnostics
        .diagnostics
        .iter()
        .find(|diagnostic| diagnostic.source.as_deref() == Some("adocweave-project"))
        .expect("project diagnostic");
    assert_eq!(
        problem.code,
        Some(lsp::NumberOrString::String("missing-resource".to_owned()))
    );
    assert_eq!(problem.range.start.line, 2);
}

#[test]
fn document_updates_are_ordered_and_stale_versions_are_ignored() {
    let mut service = LanguageService::default();
    open(&mut service, "file:///a.adoc", 2, "= A");
    open(&mut service, "file:///b.adoc", 2, "= B");

    assert!(
        !change(
            &mut service,
            "file:///a.adoc",
            1,
            json!([{"text": "stale"}])
        )
        .expect("stale change")
    );
    assert_eq!(
        service
            .documents
            .get("file:///a.adoc")
            .expect("a")
            .analysis()
            .expect("analysis")
            .source(),
        "= A"
    );
    assert_eq!(
        service
            .documents
            .get("file:///b.adoc")
            .expect("b")
            .analysis()
            .expect("analysis")
            .source(),
        "= B"
    );
}

#[test]
fn incremental_changes_apply_sequentially_with_negotiated_positions() {
    let mut service = LanguageService::default();
    open(&mut service, "file:///a.adoc", 1, "a😀c");
    assert!(
        change(
            &mut service,
            "file:///a.adoc",
            2,
            json!([
                {
                    "range": {
                        "start": {"line": 0, "character": 1},
                        "end": {"line": 0,"character": 3}
                    },
                    "text": "b"
                },
                {
                    "range": {
                        "start": {"line": 0, "character": 2},
                        "end": {"line": 0,"character": 3}
                    },
                    "text": "d"
                }
            ]),
        )
        .expect("incremental change")
    );
    assert_eq!(
        service
            .documents
            .get("file:///a.adoc")
            .expect("document")
            .analysis()
            .expect("analysis")
            .source(),
        "abd"
    );
}

#[test]
fn incremental_changes_preserve_crlf_line_boundaries() {
    let mut service = LanguageService::default();
    open(&mut service, "file:///crlf.adoc", 1, "one\r\ntwo\r\n");
    assert!(
        change(
            &mut service,
            "file:///crlf.adoc",
            2,
            json!([{
                "range": {
                    "start": {"line": 1, "character": 0},
                    "end": {"line": 1, "character": 3}
                },
                "text": "second"
            }])
        )
        .expect("incremental change")
    );
    assert_eq!(
        service
            .documents
            .get("file:///crlf.adoc")
            .expect("document")
            .analysis()
            .expect("analysis")
            .source(),
        "one\r\nsecond\r\n"
    );
}

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
fn close_clears_diagnostics() {
    let mut service = LanguageService::default();
    let document_uri = uri("file:///a.adoc");
    open(&mut service, document_uri.as_str(), 1, "bad ");
    assert!(service.close(&document_uri).0);
    let diagnostics = service.diagnostics(&document_uri).expect("clear");
    assert!(diagnostics.diagnostics.is_empty());
    assert_eq!(diagnostics.version, None);
}

#[test]
fn document_symbols_preserve_hierarchy_and_ranges() {
    let mut service = LanguageService::default();
    open(
        &mut service,
        "file:///symbols.adoc",
        1,
        "= 題名😀\n\n== 一\n\n=== 子\n\n== 二\n",
    );
    let response = service
        .document_symbols(&uri("file:///symbols.adoc"))
        .expect("symbols")
        .expect("response");
    let value = serde_json::to_value(response).expect("serialize");

    assert_eq!(value[0]["name"], "題名😀");
    assert_eq!(value[0]["children"][0]["name"], "一");
    assert_eq!(value[0]["children"][0]["children"][0]["name"], "子");
    assert_eq!(value[0]["children"][1]["name"], "二");
    assert_eq!(value[0]["selectionRange"]["end"]["character"], 6);
}

#[test]
fn code_actions_use_typed_versioned_workspace_edits() {
    let mut service = LanguageService::default();
    open(&mut service, "file:///fix.adoc", 4, "==Title\ntext  \n");
    let actions = service
        .code_actions(&uri("file:///fix.adoc"))
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

fn apply_edits(source: &str, edits: &[lsp::TextEdit]) -> String {
    use adocweave::source::{Position, PositionEncoding as CorePositionEncoding, SourceDocument};

    let index = SourceDocument::new(source).expect("line index");
    let mut byte_edits = edits
        .iter()
        .map(|edit| {
            let position = |position: lsp::Position| Position {
                line: position.line,
                character: position.character,
            };
            let start = index
                .position_to_offset(position(edit.range.start), CorePositionEncoding::Utf16)
                .expect("start")
                .to_usize();
            let end = index
                .position_to_offset(position(edit.range.end), CorePositionEncoding::Utf16)
                .expect("end")
                .to_usize();
            (start, end, edit.new_text.clone())
        })
        .collect::<Vec<_>>();
    byte_edits.sort_by_key(|(start, end, _)| (*start, *end));
    let mut output = source.to_owned();
    for (start, end, replacement) in byte_edits.into_iter().rev() {
        output.replace_range(start..end, &replacement);
    }
    output
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

#[test]
fn hover_and_completion_use_the_same_analysis_snapshot() {
    let mut service = LanguageService::default();
    open(
        &mut service,
        "file:///features.adoc",
        1,
        "= 題名😀\n\n[source, ru]\n----\ncode\n----\n",
    );
    let document_uri = uri("file:///features.adoc");
    let hover = service
        .hover(&document_uri, lsp::Position::new(0, 4))
        .expect("hover")
        .expect("value");
    let hover = serde_json::to_value(hover).expect("serialize");
    assert!(
        hover["contents"]["value"]
            .as_str()
            .expect("text")
            .contains("Generated ID")
    );

    let completion = service
        .completion(&document_uri, lsp::Position::new(2, 11))
        .expect("completion")
        .expect("response");
    let completion = serde_json::to_value(completion).expect("serialize");
    assert_eq!(completion, json!([{"label": "rust", "kind": 12}]));
}

#[test]
fn hover_and_completion_cover_attributes_references_links_and_math() {
    let mut service = LanguageService::default();
    open(
        &mut service,
        "file:///rich-features.adoc",
        1,
        "= Title\n:name: value\n\n[[part]]\n== Part\n\nhttps://example.com[Site] <<part>> stem:[x+y]\n",
    );
    let document_uri = uri("file:///rich-features.adoc");
    for (position, expected) in [
        (lsp::Position::new(1, 2), "document attribute"),
        (lsp::Position::new(3, 3), "reference target"),
        (lsp::Position::new(6, 3), "external link"),
        (lsp::Position::new(6, 29), "cross reference"),
        (lsp::Position::new(6, 43), "LaTeX formula"),
    ] {
        let hover = service
            .hover(&document_uri, position)
            .expect("hover")
            .expect("value");
        let value = serde_json::to_value(hover).expect("serialize");
        assert!(
            value["contents"]["value"]
                .as_str()
                .expect("hover text")
                .contains(expected),
            "expected {expected} at {position:?}: {value}"
        );
    }
    let completion = service
        .completion(&document_uri, lsp::Position::new(6, 31))
        .expect("completion")
        .expect("response");
    let value = serde_json::to_value(completion).expect("serialize");
    assert!(
        value
            .as_array()
            .expect("items")
            .iter()
            .any(|item| item["label"] == "part")
    );
}

#[test]
fn hover_and_completion_cover_common_block_metadata() {
    let mut service = LanguageService::default();
    open(
        &mut service,
        "file:///metadata.adoc",
        1,
        ".Visible\n[#item.lead%collapsible,cols=2]\nParagraph\n",
    );
    let document_uri = uri("file:///metadata.adoc");
    for (position, expected) in [
        (lsp::Position::new(0, 2), "block title"),
        (lsp::Position::new(1, 3), "reference target"),
        (lsp::Position::new(1, 8), "block role"),
        (lsp::Position::new(1, 14), "block option"),
        (lsp::Position::new(1, 28), "cols"),
    ] {
        let hover = service
            .hover(&document_uri, position)
            .expect("hover")
            .expect("value");
        let value = serde_json::to_value(hover).expect("serialize");
        assert!(
            value["contents"]["value"]
                .as_str()
                .expect("hover text")
                .contains(expected),
            "expected {expected} at {position:?}: {value}"
        );
    }

    let completion = service
        .completion(&document_uri, lsp::Position::new(1, 28))
        .expect("completion")
        .expect("response");
    let value = serde_json::to_value(completion).expect("serialize");
    assert!(
        value
            .as_array()
            .expect("items")
            .iter()
            .any(|item| item["label"] == "subs")
    );
}

#[test]
fn hover_uses_document_catalogs_for_footnotes_bibliography_and_index() {
    let mut service = LanguageService::default();
    let source = "footnote:n[note] footnote:n[] bibanchor:ref[] indexterm:[Rust,Ownership]";
    open(&mut service, "file:///catalogs.adoc", 1, source);
    let document_uri = uri("file:///catalogs.adoc");
    for (character, expected) in [
        (2, "footnote 1"),
        (23, "footnote 1"),
        (37, "bibliography entry"),
        (55, "Rust > Ownership"),
    ] {
        let hover = service
            .hover(&document_uri, lsp::Position::new(0, character))
            .expect("hover")
            .expect("value");
        let value = serde_json::to_value(hover).expect("serialize");
        assert!(
            value["contents"]["value"]
                .as_str()
                .expect("hover text")
                .contains(expected),
            "expected {expected}: {value}"
        );
    }
}

#[test]
fn bibliography_targets_support_hover_definition_and_references() {
    let mut service = LanguageService::default();
    let source = "= References\n\n[bibliography]\n== Sources\n\n* bibanchor:ref[] Entry\n\nSee <<ref,Entry>> and <<ref>>.\n";
    let document_uri = uri("file:///bibliography.adoc");
    open(&mut service, document_uri.as_str(), 1, source);

    let hover = service
        .hover(&document_uri, lsp::Position::new(5, 9))
        .expect("hover")
        .expect("value");
    assert!(
        serde_json::to_value(hover).expect("serialize")["contents"]["value"]
            .as_str()
            .expect("hover text")
            .contains("bibliography entry")
    );

    let definition = service
        .definition(&document_uri, lsp::Position::new(7, 6))
        .expect("definition")
        .expect("value");
    let definition = serde_json::to_value(definition).expect("serialize");
    assert_eq!(definition["uri"], "file:///bibliography.adoc");
    assert_eq!(definition["range"]["start"]["line"], 5);

    let references = service
        .references(&document_uri, lsp::Position::new(5, 13), true)
        .expect("references")
        .expect("locations");
    assert_eq!(references.len(), 3);
}

fn open_reference_workspace(service: &mut LanguageService) {
    open(
        service,
        "file:///a.adoc",
        1,
        "[[target]]\n== Target\n\nSee <<target>> and xref:b.adoc#other[B].\nhttps://example.com[Site]\n",
    );
    open(
        service,
        "file:///b.adoc",
        1,
        "[[other]]\n== Other\n\nxref:a.adoc#target[A]\n",
    );
}

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

#[test]
fn semantic_tokens_are_sorted_and_delta_encoded_from_the_analysis() {
    let mut service = LanguageService::default();
    open_reference_workspace(&mut service);
    let tokens = service
        .semantic_tokens(&uri("file:///a.adoc"))
        .expect("semantic tokens")
        .expect("tokens");
    let value = serde_json::to_value(tokens).expect("serialize");
    let data = value["data"].as_array().expect("data");

    assert!(!data.is_empty());
    assert_eq!(data.len() % 5, 0);
    assert_eq!(data[0], 0);
}

#[test]
fn semantic_tokens_split_multiline_inline_ranges_at_crlf_boundaries() {
    for (encoding, first_length) in [("utf-8", 5), ("utf-16", 3)] {
        let mut service = LanguageService::default();
        initialize(&mut service, &[encoding]);
        let document_uri = uri("file:///multiline.adoc");
        open(&mut service, document_uri.as_str(), 1, "``a😀\r\nb``");

        let tokens = service
            .semantic_tokens(&document_uri)
            .expect("semantic tokens")
            .expect("tokens");
        let value = serde_json::to_value(tokens).expect("serialize");

        assert_eq!(
            value["data"],
            json!([0, 2, first_length, 0, 0, 1, 0, 1, 0, 0])
        );
    }
}

#[test]
fn semantic_tokens_leave_syntactic_headings_to_editor_grammars() {
    let mut service = LanguageService::default();
    initialize(&mut service, &["utf-8"]);
    let document_uri = uri("file:///heading.adoc");
    open(
        &mut service,
        document_uri.as_str(),
        1,
        "= Document\n\n== Section\n",
    );

    let tokens = service
        .semantic_tokens(&document_uri)
        .expect("semantic tokens")
        .expect("tokens");
    let value = serde_json::to_value(tokens).expect("serialize");

    assert_eq!(value["data"], json!([]));
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

#[test]
fn release_fixture_is_accepted_by_all_existing_features() {
    let source = include_str!("../../../fixtures/release/core.adoc");
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
    let source = include_str!("../../../fixtures/conformance/full.adoc");
    let mut service = LanguageService::default();
    let document_uri = uri("file:///conformance.adoc");
    open(&mut service, document_uri.as_str(), 1, source);

    let symbols = service
        .document_symbols(&document_uri)
        .expect("symbols")
        .expect("response");
    let symbols = serde_json::to_value(symbols).expect("serialize symbols");
    assert_eq!(symbols.as_array().expect("symbol array").len(), 1);
    assert_eq!(symbols[0]["name"], "統合文書");

    let links = service
        .document_links(&document_uri)
        .expect("document links")
        .expect("response");
    assert!(links.iter().any(|link| {
        link.target
            .as_ref()
            .is_some_and(|target| target.as_str() == "https://example.com/doc")
    }));
    assert_eq!(links.len(), 3);

    let tokens = service
        .semantic_tokens(&document_uri)
        .expect("semantic tokens")
        .expect("response");
    let tokens = serde_json::to_value(tokens).expect("serialize tokens");
    assert!(!tokens["data"].as_array().expect("token data").is_empty());
}

#[tokio::test(flavor = "current_thread")]
async fn protocol_async_lsp_transport_runs_typed_lifecycle_and_features() {
    use tokio_util::compat::{TokioAsyncReadCompatExt, TokioAsyncWriteCompatExt};

    let (server_stream, client_stream) = tokio::io::duplex(64 * 1024);
    let (server_read, server_write) = tokio::io::split(server_stream);
    let server = run(server_read.compat(), server_write.compat_write());
    let (client_read, mut client_write) = tokio::io::split(client_stream);
    let mut client_read = BufReader::new(client_read);

    let client = async move {
        write_message(
            &mut client_write,
            &json!({
                "jsonrpc":"2.0",
                "id":1,
                "method":"initialize",
                "params":{"processId":null,"rootUri":null,"capabilities":{}}
            }),
        )
        .await;
        assert_eq!(read_message(&mut client_read).await["id"], 1);
        write_message(
            &mut client_write,
            &json!({"jsonrpc":"2.0","method":"initialized","params":{}}),
        )
        .await;
        write_message(
            &mut client_write,
            &json!({
                "jsonrpc":"2.0",
                "method":"textDocument/didOpen",
                "params":{"textDocument":{
                    "uri":"file:///typed.adoc",
                    "languageId":"asciidoc",
                    "version":1,
                    "text":"[[part]]\n= Typed path\n\n<<part>>\n"
                }}
            }),
        )
        .await;
        assert_eq!(
            read_message(&mut client_read).await["method"],
            "textDocument/publishDiagnostics"
        );
        write_message(
            &mut client_write,
            &json!({
                "jsonrpc":"2.0",
                "id":3,
                "method":"textDocument/documentSymbol",
                "params":{"textDocument":{"uri":"file:///typed.adoc"}}
            }),
        )
        .await;
        assert_eq!(
            read_message(&mut client_read).await["result"][0]["name"],
            "Typed path"
        );
        write_message(
            &mut client_write,
            &json!({
                "jsonrpc":"2.0",
                "id":6,
                "method":"textDocument/definition",
                "params":{
                    "textDocument":{"uri":"file:///typed.adoc"},
                    "position":{"line":3,"character":3}
                }
            }),
        )
        .await;
        assert_eq!(
            read_message(&mut client_read).await["result"]["uri"],
            "file:///typed.adoc"
        );
        write_message(
            &mut client_write,
            &json!({
                "jsonrpc":"2.0",
                "id":7,
                "method":"textDocument/semanticTokens/full",
                "params":{"textDocument":{"uri":"file:///typed.adoc"}}
            }),
        )
        .await;
        assert!(
            read_message(&mut client_read).await["result"]["data"]
                .as_array()
                .is_some_and(|data| !data.is_empty())
        );
        write_message(
            &mut client_write,
            &json!({
                "jsonrpc":"2.0",
                "id":8,
                "method":"textDocument/documentLink",
                "params":{"textDocument":{"uri":"file:///typed.adoc"}}
            }),
        )
        .await;
        assert_eq!(
            read_message(&mut client_read).await["result"][0]["target"],
            "file:///typed.adoc#part"
        );
        write_message(
            &mut client_write,
            &json!({
                "jsonrpc":"2.0",
                "id":9,
                "method":"textDocument/references",
                "params":{
                    "textDocument":{"uri":"file:///typed.adoc"},
                    "position":{"line":0,"character":3},
                    "context":{"includeDeclaration":false}
                }
            }),
        )
        .await;
        assert_eq!(
            read_message(&mut client_read).await["result"]
                .as_array()
                .map(Vec::len),
            Some(1)
        );
        write_message(
            &mut client_write,
            &json!({"jsonrpc":"2.0","id":2,"method":"shutdown","params":null}),
        )
        .await;
        assert_eq!(read_message(&mut client_read).await["id"], 2);
        write_message(
            &mut client_write,
            &json!({"jsonrpc":"2.0","method":"exit","params":null}),
        )
        .await;
    };

    let (server_result, ()) = tokio::join!(server, client);
    server_result.expect("clean exit");
}

#[tokio::test(flavor = "current_thread")]
async fn protocol_async_lsp_lifecycle_rejects_requests_in_invalid_states() {
    use tokio_util::compat::{TokioAsyncReadCompatExt, TokioAsyncWriteCompatExt};

    let (server_stream, client_stream) = tokio::io::duplex(64 * 1024);
    let (server_read, server_write) = tokio::io::split(server_stream);
    let server = run(server_read.compat(), server_write.compat_write());
    let (client_read, mut client_write) = tokio::io::split(client_stream);
    let mut client_read = BufReader::new(client_read);

    let client = async move {
        write_message(
            &mut client_write,
            &json!({
                "jsonrpc":"2.0",
                "id":1,
                "method":"textDocument/documentSymbol",
                "params":{"textDocument":{"uri":"file:///lifecycle.adoc"}}
            }),
        )
        .await;
        assert_eq!(
            read_message(&mut client_read).await["error"]["code"],
            -32002
        );

        let initialize = json!({
            "jsonrpc":"2.0",
            "id":2,
            "method":"initialize",
            "params":{"processId":null,"rootUri":null,"capabilities":{}}
        });
        write_message(&mut client_write, &initialize).await;
        assert_eq!(read_message(&mut client_read).await["id"], 2);

        let mut duplicate = initialize;
        duplicate["id"] = json!(3);
        write_message(
            &mut client_write,
            &json!({"jsonrpc":"2.0","method":"initialized","params":{}}),
        )
        .await;
        write_message(&mut client_write, &duplicate).await;
        assert_eq!(
            read_message(&mut client_read).await["error"]["code"],
            -32600
        );

        write_message(
            &mut client_write,
            &json!({"jsonrpc":"2.0","id":4,"method":"shutdown","params":null}),
        )
        .await;
        assert_eq!(read_message(&mut client_read).await["id"], 4);

        write_message(
            &mut client_write,
            &json!({
                "jsonrpc":"2.0",
                "id":5,
                "method":"textDocument/documentSymbol",
                "params":{"textDocument":{"uri":"file:///lifecycle.adoc"}}
            }),
        )
        .await;
        assert_eq!(
            read_message(&mut client_read).await["error"]["code"],
            -32600
        );
        write_message(
            &mut client_write,
            &json!({"jsonrpc":"2.0","method":"exit","params":null}),
        )
        .await;
    };

    let (server_result, ()) = tokio::join!(server, client);
    server_result.expect("clean exit");
}
