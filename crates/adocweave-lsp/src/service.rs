//! Runtime-independent language features over owned document analyses.

use std::fmt;
use std::sync::Arc;

use adocweave::diagnostic::{Applicability, Severity};
use adocweave::document::{
    DocumentElement, DocumentSymbol as CoreDocumentSymbol, SymbolKind as CoreSymbolKind,
    document_element_at, document_symbols, generate_heading_ids, source_language_candidates,
};
use adocweave::inline::{Inline, MathLanguage, ReferenceDestination};
use adocweave::projection::project;
use adocweave::reference::ReferenceKey;
use adocweave::source::{
    PositionEncoding as CorePositionEncoding, SourceDocument, TextRange as CoreTextRange,
};
use adocweave::{formatter, parser};
use async_lsp::lsp_types as lsp;
use serde::Deserialize;

use crate::state::DocumentStore;
use crate::state::{Adoption, AnalysisJob, DocumentSnapshot};
use crate::{SERVER_NAME, VERSION};

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

pub trait HostReferenceIndex: Send + Sync {
    fn definition(&self, request: &HostReferenceRequest) -> Result<Option<lsp::Location>, String>;

    fn references(
        &self,
        request: &HostReferenceRequest,
        include_declaration: bool,
    ) -> Result<Option<Vec<lsp::Location>>, String>;

    fn is_complete(&self) -> bool;
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HostReferenceRequest {
    pub source: lsp::Url,
    pub source_version: i32,
    pub source_generation: u64,
    pub target: ReferenceKey,
    pub encoding: PositionEncoding,
}

#[derive(Debug, Default)]
pub struct NoHostReferenceIndex;

impl HostReferenceIndex for NoHostReferenceIndex {
    fn definition(&self, _request: &HostReferenceRequest) -> Result<Option<lsp::Location>, String> {
        Ok(None)
    }

    fn references(
        &self,
        _request: &HostReferenceRequest,
        _include_declaration: bool,
    ) -> Result<Option<Vec<lsp::Location>>, String> {
        Ok(None)
    }

    fn is_complete(&self) -> bool {
        false
    }
}

#[derive(Clone)]
pub(crate) struct LanguageService {
    pub documents: DocumentStore,
    pub position_encoding: PositionEncoding,
    settings: ServerSettings,
    host_index: Arc<dyn HostReferenceIndex>,
}

impl fmt::Debug for LanguageService {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("LanguageService")
            .field("documents", &self.documents)
            .field("position_encoding", &self.position_encoding)
            .field("settings", &self.settings)
            .field("has_complete_host_index", &self.host_index.is_complete())
            .finish()
    }
}

impl Default for LanguageService {
    fn default() -> Self {
        Self {
            documents: DocumentStore::default(),
            position_encoding: PositionEncoding::Utf16,
            settings: ServerSettings::default(),
            host_index: Arc::new(NoHostReferenceIndex),
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(default, rename_all = "camelCase", deny_unknown_fields)]
struct ServerSettings {
    debounce_ms: u64,
}

impl Default for ServerSettings {
    fn default() -> Self {
        Self { debounce_ms: 30 }
    }
}

impl LanguageService {
    pub fn with_host_index(host_index: Arc<dyn HostReferenceIndex>) -> Self {
        Self {
            host_index,
            ..Self::default()
        }
    }

    pub fn initialize(&mut self, params: &lsp::InitializeParams) -> lsp::InitializeResult {
        self.position_encoding = negotiate_encoding(params);
        lsp::InitializeResult {
            capabilities: lsp::ServerCapabilities {
                position_encoding: Some(self.position_encoding.lsp()),
                text_document_sync: Some(lsp::TextDocumentSyncCapability::Options(
                    lsp::TextDocumentSyncOptions {
                        open_close: Some(true),
                        change: Some(lsp::TextDocumentSyncKind::INCREMENTAL),
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
                definition_provider: Some(lsp::OneOf::Left(true)),
                references_provider: Some(lsp::OneOf::Left(true)),
                rename_provider: Some(lsp::OneOf::Left(true)),
                document_link_provider: Some(lsp::DocumentLinkOptions {
                    resolve_provider: Some(false),
                    work_done_progress_options: lsp::WorkDoneProgressOptions::default(),
                }),
                semantic_tokens_provider: Some(
                    lsp::SemanticTokensOptions {
                        work_done_progress_options: lsp::WorkDoneProgressOptions::default(),
                        legend: lsp::SemanticTokensLegend {
                            token_types: vec![
                                lsp::SemanticTokenType::KEYWORD,
                                lsp::SemanticTokenType::NAMESPACE,
                                lsp::SemanticTokenType::STRING,
                                lsp::SemanticTokenType::VARIABLE,
                            ],
                            token_modifiers: Vec::new(),
                        },
                        range: None,
                        full: Some(lsp::SemanticTokensFullOptions::Bool(true)),
                    }
                    .into(),
                ),
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

    pub fn begin_change(
        &mut self,
        params: lsp::DidChangeTextDocumentParams,
    ) -> Result<Option<AnalysisJob>, String> {
        let Some(current) = self.documents.get(params.text_document.uri.as_str()) else {
            return Ok(None);
        };
        if params.text_document.version <= current.version {
            return Ok(None);
        }
        let mut source = current.source.to_string();
        for change in params.content_changes {
            match change.range {
                None => source = change.text,
                Some(range) => {
                    let index = SourceDocument::new(&source).map_err(|error| error.to_string())?;
                    let position = |position: lsp::Position| adocweave::source::Position {
                        line: position.line,
                        character: position.character,
                    };
                    let start = index
                        .position_to_offset(position(range.start), self.position_encoding.core())
                        .map_err(|error| error.to_string())?
                        .to_usize();
                    let end = index
                        .position_to_offset(position(range.end), self.position_encoding.core())
                        .map_err(|error| error.to_string())?
                        .to_usize();
                    if start > end {
                        return Err("incremental change range is reversed".to_owned());
                    }
                    source.replace_range(start..end, &change.text);
                }
            }
        }
        Ok(self.documents.begin_change(
            params.text_document.uri.as_str(),
            params.text_document.version,
            source,
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

    pub fn document_cancellation(
        &self,
        uri: &lsp::Url,
    ) -> Option<Arc<adocweave::CancellationToken>> {
        self.documents.cancellation(uri.as_str())
    }

    pub fn update_configuration(&mut self, settings: serde_json::Value) -> Result<(), String> {
        let settings = settings.get("adocweave").cloned().unwrap_or(settings);
        let mut settings: ServerSettings =
            serde_json::from_value(settings).map_err(|error| error.to_string())?;
        settings.debounce_ms = settings.debounce_ms.min(1_000);
        self.settings = settings;
        Ok(())
    }

    pub const fn debounce_ms(&self) -> u64 {
        self.settings.debounce_ms
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
                        analysis.source_document(),
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
                    document.analysis.source_document(),
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
                                document.analysis.source_document(),
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
                        document.analysis.source_document(),
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
        if let Some(attribute) = document
            .analysis
            .ast
            .attributes
            .iter()
            .find(|attribute| contains(attribute.range, offset))
        {
            return hover_markup(
                format!(
                    "**document attribute**  \nName: `{}`  \nRaw value: `{}`",
                    attribute.name, attribute.raw_value
                ),
                attribute.range,
                &document,
                self.position_encoding,
            );
        }
        if let Some(target) = document.analysis.reference_targets.iter().find(|target| {
            contains(target.id_range, offset)
                && !document.analysis.ast.blocks.iter().any(|block| {
                    matches!(
                        block,
                        parser::AstBlock::Heading(heading)
                            if heading.text_range == target.id_range
                    )
                })
        }) {
            return hover_markup(
                format!("**reference target**  \nID: `{}`", target.id),
                target.id_range,
                &document,
                self.position_encoding,
            );
        }
        if let Some((value, range)) = inline_hover(&document.analysis.ast, offset) {
            return hover_markup(value, range, &document, self.position_encoding);
        }
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
                document.analysis.source_document(),
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
        if document
            .analysis
            .references
            .iter()
            .any(|reference| contains(reference.target_range, offset))
        {
            let items = document
                .analysis
                .reference_targets
                .iter()
                .map(|target| lsp::CompletionItem {
                    label: target.id.clone(),
                    detail: Some(target.label.clone()),
                    kind: Some(lsp::CompletionItemKind::REFERENCE),
                    ..lsp::CompletionItem::default()
                })
                .collect();
            return Ok(Some(lsp::CompletionResponse::Array(items)));
        }
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

    pub fn definition(
        &self,
        uri: &lsp::Url,
        position: lsp::Position,
    ) -> Result<Option<lsp::GotoDefinitionResponse>, String> {
        let Some(document) = self.documents.snapshot(uri.as_str()) else {
            return Ok(None);
        };
        let offset = request_offset(&document, position, self.position_encoding)?;
        let Some(reference) = document
            .analysis
            .references
            .iter()
            .find(|reference| contains(reference.range, offset))
        else {
            return Ok(None);
        };
        let Some(key) = ReferenceKey::from_destination(&reference.destination) else {
            return Ok(None);
        };
        if let Some(identity) = reference_identity(uri, &reference.destination) {
            if let Some(location) =
                self.target_location(&identity.uri, identity.anchor.as_deref())?
            {
                return Ok(Some(lsp::GotoDefinitionResponse::Scalar(location)));
            }
        }
        let host_request = host_reference_request(&document, uri, key, self.position_encoding);
        self.host_index
            .definition(&host_request)
            .map(|location| location.map(lsp::GotoDefinitionResponse::Scalar))
    }

    pub fn references(
        &self,
        uri: &lsp::Url,
        position: lsp::Position,
        include_declaration: bool,
    ) -> Result<Option<Vec<lsp::Location>>, String> {
        let Some(document) = self.documents.snapshot(uri.as_str()) else {
            return Ok(Some(Vec::new()));
        };
        let offset = request_offset(&document, position, self.position_encoding)?;
        let reference_at_position = document
            .analysis
            .references
            .iter()
            .find(|reference| contains(reference.range, offset));
        let key = reference_at_position
            .and_then(|reference| ReferenceKey::from_destination(&reference.destination))
            .or_else(|| {
                document
                    .analysis
                    .reference_targets
                    .iter()
                    .find(|target| contains(target.id_range, offset))
                    .map(|target| ReferenceKey::Local {
                        anchor: target.id.clone(),
                    })
            });
        let Some(key) = key else {
            return Ok(Some(Vec::new()));
        };
        let host_request =
            host_reference_request(&document, uri, key.clone(), self.position_encoding);
        if let Some(locations) = self
            .host_index
            .references(&host_request, include_declaration)?
        {
            return Ok(Some(locations));
        }
        let identity = reference_at_position
            .and_then(|reference| reference_identity(uri, &reference.destination))
            .or_else(|| match &key {
                ReferenceKey::Local { anchor } => Some(TargetIdentity {
                    uri: uri.clone(),
                    anchor: Some(anchor.clone()),
                }),
                ReferenceKey::Document { document, anchor } => {
                    uri.join(document).ok().map(|uri| TargetIdentity {
                        uri,
                        anchor: anchor.clone(),
                    })
                }
                ReferenceKey::Scheme { .. } => None,
            });
        let Some(identity) = identity else {
            return Ok(Some(Vec::new()));
        };

        let mut locations = Vec::new();
        if include_declaration {
            if let Some(location) =
                self.target_location(&identity.uri, identity.anchor.as_deref())?
            {
                locations.push(location);
            }
        }
        for candidate in self.documents.snapshots() {
            let candidate_uri: lsp::Url = candidate
                .uri
                .parse()
                .map_err(|error| format!("invalid open document URI {}: {error}", candidate.uri))?;
            for reference in &candidate.analysis.references {
                if reference_identity(&candidate_uri, &reference.destination).as_ref()
                    == Some(&identity)
                {
                    locations.push(lsp::Location::new(
                        candidate_uri.clone(),
                        range_to_lsp(
                            reference.target_range,
                            candidate.analysis.source_document(),
                            self.position_encoding,
                        )?,
                    ));
                }
            }
        }
        Ok(Some(locations))
    }

    pub fn rename(
        &self,
        uri: &lsp::Url,
        position: lsp::Position,
        new_name: &str,
    ) -> Result<Option<lsp::WorkspaceEdit>, String> {
        if !self.host_index.is_complete() || !valid_anchor_name(new_name) {
            return Ok(None);
        }
        let Some(document) = self.documents.snapshot(uri.as_str()) else {
            return Ok(None);
        };
        let offset = request_offset(&document, position, self.position_encoding)?;
        let Some(target) = document
            .analysis
            .reference_targets
            .iter()
            .find(|target| contains(target.id_range, offset))
        else {
            return Ok(None);
        };
        let key = ReferenceKey::Local {
            anchor: target.id.clone(),
        };
        let host_request = host_reference_request(&document, uri, key, self.position_encoding);
        let Some(locations) = self.host_index.references(&host_request, true)? else {
            return Ok(None);
        };
        let mut changes = std::collections::HashMap::<lsp::Url, Vec<lsp::TextEdit>>::new();
        for location in locations {
            changes
                .entry(location.uri)
                .or_default()
                .push(lsp::TextEdit::new(location.range, new_name.to_owned()));
        }
        Ok(Some(lsp::WorkspaceEdit {
            changes: Some(changes),
            ..lsp::WorkspaceEdit::default()
        }))
    }

    pub fn document_links(&self, uri: &lsp::Url) -> Result<Option<Vec<lsp::DocumentLink>>, String> {
        let Some(document) = self.documents.snapshot(uri.as_str()) else {
            return Ok(Some(Vec::new()));
        };
        let mut links = Vec::new();
        for link in project(&document.analysis, &[]).external_links {
            if !adocweave::url::UrlPolicy::default().allows(&link.target) {
                continue;
            }
            let Ok(target) = lsp::Url::parse(&link.target) else {
                continue;
            };
            links.push(lsp::DocumentLink {
                range: range_to_lsp(
                    link.target_range,
                    document.analysis.source_document(),
                    self.position_encoding,
                )?,
                target: Some(target),
                tooltip: Some("外部リンクを開く".to_owned()),
                data: None,
            });
        }
        for reference in &document.analysis.references {
            let target = if let Some(identity) = reference_identity(uri, &reference.destination) {
                let mut target = identity.uri;
                target.set_fragment(identity.anchor.as_deref());
                Some(target)
            } else if let Some(key) = ReferenceKey::from_destination(&reference.destination) {
                let host_request =
                    host_reference_request(&document, uri, key, self.position_encoding);
                self.host_index
                    .definition(&host_request)
                    .ok()
                    .flatten()
                    .map(|location| location.uri)
            } else {
                None
            };
            let Some(target) = target else {
                continue;
            };
            links.push(lsp::DocumentLink {
                range: range_to_lsp(
                    reference.target_range,
                    document.analysis.source_document(),
                    self.position_encoding,
                )?,
                target: Some(target),
                tooltip: Some("参照先を開く".to_owned()),
                data: None,
            });
        }
        links.sort_by_key(|link| {
            (
                link.range.start.line,
                link.range.start.character,
                link.range.end.line,
                link.range.end.character,
            )
        });
        Ok(Some(links))
    }

    pub fn semantic_tokens(
        &self,
        uri: &lsp::Url,
    ) -> Result<Option<lsp::SemanticTokensResult>, String> {
        let Some(document) = self.documents.snapshot(uri.as_str()) else {
            return Ok(Some(lsp::SemanticTokensResult::Tokens(
                lsp::SemanticTokens {
                    result_id: None,
                    data: Vec::new(),
                },
            )));
        };
        let mut raw = Vec::<(lsp::Position, u32, u32)>::new();
        for block in &document.analysis.ast.blocks {
            if let parser::AstBlock::Heading(heading) = block {
                push_semantic_range(
                    &mut raw,
                    heading.marker_range,
                    0,
                    document.analysis.source_document(),
                    self.position_encoding,
                )?;
                push_semantic_range(
                    &mut raw,
                    heading.text_range,
                    1,
                    document.analysis.source_document(),
                    self.position_encoding,
                )?;
            }
        }
        for link in project(&document.analysis, &[]).external_links {
            push_semantic_range(
                &mut raw,
                link.target_range,
                2,
                document.analysis.source_document(),
                self.position_encoding,
            )?;
        }
        for reference in &document.analysis.references {
            push_semantic_range(
                &mut raw,
                reference.target_range,
                2,
                document.analysis.source_document(),
                self.position_encoding,
            )?;
        }
        for target in &document.analysis.reference_targets {
            push_semantic_range(
                &mut raw,
                target.id_range,
                3,
                document.analysis.source_document(),
                self.position_encoding,
            )?;
        }
        let mut inline_ranges = Vec::new();
        document.analysis.ast.visit_inline_sequences(|inlines| {
            collect_inline_semantic_ranges(inlines, &mut inline_ranges);
        });
        for (range, token_type) in inline_ranges {
            push_semantic_range(
                &mut raw,
                range,
                token_type,
                document.analysis.source_document(),
                self.position_encoding,
            )?;
        }
        raw.sort_by_key(|(position, length, token_type)| {
            (position.line, position.character, *length, *token_type)
        });
        raw.dedup();
        let mut previous = lsp::Position::new(0, 0);
        let data = raw
            .into_iter()
            .map(|(position, length, token_type)| {
                let delta_line = position.line - previous.line;
                let delta_start = if delta_line == 0 {
                    position.character - previous.character
                } else {
                    position.character
                };
                previous = position;
                lsp::SemanticToken {
                    delta_line,
                    delta_start,
                    length,
                    token_type,
                    token_modifiers_bitset: 0,
                }
            })
            .collect();
        Ok(Some(lsp::SemanticTokensResult::Tokens(
            lsp::SemanticTokens {
                result_id: None,
                data,
            },
        )))
    }

    fn target_location(
        &self,
        uri: &lsp::Url,
        anchor: Option<&str>,
    ) -> Result<Option<lsp::Location>, String> {
        let Some(document) = self.documents.snapshot(uri.as_str()) else {
            return Ok(None);
        };
        let target = anchor
            .and_then(|anchor| {
                document
                    .analysis
                    .reference_targets
                    .iter()
                    .find(|target| target.id == anchor)
            })
            .or_else(|| document.analysis.reference_targets.first());
        let Some(target) = target else {
            return Ok(None);
        };
        Ok(Some(lsp::Location::new(
            uri.clone(),
            range_to_lsp(
                target.target_range,
                document.analysis.source_document(),
                self.position_encoding,
            )?,
        )))
    }
}

fn hover_markup(
    value: String,
    range: CoreTextRange,
    document: &DocumentSnapshot,
    encoding: PositionEncoding,
) -> Result<Option<lsp::Hover>, String> {
    Ok(Some(lsp::Hover {
        contents: lsp::HoverContents::Markup(lsp::MarkupContent {
            kind: lsp::MarkupKind::Markdown,
            value,
        }),
        range: Some(range_to_lsp(
            range,
            document.analysis.source_document(),
            encoding,
        )?),
    }))
}

fn inline_hover(document: &parser::AstDocument, offset: u32) -> Option<(String, CoreTextRange)> {
    fn find(inlines: &[Inline], offset: u32) -> Option<(String, CoreTextRange)> {
        for inline in inlines {
            if !contains(inline.range(), offset) {
                continue;
            }
            let value = match inline {
                Inline::Link(link) => {
                    Some(format!("**external link**  \nTarget: `{}`", link.target))
                }
                Inline::Reference(reference) => Some(format!(
                    "**cross reference**  \nTarget: `{}`",
                    reference.target_source
                )),
                Inline::Formula(formula) => Some(format!(
                    "**{} formula**  \nContent: `{}`",
                    match formula.language {
                        MathLanguage::Latex => "LaTeX",
                        MathLanguage::Typst => "Typst",
                    },
                    formula.value
                )),
                Inline::AttributeReference { name, .. } => {
                    Some(format!("**attribute reference**  \nName: `{name}`"))
                }
                Inline::Styled { children, .. } => return find(children, offset),
                Inline::Text(_) | Inline::Literal { .. } => None,
            };
            if let Some(value) = value {
                return Some((value, inline.range()));
            }
        }
        None
    }

    let mut found = None;
    document.visit_inline_sequences(|inlines| {
        if found.is_none() {
            found = find(inlines, offset);
        }
    });
    found
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct TargetIdentity {
    uri: lsp::Url,
    anchor: Option<String>,
}

fn reference_identity(
    source_uri: &lsp::Url,
    destination: &ReferenceDestination,
) -> Option<TargetIdentity> {
    match destination {
        ReferenceDestination::Local { anchor, .. } => Some(TargetIdentity {
            uri: source_uri.clone(),
            anchor: Some(anchor.clone()),
        }),
        ReferenceDestination::Document {
            document, anchor, ..
        } => source_uri.join(document).ok().map(|uri| TargetIdentity {
            uri,
            anchor: anchor.clone(),
        }),
        ReferenceDestination::Scheme { .. } | ReferenceDestination::Invalid => None,
    }
}

fn contains(range: CoreTextRange, offset: u32) -> bool {
    range.start().to_u32() <= offset && offset <= range.end().to_u32()
}

fn host_reference_request(
    document: &DocumentSnapshot,
    uri: &lsp::Url,
    target: ReferenceKey,
    encoding: PositionEncoding,
) -> HostReferenceRequest {
    HostReferenceRequest {
        source: uri.clone(),
        source_version: document.version,
        source_generation: document.generation,
        target,
        encoding,
    }
}

fn valid_anchor_name(value: &str) -> bool {
    !value.is_empty()
        && !value.chars().any(|character| {
            character.is_whitespace()
                || character.is_control()
                || matches!(character, '[' | ']' | '<' | '>' | '#')
        })
}

fn push_semantic_range(
    output: &mut Vec<(lsp::Position, u32, u32)>,
    range: CoreTextRange,
    token_type: u32,
    source_document: &SourceDocument,
    encoding: PositionEncoding,
) -> Result<(), String> {
    let range = range_to_lsp(range, source_document, encoding)?;
    for line in range.start.line..=range.end.line {
        let start = if line == range.start.line {
            range.start.character
        } else {
            0
        };
        let end = if line == range.end.line {
            range.end.character
        } else {
            source_document
                .line_length(line, encoding.core())
                .map_err(|error| error.to_string())?
        };
        if end > start {
            output.push((lsp::Position::new(line, start), end - start, token_type));
        }
    }
    Ok(())
}

fn collect_inline_semantic_ranges(
    inlines: &[adocweave::inline::Inline],
    output: &mut Vec<(CoreTextRange, u32)>,
) {
    for inline in inlines {
        match inline {
            adocweave::inline::Inline::Literal { content_range, .. }
            | adocweave::inline::Inline::Formula(adocweave::inline::InlineFormula {
                content_range,
                ..
            }) => output.push((*content_range, 2)),
            adocweave::inline::Inline::Styled { children, .. } => {
                collect_inline_semantic_ranges(children, output);
            }
            adocweave::inline::Inline::Link(link) => {
                collect_inline_semantic_ranges(&link.label, output);
            }
            adocweave::inline::Inline::Reference(reference) => {
                collect_inline_semantic_ranges(&reference.label, output);
            }
            adocweave::inline::Inline::Text(_)
            | adocweave::inline::Inline::AttributeReference { .. } => {}
        }
    }
}

fn request_offset(
    document: &DocumentSnapshot,
    position: lsp::Position,
    encoding: PositionEncoding,
) -> Result<u32, String> {
    if position.line >= document.analysis.source_document().line_count() {
        return Err("position.line is outside the document".to_owned());
    }
    document
        .analysis
        .source_document()
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
    source_document: &SourceDocument,
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
        range: range_to_lsp(symbol.range, source_document, encoding)?,
        selection_range: range_to_lsp(symbol.selection_range, source_document, encoding)?,
        children: Some(
            symbol
                .children
                .iter()
                .map(|child| symbol_to_lsp(child, source_document, encoding))
                .collect::<Result<Vec<_>, _>>()?,
        ),
    })
}

fn range_to_lsp(
    range: CoreTextRange,
    source_document: &SourceDocument,
    encoding: PositionEncoding,
) -> Result<lsp::Range, String> {
    let start = source_document
        .offset_to_position(range.start(), encoding.core())
        .map_err(|error| error.to_string())?;
    let end = source_document
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
