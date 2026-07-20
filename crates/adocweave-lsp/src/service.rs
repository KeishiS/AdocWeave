//! Runtime-independent language features over owned document analyses.

use adocweave::diagnostic::{Applicability, Severity};
use adocweave::document::{
    DocumentElement, DocumentSymbol as CoreDocumentSymbol, SymbolKind as CoreSymbolKind,
    document_element_at, document_symbols, generate_heading_ids, source_language_candidates,
};
use adocweave::source::{
    LineIndex, PositionEncoding as CorePositionEncoding, TextRange as CoreTextRange,
};
use adocweave::{formatter, parser};
use async_lsp::lsp_types as lsp;

use crate::state::{Adoption, AnalysisJob, DocumentSnapshot};
use crate::{DocumentStore, SERVER_NAME, VERSION};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PositionEncoding {
    Utf8,
    Utf16,
}

impl PositionEncoding {
    const fn core(self) -> CorePositionEncoding {
        match self {
            Self::Utf8 => CorePositionEncoding::Utf8,
            Self::Utf16 => CorePositionEncoding::Utf16,
        }
    }

    fn lsp(self) -> lsp::PositionEncodingKind {
        match self {
            Self::Utf8 => lsp::PositionEncodingKind::UTF8,
            Self::Utf16 => lsp::PositionEncodingKind::UTF16,
        }
    }
}

#[derive(Clone, Debug)]
pub struct LanguageService {
    pub documents: DocumentStore,
    pub position_encoding: PositionEncoding,
}

impl Default for LanguageService {
    fn default() -> Self {
        Self {
            documents: DocumentStore::default(),
            position_encoding: PositionEncoding::Utf16,
        }
    }
}

impl LanguageService {
    pub fn initialize(&mut self, params: &lsp::InitializeParams) -> lsp::InitializeResult {
        self.position_encoding = negotiate_encoding(params);
        lsp::InitializeResult {
            capabilities: lsp::ServerCapabilities {
                position_encoding: Some(self.position_encoding.lsp()),
                text_document_sync: Some(lsp::TextDocumentSyncCapability::Options(
                    lsp::TextDocumentSyncOptions {
                        open_close: Some(true),
                        change: Some(lsp::TextDocumentSyncKind::FULL),
                        save: Some(
                            lsp::SaveOptions {
                                include_text: Some(true),
                            }
                            .into(),
                        ),
                        ..lsp::TextDocumentSyncOptions::default()
                    },
                )),
                document_symbol_provider: Some(lsp::OneOf::Left(true)),
                code_action_provider: Some(lsp::CodeActionProviderCapability::Simple(true)),
                document_formatting_provider: Some(lsp::OneOf::Left(true)),
                hover_provider: Some(lsp::HoverProviderCapability::Simple(true)),
                completion_provider: Some(lsp::CompletionOptions {
                    trigger_characters: Some(vec![",".to_owned(), " ".to_owned()]),
                    ..lsp::CompletionOptions::default()
                }),
                ..lsp::ServerCapabilities::default()
            },
            server_info: Some(lsp::ServerInfo {
                name: SERVER_NAME.to_owned(),
                version: Some(VERSION.to_owned()),
            }),
        }
    }

    pub fn begin_open(&mut self, params: lsp::DidOpenTextDocumentParams) -> AnalysisJob {
        let document = params.text_document;
        self.documents
            .begin_open(document.uri.to_string(), document.version, document.text)
    }

    pub fn begin_change_full(
        &mut self,
        params: lsp::DidChangeTextDocumentParams,
    ) -> Result<Option<AnalysisJob>, String> {
        if params
            .content_changes
            .iter()
            .any(|change| change.range.is_some())
        {
            return Err("incremental changes are not accepted; send full text".to_owned());
        }
        let Some(change) = params.content_changes.into_iter().last() else {
            return Err("full change text is missing".to_owned());
        };
        Ok(self.documents.begin_change(
            params.text_document.uri.as_str(),
            params.text_document.version,
            change.text,
        ))
    }

    pub fn adopt(&mut self, job: &AnalysisJob, analysis: adocweave::Analysis) -> Adoption {
        self.documents.adopt(job, analysis)
    }

    pub fn close(&mut self, uri: &lsp::Url) -> bool {
        self.documents.close(uri.as_str())
    }

    pub fn cancel_all(&mut self) {
        self.documents.cancel_all();
    }

    pub fn diagnostics(&self, uri: &lsp::Url) -> Result<lsp::PublishDiagnosticsParams, String> {
        let Some(document) = self.documents.get(uri.as_str()) else {
            return Ok(lsp::PublishDiagnosticsParams::new(
                uri.clone(),
                Vec::new(),
                None,
            ));
        };
        let Some(analysis) = document.analysis.as_ref() else {
            return Ok(lsp::PublishDiagnosticsParams::new(
                uri.clone(),
                Vec::new(),
                Some(document.version),
            ));
        };
        let diagnostics = analysis
            .diagnostics
            .iter()
            .map(|diagnostic| {
                Ok(lsp::Diagnostic {
                    range: range_to_lsp(
                        diagnostic.range,
                        &analysis.line_index,
                        self.position_encoding,
                    )?,
                    severity: Some(match diagnostic.severity {
                        Severity::Error => lsp::DiagnosticSeverity::ERROR,
                        Severity::Warning => lsp::DiagnosticSeverity::WARNING,
                        Severity::Information => lsp::DiagnosticSeverity::INFORMATION,
                        Severity::Hint => lsp::DiagnosticSeverity::HINT,
                    }),
                    code: Some(lsp::NumberOrString::String(
                        diagnostic.code.as_str().to_owned(),
                    )),
                    source: Some("adocweave".to_owned()),
                    message: diagnostic.message.clone(),
                    ..lsp::Diagnostic::default()
                })
            })
            .collect::<Result<Vec<_>, String>>()?;
        Ok(lsp::PublishDiagnosticsParams::new(
            uri.clone(),
            diagnostics,
            Some(document.version),
        ))
    }

    pub fn document_symbols(
        &self,
        uri: &lsp::Url,
    ) -> Result<Option<lsp::DocumentSymbolResponse>, String> {
        let Some(document) = self.documents.snapshot(uri.as_str()) else {
            return Ok(Some(lsp::DocumentSymbolResponse::Nested(Vec::new())));
        };
        let symbols = document_symbols(&document.analysis.ast)
            .iter()
            .map(|symbol| {
                symbol_to_lsp(
                    symbol,
                    &document.analysis.line_index,
                    self.position_encoding,
                )
            })
            .collect::<Result<Vec<_>, String>>()?;
        Ok(Some(lsp::DocumentSymbolResponse::Nested(symbols)))
    }

    pub fn code_actions(
        &self,
        uri: &lsp::Url,
    ) -> Result<Option<Vec<lsp::CodeActionOrCommand>>, String> {
        let Some(document) = self.documents.snapshot(uri.as_str()) else {
            return Ok(Some(Vec::new()));
        };
        let mut actions = Vec::new();
        for diagnostic in &document.analysis.diagnostics {
            for fix in &diagnostic.fixes {
                let edits = fix
                    .edits()
                    .iter()
                    .map(|edit| {
                        Ok(lsp::OneOf::Left(lsp::TextEdit::new(
                            range_to_lsp(
                                edit.range,
                                &document.analysis.line_index,
                                self.position_encoding,
                            )?,
                            edit.replacement.clone(),
                        )))
                    })
                    .collect::<Result<Vec<_>, String>>()?;
                actions.push(lsp::CodeActionOrCommand::CodeAction(lsp::CodeAction {
                    title: fix.title.clone(),
                    kind: Some(lsp::CodeActionKind::QUICKFIX),
                    edit: Some(lsp::WorkspaceEdit {
                        document_changes: Some(lsp::DocumentChanges::Edits(vec![
                            lsp::TextDocumentEdit {
                                text_document: lsp::OptionalVersionedTextDocumentIdentifier {
                                    uri: uri.clone(),
                                    version: Some(document.version),
                                },
                                edits,
                            },
                        ])),
                        ..lsp::WorkspaceEdit::default()
                    }),
                    is_preferred: Some(fix.applicability == Applicability::Always),
                    ..lsp::CodeAction::default()
                }));
            }
        }
        Ok(Some(actions))
    }

    pub fn formatting(&self, uri: &lsp::Url) -> Result<Option<Vec<lsp::TextEdit>>, String> {
        let Some(document) = self.documents.snapshot(uri.as_str()) else {
            return Ok(Some(Vec::new()));
        };
        let output =
            formatter::format_analysis(&document.analysis, &formatter::FormatConfig::default())
                .map_err(|error| error.to_string())?;
        let edits = output
            .edits
            .iter()
            .map(|edit| {
                Ok(lsp::TextEdit::new(
                    range_to_lsp(
                        edit.range,
                        &document.analysis.line_index,
                        self.position_encoding,
                    )?,
                    edit.replacement.clone(),
                ))
            })
            .collect::<Result<Vec<_>, String>>()?;
        Ok(Some(edits))
    }

    pub fn hover(
        &self,
        uri: &lsp::Url,
        position: lsp::Position,
    ) -> Result<Option<lsp::Hover>, String> {
        let Some(document) = self.documents.snapshot(uri.as_str()) else {
            return Ok(None);
        };
        let offset = request_offset(&document, position, self.position_encoding)?;
        let Some(element) = document_element_at(&document.analysis.ast, offset) else {
            return Ok(None);
        };
        let (heading, range, part) = match element {
            DocumentElement::HeadingMarker(heading) => (heading, heading.marker_range, "marker"),
            DocumentElement::HeadingText(heading) => (heading, heading.text_range, "text"),
            DocumentElement::SourceLanguage(_) | DocumentElement::SourceAttribute(_) => {
                return Ok(None);
            }
        };
        let id = generate_heading_ids(&document.analysis.ast)
            .into_iter()
            .find(|candidate| candidate.range == heading.text_range)
            .map(|candidate| candidate.id)
            .unwrap_or_else(|| "_section".to_owned());
        let level = match heading.kind {
            parser::HeadingKind::DocumentTitle => "document title".to_owned(),
            parser::HeadingKind::Section { level } => format!("section level {level}"),
        };
        Ok(Some(lsp::Hover {
            contents: lsp::HoverContents::Markup(lsp::MarkupContent {
                kind: lsp::MarkupKind::Markdown,
                value: format!("**{level}**  \nGenerated ID: `{id}`  \nPart: {part}"),
            }),
            range: Some(range_to_lsp(
                range,
                &document.analysis.line_index,
                self.position_encoding,
            )?),
        }))
    }

    pub fn completion(
        &self,
        uri: &lsp::Url,
        position: lsp::Position,
    ) -> Result<Option<lsp::CompletionResponse>, String> {
        let Some(document) = self.documents.snapshot(uri.as_str()) else {
            return Ok(Some(lsp::CompletionResponse::Array(Vec::new())));
        };
        let offset = request_offset(&document, position, self.position_encoding)?;
        let Some(element) = document_element_at(&document.analysis.ast, offset) else {
            return Ok(Some(lsp::CompletionResponse::Array(Vec::new())));
        };
        let source = match element {
            DocumentElement::SourceLanguage(source) | DocumentElement::SourceAttribute(source) => {
                source
            }
            DocumentElement::HeadingMarker(_) | DocumentElement::HeadingText(_) => {
                return Ok(Some(lsp::CompletionResponse::Array(Vec::new())));
            }
        };
        let offset = offset as usize;
        let text = document.analysis.source();
        let attribute_start = source.attribute_range.start().to_usize();
        if offset > text.len() || !text[attribute_start..offset].contains(',') {
            return Ok(Some(lsp::CompletionResponse::Array(Vec::new())));
        }
        let prefix = source
            .language_range
            .and_then(|range| {
                let start = range.start().to_usize();
                (start <= offset).then(|| &text[start..offset])
            })
            .unwrap_or("");
        let items = source_language_candidates(prefix)
            .into_iter()
            .map(|language| lsp::CompletionItem {
                label: language.to_owned(),
                kind: Some(lsp::CompletionItemKind::VALUE),
                ..lsp::CompletionItem::default()
            })
            .collect();
        Ok(Some(lsp::CompletionResponse::Array(items)))
    }
}

fn request_offset(
    document: &DocumentSnapshot,
    position: lsp::Position,
    encoding: PositionEncoding,
) -> Result<u32, String> {
    if position.line >= document.analysis.line_index.line_count() {
        return Err("position.line is outside the document".to_owned());
    }
    document
        .analysis
        .line_index
        .position_to_offset(
            adocweave::source::Position {
                line: position.line,
                character: position.character,
            },
            encoding.core(),
        )
        .map(|offset| offset.to_u32())
        .map_err(|error| error.to_string())
}

#[allow(deprecated)]
fn symbol_to_lsp(
    symbol: &CoreDocumentSymbol,
    line_index: &LineIndex,
    encoding: PositionEncoding,
) -> Result<lsp::DocumentSymbol, String> {
    Ok(lsp::DocumentSymbol {
        name: symbol.name.clone(),
        detail: None,
        kind: match symbol.kind {
            CoreSymbolKind::DocumentTitle => lsp::SymbolKind::FILE,
            CoreSymbolKind::Section => lsp::SymbolKind::NAMESPACE,
            CoreSymbolKind::ListItem => lsp::SymbolKind::STRING,
        },
        tags: None,
        deprecated: None,
        range: range_to_lsp(symbol.range, line_index, encoding)?,
        selection_range: range_to_lsp(symbol.selection_range, line_index, encoding)?,
        children: Some(
            symbol
                .children
                .iter()
                .map(|child| symbol_to_lsp(child, line_index, encoding))
                .collect::<Result<Vec<_>, _>>()?,
        ),
    })
}

fn range_to_lsp(
    range: CoreTextRange,
    line_index: &LineIndex,
    encoding: PositionEncoding,
) -> Result<lsp::Range, String> {
    let start = line_index
        .offset_to_position(range.start(), encoding.core())
        .map_err(|error| error.to_string())?;
    let end = line_index
        .offset_to_position(range.end(), encoding.core())
        .map_err(|error| error.to_string())?;
    Ok(lsp::Range::new(
        lsp::Position::new(start.line, start.character),
        lsp::Position::new(end.line, end.character),
    ))
}

fn negotiate_encoding(params: &lsp::InitializeParams) -> PositionEncoding {
    if params
        .capabilities
        .general
        .as_ref()
        .and_then(|general| general.position_encodings.as_ref())
        .is_some_and(|encodings| encodings.contains(&lsp::PositionEncodingKind::UTF8))
    {
        PositionEncoding::Utf8
    } else {
        PositionEncoding::Utf16
    }
}
