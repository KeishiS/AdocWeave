//! LSP transport and document state, isolated from the parsing core.

use std::collections::BTreeMap;
use std::io::{self, BufRead, Write};

use asciiloom::document::{DocumentSymbol, SymbolKind, document_symbols};
use asciiloom::parser::{AstDocument, parse};
use asciiloom::source::{LineIndex, PositionEncoding as CorePositionEncoding, TextRange};
use asciiloom::{diagnostic::Severity, lint};
use serde_json::{Value, json};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PositionEncoding {
    Utf8,
    Utf16,
}

impl PositionEncoding {
    pub const fn as_lsp_name(self) -> &'static str {
        match self {
            Self::Utf8 => "utf-8",
            Self::Utf16 => "utf-16",
        }
    }
}

#[derive(Debug)]
pub struct DocumentState {
    pub uri: String,
    pub version: i64,
    pub text: String,
    pub line_starts: Vec<usize>,
    pub ast: AstDocument,
}

impl DocumentState {
    fn new(uri: String, version: i64, text: String) -> Result<Self, String> {
        let ast = parse(&text).map_err(|error| error.to_string())?.ast;
        let mut line_starts = vec![0];
        line_starts.extend(
            text.bytes()
                .enumerate()
                .filter_map(|(index, byte)| (byte == b'\n').then_some(index + 1)),
        );
        Ok(Self {
            uri,
            version,
            text,
            line_starts,
            ast,
        })
    }
}

#[derive(Debug, Default)]
pub struct DocumentStore {
    documents: BTreeMap<String, DocumentState>,
}

impl DocumentStore {
    pub fn get(&self, uri: &str) -> Option<&DocumentState> {
        self.documents.get(uri)
    }

    pub fn open(&mut self, uri: String, version: i64, text: String) -> Result<(), String> {
        let state = DocumentState::new(uri.clone(), version, text)?;
        self.documents.insert(uri, state);
        Ok(())
    }

    pub fn change_full(&mut self, uri: &str, version: i64, text: String) -> Result<bool, String> {
        let Some(current) = self.documents.get(uri) else {
            return Ok(false);
        };
        if version <= current.version {
            return Ok(false);
        }
        let state = DocumentState::new(uri.to_owned(), version, text)?;
        self.documents.insert(uri.to_owned(), state);
        Ok(true)
    }

    pub fn close(&mut self, uri: &str) -> bool {
        self.documents.remove(uri).is_some()
    }

    pub fn len(&self) -> usize {
        self.documents.len()
    }

    pub fn is_empty(&self) -> bool {
        self.documents.is_empty()
    }
}

#[derive(Debug)]
pub struct Server {
    pub documents: DocumentStore,
    pub position_encoding: PositionEncoding,
    shutdown_requested: bool,
    outgoing: Vec<Value>,
}

impl Default for Server {
    fn default() -> Self {
        Self {
            documents: DocumentStore::default(),
            position_encoding: PositionEncoding::Utf16,
            shutdown_requested: false,
            outgoing: Vec::new(),
        }
    }
}

impl Server {
    pub fn handle(&mut self, message: &Value) -> Result<Option<Value>, String> {
        let method = message
            .get("method")
            .and_then(Value::as_str)
            .ok_or_else(|| "message method is missing".to_owned())?;
        let id = message.get("id").cloned();
        let params = message.get("params").cloned().unwrap_or(Value::Null);

        match method {
            "initialize" => {
                self.position_encoding = negotiate_encoding(&params);
                Ok(id.map(|id| {
                    json!({
                        "jsonrpc": "2.0",
                        "id": id,
                        "result": {
                            "capabilities": {
                                "positionEncoding": self.position_encoding.as_lsp_name(),
                                "textDocumentSync": {
                                    "openClose": true,
                                    "change": 1,
                                    "save": {"includeText": true}
                                },
                                "documentSymbolProvider": true
                            },
                            "serverInfo": {"name": "asciiloom-lsp", "version": env!("CARGO_PKG_VERSION")}
                        }
                    })
                }))
            }
            "initialized" => Ok(None),
            "textDocument/didSave" => {
                let uri = string_field(&params["textDocument"], "uri")?;
                self.publish_diagnostics(&uri)?;
                Ok(None)
            }
            "shutdown" => {
                self.shutdown_requested = true;
                Ok(id.map(|id| json!({"jsonrpc": "2.0", "id": id, "result": null})))
            }
            "exit" => Ok(None),
            "textDocument/didOpen" => {
                let document = &params["textDocument"];
                self.documents.open(
                    string_field(document, "uri")?,
                    integer_field(document, "version")?,
                    string_field(document, "text")?,
                )?;
                self.publish_diagnostics(
                    params["textDocument"]["uri"]
                        .as_str()
                        .expect("validated URI"),
                )?;
                Ok(None)
            }
            "textDocument/didChange" => {
                let document = &params["textDocument"];
                let changes = params["contentChanges"]
                    .as_array()
                    .ok_or_else(|| "contentChanges must be an array".to_owned())?;
                if changes.iter().any(|change| change.get("range").is_some()) {
                    return Err("incremental changes are not accepted; send full text".to_owned());
                }
                let text = changes
                    .last()
                    .and_then(|change| change.get("text"))
                    .and_then(Value::as_str)
                    .ok_or_else(|| "full change text is missing".to_owned())?;
                let uri = string_field(document, "uri")?;
                let changed = self.documents.change_full(
                    &uri,
                    integer_field(document, "version")?,
                    text.to_owned(),
                )?;
                if changed {
                    self.publish_diagnostics(&uri)?;
                }
                Ok(None)
            }
            "textDocument/didClose" => {
                let uri = string_field(&params["textDocument"], "uri")?;
                self.documents.close(&uri);
                self.outgoing.push(json!({
                    "jsonrpc": "2.0",
                    "method": "textDocument/publishDiagnostics",
                    "params": {"uri": uri, "diagnostics": []}
                }));
                Ok(None)
            }
            "textDocument/documentSymbol" => {
                let uri = string_field(&params["textDocument"], "uri")?;
                let result = self
                    .documents
                    .get(&uri)
                    .map(|document| {
                        let line_index =
                            LineIndex::new(&document.text).map_err(|error| error.to_string())?;
                        document_symbols(&document.ast)
                            .iter()
                            .map(|symbol| {
                                symbol_to_lsp(symbol, &line_index, self.position_encoding)
                            })
                            .collect::<Result<Vec<_>, String>>()
                    })
                    .transpose()?
                    .unwrap_or_default();
                Ok(id.map(|id| json!({"jsonrpc": "2.0", "id": id, "result": result})))
            }
            _ => Ok(id.map(|id| {
                json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "error": {"code": -32601, "message": "method not found"}
                })
            })),
        }
    }

    pub const fn should_exit(&self) -> bool {
        self.shutdown_requested
    }

    pub fn drain_outgoing(&mut self) -> impl Iterator<Item = Value> + '_ {
        self.outgoing.drain(..)
    }

    fn publish_diagnostics(&mut self, uri: &str) -> Result<(), String> {
        let Some(document) = self.documents.get(uri) else {
            return Ok(());
        };
        let diagnostics = lint::lint(&document.text, &lint::LintConfig::default())
            .map_err(|error| error.to_string())?;
        let line_index = LineIndex::new(&document.text).map_err(|error| error.to_string())?;
        let encoding = match self.position_encoding {
            PositionEncoding::Utf8 => CorePositionEncoding::Utf8,
            PositionEncoding::Utf16 => CorePositionEncoding::Utf16,
        };
        let diagnostics = diagnostics
            .iter()
            .map(|diagnostic| {
                let start = line_index
                    .offset_to_position(diagnostic.range.start(), encoding)
                    .map_err(|error| error.to_string())?;
                let end = line_index
                    .offset_to_position(diagnostic.range.end(), encoding)
                    .map_err(|error| error.to_string())?;
                let severity = match diagnostic.severity {
                    Severity::Error => 1,
                    Severity::Warning => 2,
                    Severity::Information => 3,
                    Severity::Hint => 4,
                };
                Ok(json!({
                    "range": {
                        "start": {"line": start.line, "character": start.character},
                        "end": {"line": end.line, "character": end.character}
                    },
                    "severity": severity,
                    "code": diagnostic.code.as_str(),
                    "source": "asciiloom",
                    "message": diagnostic.message
                }))
            })
            .collect::<Result<Vec<_>, String>>()?;
        self.outgoing.push(json!({
            "jsonrpc": "2.0",
            "method": "textDocument/publishDiagnostics",
            "params": {
                "uri": uri,
                "version": document.version,
                "diagnostics": diagnostics
            }
        }));
        Ok(())
    }
}

fn symbol_to_lsp(
    symbol: &DocumentSymbol,
    line_index: &LineIndex<'_>,
    encoding: PositionEncoding,
) -> Result<Value, String> {
    let kind = match symbol.kind {
        SymbolKind::DocumentTitle => 1,
        SymbolKind::Section => 3,
    };
    let children = symbol
        .children
        .iter()
        .map(|child| symbol_to_lsp(child, line_index, encoding))
        .collect::<Result<Vec<_>, _>>()?;
    Ok(json!({
        "name": symbol.name,
        "kind": kind,
        "range": range_to_lsp(symbol.range, line_index, encoding)?,
        "selectionRange": range_to_lsp(symbol.selection_range, line_index, encoding)?,
        "children": children
    }))
}

fn range_to_lsp(
    range: TextRange,
    line_index: &LineIndex<'_>,
    encoding: PositionEncoding,
) -> Result<Value, String> {
    let encoding = match encoding {
        PositionEncoding::Utf8 => CorePositionEncoding::Utf8,
        PositionEncoding::Utf16 => CorePositionEncoding::Utf16,
    };
    let start = line_index
        .offset_to_position(range.start(), encoding)
        .map_err(|error| error.to_string())?;
    let end = line_index
        .offset_to_position(range.end(), encoding)
        .map_err(|error| error.to_string())?;
    Ok(json!({
        "start": {"line": start.line, "character": start.character},
        "end": {"line": end.line, "character": end.character}
    }))
}

fn negotiate_encoding(params: &Value) -> PositionEncoding {
    let encodings = params
        .pointer("/capabilities/general/positionEncodings")
        .and_then(Value::as_array);
    if encodings.is_some_and(|values| values.iter().any(|value| value == "utf-8")) {
        PositionEncoding::Utf8
    } else {
        PositionEncoding::Utf16
    }
}

fn string_field(value: &Value, field: &str) -> Result<String, String> {
    value
        .get(field)
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| format!("{field} must be a string"))
}

fn integer_field(value: &Value, field: &str) -> Result<i64, String> {
    value
        .get(field)
        .and_then(Value::as_i64)
        .ok_or_else(|| format!("{field} must be an integer"))
}

pub fn run_stdio() -> Result<(), String> {
    let stdin = io::stdin();
    let stdout = io::stdout();
    run(stdin.lock(), stdout.lock())
}

pub fn run<R: BufRead, W: Write>(mut input: R, mut output: W) -> Result<(), String> {
    let mut server = Server::default();
    while let Some(message) = read_message(&mut input)? {
        let exit = message.get("method").and_then(Value::as_str) == Some("exit");
        if let Some(response) = server.handle(&message)? {
            write_message(&mut output, &response)?;
        }
        for notification in server.drain_outgoing() {
            write_message(&mut output, &notification)?;
        }
        if exit {
            return if server.should_exit() {
                Ok(())
            } else {
                Err("exit received before shutdown".to_owned())
            };
        }
    }
    Ok(())
}

fn read_message<R: BufRead>(input: &mut R) -> Result<Option<Value>, String> {
    let mut content_length = None;
    loop {
        let mut header = String::new();
        if input
            .read_line(&mut header)
            .map_err(|error| error.to_string())?
            == 0
        {
            return Ok(None);
        }
        if header == "\r\n" || header == "\n" {
            break;
        }
        if let Some(value) = header.strip_prefix("Content-Length:") {
            content_length = Some(
                value
                    .trim()
                    .parse::<usize>()
                    .map_err(|error| error.to_string())?,
            );
        }
    }
    let length = content_length.ok_or_else(|| "Content-Length is missing".to_owned())?;
    let mut body = vec![0; length];
    input
        .read_exact(&mut body)
        .map_err(|error| error.to_string())?;
    serde_json::from_slice(&body)
        .map(Some)
        .map_err(|error| error.to_string())
}

fn write_message<W: Write>(output: &mut W, message: &Value) -> Result<(), String> {
    let body = serde_json::to_vec(message).map_err(|error| error.to_string())?;
    write!(output, "Content-Length: {}\r\n\r\n", body.len()).map_err(|error| error.to_string())?;
    output.write_all(&body).map_err(|error| error.to_string())?;
    output.flush().map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
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
                .text,
            "= A"
        );
        assert_eq!(
            server
                .documents
                .get("file:///b.adoc")
                .expect("document")
                .text,
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
                .text,
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
}
