//! Typed Language Server service and transport tests.

use async_lsp::lsp_types as lsp;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};

use super::{LanguageService, PositionEncoding, run};
use crate::state::{Adoption, AnalysisJob};

fn typed<T: DeserializeOwned>(value: Value) -> T {
    serde_json::from_value(value).expect("valid LSP value")
}

fn uri(value: &str) -> lsp::Url {
    value.parse().expect("valid URI")
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
    let job = service.begin_open(typed(json!({
        "textDocument": {
            "uri": uri,
            "languageId": "asciidoc",
            "version": version,
            "text": text
        }
    })));
    adopt(service, job);
}

fn change(
    service: &mut LanguageService,
    uri: &str,
    version: i32,
    changes: Value,
) -> Result<bool, String> {
    let job = service.begin_change_full(typed(json!({
        "textDocument": {"uri": uri, "version": version},
        "contentChanges": changes
    })))?;
    let Some(job) = job else {
        return Ok(false);
    };
    adopt(service, job);
    Ok(true)
}

fn adopt(service: &mut LanguageService, job: AnalysisJob) {
    use adocweave::{Engine, ParseOptions, SourceId};

    let analysis = Engine::new(ParseOptions {
        source_id: Some(SourceId::new(job.uri.clone())),
        ..ParseOptions::default()
    })
    .analyze_cancellable(&job.source, job.cancellation.as_ref())
    .expect("analysis");
    assert_eq!(service.adopt(&job, analysis), Adoption::Adopted);
}

#[test]
fn initialize_negotiates_encoding_and_advertises_existing_features() {
    let mut service = LanguageService::default();
    let result = initialize(&mut service, &["utf-8", "utf-16"]);
    let value = serde_json::to_value(result).expect("serialize");

    assert_eq!(service.position_encoding, PositionEncoding::Utf8);
    assert_eq!(value["capabilities"]["positionEncoding"], "utf-8");
    assert_eq!(value["capabilities"]["textDocumentSync"]["change"], 1);
    assert_eq!(value["capabilities"]["documentSymbolProvider"], true);
    assert_eq!(value["serverInfo"]["name"], "adocweave-lsp");
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
            .analysis
            .as_ref()
            .expect("analysis")
            .source(),
        "= A"
    );
    assert_eq!(
        service
            .documents
            .get("file:///b.adoc")
            .expect("b")
            .analysis
            .as_ref()
            .expect("analysis")
            .source(),
        "= B"
    );
}

#[test]
fn incremental_changes_are_rejected_without_changing_the_snapshot() {
    let mut service = LanguageService::default();
    open(&mut service, "file:///a.adoc", 1, "one");
    let result = change(
        &mut service,
        "file:///a.adoc",
        2,
        json!([{
            "range": {
                "start": {"line": 0, "character": 0},
                "end": {"line": 0, "character": 3}
            },
            "text": "two"
        }]),
    );
    assert!(result.is_err());
    assert_eq!(
        service
            .documents
            .get("file:///a.adoc")
            .expect("document")
            .analysis
            .as_ref()
            .expect("analysis")
            .source(),
        "one"
    );
}

#[test]
fn diagnostics_use_current_version_codes_and_unicode_positions() {
    let text = "日😀e\u{301} ";
    for (encoding, expected_start, expected_end) in [
        (PositionEncoding::Utf8, 10, 11),
        (PositionEncoding::Utf16, 5, 6),
    ] {
        let mut service = LanguageService {
            position_encoding: encoding,
            ..LanguageService::default()
        };
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
fn close_clears_diagnostics() {
    let mut service = LanguageService::default();
    let document_uri = uri("file:///a.adoc");
    open(&mut service, document_uri.as_str(), 1, "bad ");
    assert!(service.close(&document_uri));
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
    use adocweave::source::{LineIndex, Position, PositionEncoding as CorePositionEncoding};

    let index = LineIndex::new(source).expect("line index");
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

#[tokio::test(flavor = "current_thread")]
async fn async_lsp_transport_runs_typed_lifecycle_and_features() {
    use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
    use tokio_util::compat::{TokioAsyncReadCompatExt, TokioAsyncWriteCompatExt};

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
                    "text":"= Typed path\n"
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
