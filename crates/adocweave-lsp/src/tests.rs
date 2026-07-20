//! AdocWeave Language Server protocol tests.

use super::{PositionEncoding, Server, run};
use serde_json::json;
use std::io::Cursor;

fn notify(method: &str, params: serde_json::Value) -> serde_json::Value {
    json!({"jsonrpc": "2.0", "method": method, "params": params})
}

#[test]
fn document_sync_keeps_documents_independent_and_ignores_stale_versions() {
    let mut server = Server::default();
    for (uri, text) in [("file:///a.adoc", "= A"), ("file:///b.adoc", "= B")] {
        server
            .handle(&notify(
                "textDocument/didOpen",
                json!({"textDocument": {"uri": uri, "version": 2, "text": text}}),
            ))
            .expect("open succeeds");
    }
    assert_eq!(server.documents.len(), 2);

    server
        .handle(&notify(
            "textDocument/didChange",
            json!({
                "textDocument": {"uri": "file:///a.adoc", "version": 1},
                "contentChanges": [{"text": "stale"}]
            }),
        ))
        .expect("stale change is ignored");
    assert_eq!(
        server
            .documents
            .get("file:///a.adoc")
            .expect("document")
            .analysis
            .source(),
        "= A"
    );
    assert_eq!(
        server
            .documents
            .get("file:///b.adoc")
            .expect("document")
            .analysis
            .source(),
        "= B"
    );

    server
        .handle(&notify(
            "textDocument/didClose",
            json!({"textDocument": {"uri": "file:///a.adoc"}}),
        ))
        .expect("close succeeds");
    assert!(server.documents.get("file:///a.adoc").is_none());
    assert_eq!(server.documents.len(), 1);
}

#[test]
fn document_sync_negotiates_encoding_and_advertises_full_sync() {
    let mut server = Server::default();
    let response = server
        .handle(&json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "capabilities": {
                    "general": {"positionEncodings": ["utf-8", "utf-16"]}
                }
            }
        }))
        .expect("initialize succeeds")
        .expect("response");

    assert_eq!(server.position_encoding, PositionEncoding::Utf8);
    assert_eq!(
        response["result"]["capabilities"]["positionEncoding"],
        "utf-8"
    );
    assert_eq!(
        response["result"]["capabilities"]["textDocumentSync"]["change"],
        1
    );
}

#[test]
fn document_sync_rejects_incremental_changes_explicitly() {
    let mut server = Server::default();
    server
        .handle(&notify(
            "textDocument/didOpen",
            json!({"textDocument": {
                "uri": "file:///a.adoc", "version": 1, "text": "one"
            }}),
        ))
        .expect("open");
    let result = server.handle(&notify(
        "textDocument/didChange",
        json!({
            "textDocument": {"uri": "file:///a.adoc", "version": 2},
            "contentChanges": [{
                "range": {
                    "start": {"line": 0, "character": 0},
                    "end": {"line": 0, "character": 3}
                },
                "text": "two"
            }]
        }),
    ));

    assert!(result.is_err());
    assert_eq!(
        server
            .documents
            .get("file:///a.adoc")
            .expect("document")
            .analysis
            .source(),
        "one"
    );
}

#[test]
fn document_sync_stdio_runs_initialize_shutdown_exit() {
    let messages = [
        json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}),
        json!({"jsonrpc":"2.0","method":"initialized","params":{}}),
        json!({"jsonrpc":"2.0","id":2,"method":"shutdown","params":null}),
        json!({"jsonrpc":"2.0","method":"exit","params":null}),
    ];
    let mut input = Vec::new();
    for message in messages {
        let body = serde_json::to_vec(&message).expect("serialize");
        input.extend_from_slice(format!("Content-Length: {}\r\n\r\n", body.len()).as_bytes());
        input.extend_from_slice(&body);
    }
    let mut output = Vec::new();

    run(Cursor::new(input), &mut output).expect("server exits cleanly");
    let output = String::from_utf8(output).expect("utf-8 protocol");
    assert!(output.contains("\"id\":1"));
    assert!(output.contains("\"id\":2"));
}

fn open_with_diagnostic(server: &mut Server, uri: &str, text: &str) -> serde_json::Value {
    server
        .handle(&notify(
            "textDocument/didOpen",
            json!({"textDocument": {
                "uri": uri, "version": 1, "text": text
            }}),
        ))
        .expect("open succeeds");
    server.drain_outgoing().next().expect("diagnostics")
}

#[test]
fn diagnostics_preserve_code_severity_version_and_latest_change() {
    let mut server = Server::default();
    let first = open_with_diagnostic(&mut server, "file:///a.adoc", "one ");
    assert_eq!(first["params"]["version"], 1);
    assert_eq!(
        first["params"]["diagnostics"][0]["code"],
        "trailing-whitespace"
    );
    assert_eq!(first["params"]["diagnostics"][0]["severity"], 2);

    server
        .handle(&notify(
            "textDocument/didChange",
            json!({
                "textDocument": {"uri": "file:///a.adoc", "version": 2},
                "contentChanges": [{"text": "_unfinished"}]
            }),
        ))
        .expect("incomplete input is accepted");
    let latest = server.drain_outgoing().next().expect("latest diagnostics");
    assert_eq!(latest["params"]["version"], 2);
    assert_eq!(
        latest["params"]["diagnostics"][0]["code"],
        "unclosed-inline"
    );

    server
        .handle(&notify(
            "textDocument/didChange",
            json!({
                "textDocument": {"uri": "file:///a.adoc", "version": 1},
                "contentChanges": [{"text": "stale "}]
            }),
        ))
        .expect("stale input is ignored");
    assert!(server.drain_outgoing().next().is_none());
}

#[test]
fn unicode_positions_follow_negotiated_utf8_and_utf16() {
    let text = "日😀e\u{301} ";
    for (encoding, expected_start, expected_end) in [
        (PositionEncoding::Utf8, 10, 11),
        (PositionEncoding::Utf16, 5, 6),
    ] {
        let mut server = Server {
            position_encoding: encoding,
            ..Server::default()
        };
        let notification = open_with_diagnostic(&mut server, "file:///unicode.adoc", text);
        let range = &notification["params"]["diagnostics"][0]["range"];
        assert_eq!(range["start"]["character"], expected_start);
        assert_eq!(range["end"]["character"], expected_end);
    }
}

#[test]
fn diagnostics_are_cleared_when_document_closes() {
    let mut server = Server::default();
    open_with_diagnostic(&mut server, "file:///a.adoc", "bad ");
    server
        .handle(&notify(
            "textDocument/didClose",
            json!({"textDocument": {"uri": "file:///a.adoc"}}),
        ))
        .expect("close succeeds");

    let notification = server.drain_outgoing().next().expect("clear notification");
    assert_eq!(notification["params"]["uri"], "file:///a.adoc");
    assert_eq!(notification["params"]["diagnostics"], json!([]));
}

fn request_symbols(server: &mut Server, uri: &str) -> serde_json::Value {
    server
        .handle(&json!({
            "jsonrpc": "2.0",
            "id": 7,
            "method": "textDocument/documentSymbol",
            "params": {"textDocument": {"uri": uri}}
        }))
        .expect("symbol request succeeds")
        .expect("symbol response")["result"]
        .clone()
}

#[test]
fn document_symbols_match_core_hierarchy_and_ranges() {
    let mut server = Server::default();
    open_with_diagnostic(
        &mut server,
        "file:///symbols.adoc",
        "= 題名😀\n\n== 一\n\n=== 子\n\n== 二\n",
    );
    let symbols = request_symbols(&mut server, "file:///symbols.adoc");

    assert_eq!(symbols[0]["name"], "題名😀");
    assert_eq!(symbols[0]["children"][0]["name"], "一");
    assert_eq!(symbols[0]["children"][0]["children"][0]["name"], "子");
    assert_eq!(symbols[0]["children"][1]["name"], "二");
    assert_eq!(symbols[0]["selectionRange"]["end"]["character"], 6);
    assert_ne!(symbols[0]["range"], symbols[0]["selectionRange"]);
}

#[test]
fn document_symbols_return_empty_for_empty_or_unknown_document() {
    let mut server = Server::default();
    open_with_diagnostic(&mut server, "file:///empty.adoc", "");

    assert_eq!(
        request_symbols(&mut server, "file:///empty.adoc"),
        json!([])
    );
    assert_eq!(
        request_symbols(&mut server, "file:///missing.adoc"),
        json!([])
    );
}

fn request(server: &mut Server, method: &str, uri: &str) -> serde_json::Value {
    server
        .handle(&json!({
            "jsonrpc": "2.0",
            "id": 9,
            "method": method,
            "params": {
                "textDocument": {"uri": uri},
                "options": {"tabSize": 4, "insertSpaces": true},
                "context": {"diagnostics": []}
            }
        }))
        .expect("request succeeds")
        .expect("response")["result"]
        .clone()
}

fn apply_lsp_edits(source: &str, edits: &[serde_json::Value]) -> String {
    use adocweave::source::{LineIndex, Position, PositionEncoding as CorePositionEncoding};

    let index = LineIndex::new(source).expect("line index");
    let mut byte_edits = edits
        .iter()
        .map(|edit| {
            let range = &edit["range"];
            let position = |value: &serde_json::Value| Position {
                line: value["line"].as_u64().expect("line") as u32,
                character: value["character"].as_u64().expect("character") as u32,
            };
            let start = index
                .position_to_offset(position(&range["start"]), CorePositionEncoding::Utf16)
                .expect("start")
                .to_usize();
            let end = index
                .position_to_offset(position(&range["end"]), CorePositionEncoding::Utf16)
                .expect("end")
                .to_usize();
            (
                start,
                end,
                edit["newText"].as_str().expect("newText").to_owned(),
            )
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
fn code_action_exposes_safe_fixes_with_current_document_version() {
    let mut server = Server::default();
    open_with_diagnostic(&mut server, "file:///fix.adoc", "==Title\ntext  \n");
    let actions = request(&mut server, "textDocument/codeAction", "file:///fix.adoc");

    assert_eq!(actions.as_array().expect("actions").len(), 2);
    assert!(
        actions
            .as_array()
            .expect("actions")
            .iter()
            .all(|action| { action["edit"]["documentChanges"][0]["textDocument"]["version"] == 1 })
    );
    let titles = actions
        .as_array()
        .expect("actions")
        .iter()
        .map(|action| action["title"].as_str().expect("title"))
        .collect::<Vec<_>>();
    assert!(titles.contains(&"insert a space after heading marker"));
    assert!(titles.contains(&"remove trailing whitespace"));
}

#[test]
fn formatting_is_idempotent_and_preserves_literal_body() {
    let source = "before  \n\n....\ncode  \n....\n\nafter  ";
    let mut server = Server::default();
    open_with_diagnostic(&mut server, "file:///format.adoc", source);
    let edits = request(
        &mut server,
        "textDocument/formatting",
        "file:///format.adoc",
    );
    let edits = edits.as_array().expect("edits");
    assert!(edits.iter().all(|edit| edit["range"]["start"]["line"] != 3));
    let formatted = apply_lsp_edits(source, edits);
    assert!(formatted.contains("....\ncode  \n....\n"));

    server
        .handle(&notify(
            "textDocument/didChange",
            json!({
                "textDocument": {"uri": "file:///format.adoc", "version": 2},
                "contentChanges": [{"text": formatted}]
            }),
        ))
        .expect("change succeeds");
    server.drain_outgoing().for_each(drop);
    let second = request(
        &mut server,
        "textDocument/formatting",
        "file:///format.adoc",
    );
    assert_eq!(second, json!([]));
}

#[test]
fn release_fixture_is_accepted_by_lsp_features() {
    let source = include_str!("../../../fixtures/release/core.adoc");
    let mut server = Server::default();
    let diagnostics = open_with_diagnostic(&mut server, "file:///release.adoc", source);
    assert_eq!(diagnostics["params"]["diagnostics"], json!([]));

    let symbols = request_symbols(&mut server, "file:///release.adoc");
    assert_eq!(symbols[0]["name"], "AdocWeave 初期リリース");
    assert_eq!(
        symbols[0]["children"].as_array().expect("children").len(),
        3
    );

    let formatting = request(
        &mut server,
        "textDocument/formatting",
        "file:///release.adoc",
    );
    assert_eq!(formatting, json!([]));
}

fn position_request(
    server: &mut Server,
    method: &str,
    uri: &str,
    line: u32,
    character: u32,
) -> serde_json::Value {
    server
        .handle(&json!({
            "jsonrpc": "2.0",
            "id": 11,
            "method": method,
            "params": {
                "textDocument": {"uri": uri},
                "position": {"line": line, "character": character}
            }
        }))
        .expect("position request succeeds")
        .expect("response")["result"]
        .clone()
}

#[test]
fn hover_distinguishes_heading_marker_text_and_unrelated_positions() {
    let mut server = Server::default();
    open_with_diagnostic(&mut server, "file:///hover.adoc", "= 題名😀\n\nparagraph\n");

    let marker = position_request(
        &mut server,
        "textDocument/hover",
        "file:///hover.adoc",
        0,
        0,
    );
    let text = position_request(
        &mut server,
        "textDocument/hover",
        "file:///hover.adoc",
        0,
        4,
    );
    let outside = position_request(
        &mut server,
        "textDocument/hover",
        "file:///hover.adoc",
        2,
        2,
    );

    assert!(
        marker["contents"]["value"]
            .as_str()
            .expect("hover")
            .contains("Part: marker")
    );
    assert!(
        text["contents"]["value"]
            .as_str()
            .expect("hover")
            .contains("Generated ID")
    );
    assert_eq!(outside, serde_json::Value::Null);
}

#[test]
fn completion_filters_source_languages_and_ignores_code_or_paragraphs() {
    let mut server = Server::default();
    open_with_diagnostic(
        &mut server,
        "file:///completion.adoc",
        "[source, ru]\n----\ncode\n----\n\nparagraph\n",
    );

    let language = position_request(
        &mut server,
        "textDocument/completion",
        "file:///completion.adoc",
        0,
        11,
    );
    let code = position_request(
        &mut server,
        "textDocument/completion",
        "file:///completion.adoc",
        2,
        2,
    );
    let paragraph = position_request(
        &mut server,
        "textDocument/completion",
        "file:///completion.adoc",
        5,
        3,
    );

    assert_eq!(language, json!([{"label": "rust", "kind": 12}]));
    assert_eq!(code, json!([]));
    assert_eq!(paragraph, json!([]));
}
