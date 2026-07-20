//! LSP transport and document state, isolated from the parsing core.

use asciiloom::document::{
    DocumentElement, DocumentSymbol, SymbolKind, document_element_at, document_symbols,
    generate_heading_ids, source_language_candidates,
};
use asciiloom::source::{LineIndex, PositionEncoding as CorePositionEncoding, TextRange};
use asciiloom::{diagnostic::Severity, formatter, lint};
use serde_json::{Value, json};

mod state;
mod transport;

pub use state::{DocumentState, DocumentStore};
pub use transport::{run, run_stdio};

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
                                "documentSymbolProvider": true,
                                "codeActionProvider": true,
                                "documentFormattingProvider": true
                                ,"hoverProvider": true,
                                "completionProvider": {
                                    "triggerCharacters": [",", " "]
                                }
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
            "textDocument/codeAction" => {
                let uri = string_field(&params["textDocument"], "uri")?;
                let actions = self
                    .documents
                    .get(&uri)
                    .map(|document| code_actions(document, self.position_encoding))
                    .transpose()?
                    .unwrap_or_default();
                Ok(id.map(|id| json!({"jsonrpc": "2.0", "id": id, "result": actions})))
            }
            "textDocument/formatting" => {
                let uri = string_field(&params["textDocument"], "uri")?;
                let edits = self
                    .documents
                    .get(&uri)
                    .map(|document| formatting_edits(document, self.position_encoding))
                    .transpose()?
                    .unwrap_or_default();
                Ok(id.map(|id| json!({"jsonrpc": "2.0", "id": id, "result": edits})))
            }
            "textDocument/hover" => {
                let uri = string_field(&params["textDocument"], "uri")?;
                let result = self
                    .documents
                    .get(&uri)
                    .map(|document| {
                        let offset =
                            request_offset(document, &params["position"], self.position_encoding)?;
                        hover(document, offset, self.position_encoding)
                    })
                    .transpose()?
                    .flatten();
                Ok(id.map(|id| json!({"jsonrpc": "2.0", "id": id, "result": result})))
            }
            "textDocument/completion" => {
                let uri = string_field(&params["textDocument"], "uri")?;
                let result = self
                    .documents
                    .get(&uri)
                    .map(|document| {
                        let offset =
                            request_offset(document, &params["position"], self.position_encoding)?;
                        completion(document, offset)
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

fn request_offset(
    document: &DocumentState,
    position: &Value,
    encoding: PositionEncoding,
) -> Result<u32, String> {
    let line = position["line"]
        .as_u64()
        .ok_or_else(|| "position.line must be an integer".to_owned())? as u32;
    let character = position["character"]
        .as_u64()
        .ok_or_else(|| "position.character must be an integer".to_owned())?
        as u32;
    let encoding = match encoding {
        PositionEncoding::Utf8 => CorePositionEncoding::Utf8,
        PositionEncoding::Utf16 => CorePositionEncoding::Utf16,
    };
    LineIndex::new(&document.text)
        .map_err(|error| error.to_string())?
        .position_to_offset(asciiloom::source::Position { line, character }, encoding)
        .map(|offset| offset.to_u32())
        .map_err(|error| error.to_string())
}

fn hover(
    document: &DocumentState,
    offset: u32,
    encoding: PositionEncoding,
) -> Result<Option<Value>, String> {
    let Some(element) = document_element_at(&document.ast, offset) else {
        return Ok(None);
    };
    let (heading, range, part) = match element {
        DocumentElement::HeadingMarker(heading) => (heading, heading.marker_range, "marker"),
        DocumentElement::HeadingText(heading) => (heading, heading.text_range, "text"),
        DocumentElement::SourceLanguage(_) | DocumentElement::SourceAttribute(_) => {
            return Ok(None);
        }
    };
    let id = generate_heading_ids(&document.ast)
        .into_iter()
        .find(|candidate| candidate.range == heading.text_range)
        .map(|candidate| candidate.id)
        .unwrap_or_else(|| "_section".to_owned());
    let level = match heading.kind {
        asciiloom::parser::HeadingKind::DocumentTitle => "document title".to_owned(),
        asciiloom::parser::HeadingKind::Section { level } => {
            format!("section level {level}")
        }
    };
    let line_index = LineIndex::new(&document.text).map_err(|error| error.to_string())?;
    Ok(Some(json!({
        "contents": {
            "kind": "markdown",
            "value": format!("**{level}**  \nGenerated ID: `{id}`  \nPart: {part}")
        },
        "range": range_to_lsp(range, &line_index, encoding)?
    })))
}

fn completion(document: &DocumentState, offset: u32) -> Result<Vec<Value>, String> {
    let Some(element) = document_element_at(&document.ast, offset) else {
        return Ok(Vec::new());
    };
    let source = match element {
        DocumentElement::SourceLanguage(source) | DocumentElement::SourceAttribute(source) => {
            source
        }
        DocumentElement::HeadingMarker(_) | DocumentElement::HeadingText(_) => {
            return Ok(Vec::new());
        }
    };
    let offset = offset as usize;
    let attribute_start = source.attribute_range.start().to_usize();
    if offset > document.text.len() || !document.text[attribute_start..offset].contains(',') {
        return Ok(Vec::new());
    }
    let prefix = source
        .language_range
        .and_then(|range| {
            let start = range.start().to_usize();
            (start <= offset).then(|| &document.text[start..offset])
        })
        .unwrap_or("");
    Ok(source_language_candidates(prefix)
        .into_iter()
        .map(|language| json!({"label": language, "kind": 12}))
        .collect())
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

fn code_actions(
    document: &DocumentState,
    encoding: PositionEncoding,
) -> Result<Vec<Value>, String> {
    let line_index = LineIndex::new(&document.text).map_err(|error| error.to_string())?;
    let diagnostics = lint::lint(&document.text, &lint::LintConfig::default())
        .map_err(|error| error.to_string())?;
    let mut actions = Vec::new();
    for diagnostic in diagnostics {
        for fix in diagnostic.fixes {
            let edits = fix
                .edits()
                .iter()
                .map(|edit| {
                    Ok(json!({
                        "range": range_to_lsp(edit.range, &line_index, encoding)?,
                        "newText": edit.replacement
                    }))
                })
                .collect::<Result<Vec<_>, String>>()?;
            actions.push(json!({
                "title": fix.title,
                "kind": "quickfix",
                "isPreferred": fix.applicability == asciiloom::diagnostic::Applicability::Always,
                "edit": {
                    "documentChanges": [{
                        "textDocument": {
                            "uri": document.uri,
                            "version": document.version
                        },
                        "edits": edits
                    }]
                }
            }));
        }
    }
    Ok(actions)
}

fn formatting_edits(
    document: &DocumentState,
    encoding: PositionEncoding,
) -> Result<Vec<Value>, String> {
    let output = formatter::format(&document.text, &formatter::FormatConfig::default())
        .map_err(|error| error.to_string())?;
    let line_index = LineIndex::new(&document.text).map_err(|error| error.to_string())?;
    output
        .edits
        .iter()
        .map(|edit| {
            Ok(json!({
                "range": range_to_lsp(edit.range, &line_index, encoding)?,
                "newText": edit.replacement
            }))
        })
        .collect()
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

#[cfg(test)]
mod tests;
