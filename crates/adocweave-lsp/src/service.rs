//! Runtime-independent language features over owned document analyses.

use std::fmt;
use std::sync::Arc;

use adocweave::output::diagnostics::{Applicability, RuleSettings, Severity, lint_rule};
use adocweave::output::formatter;
use adocweave::output::projection::project;
use adocweave::resolution::ReferenceKey;
use adocweave::semantic as parser;
use adocweave::semantic::{
    DocumentElement, document_element_at, generate_heading_ids, source_language_candidates,
};
use adocweave::semantic::{Inline, MathLanguage};
use adocweave::text::{SourceDocument, TextRange as CoreTextRange};
use async_lsp::lsp_types as lsp;
use serde::Deserialize;

use crate::document_symbols::SymbolPresentation;
use crate::position::{
    PositionEncoding, cursor_touches_range, negotiate_encoding, range_contains_offset,
    range_to_lsp, ranges_intersect, request_offset,
};
use crate::state::DocumentStore;
use crate::state::{
    Adoption, AnalysisJob, DocumentSnapshot, WorkspaceAnalysis as DocumentWorkspaceAnalysis,
    WorkspaceProblem,
};
use crate::workspace::WorkspaceResources;
use crate::{SERVER_NAME, VERSION};

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
enum HoverPresentation {
    #[default]
    Legacy,
    Markdown,
    PlainText,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ClientProfile {
    hover: HoverPresentation,
    hierarchical_document_symbols: bool,
    code_action_quickfix: bool,
    code_action_is_preferred: bool,
    versioned_document_changes: bool,
    diagnostic_version: bool,
    document_link_tooltip: bool,
    semantic_tokens_full: bool,
    workspace_folders: bool,
    watched_files_dynamic_registration: bool,
}

impl Default for ClientProfile {
    fn default() -> Self {
        Self {
            hover: HoverPresentation::Markdown,
            hierarchical_document_symbols: true,
            code_action_quickfix: true,
            code_action_is_preferred: true,
            versioned_document_changes: true,
            diagnostic_version: true,
            document_link_tooltip: true,
            semantic_tokens_full: true,
            workspace_folders: false,
            watched_files_dynamic_registration: false,
        }
    }
}

impl ClientProfile {
    fn from_capabilities(capabilities: &lsp::ClientCapabilities) -> Self {
        let text_document = capabilities.text_document.as_ref();
        let workspace = capabilities.workspace.as_ref();
        let hover = text_document
            .and_then(|capabilities| capabilities.hover.as_ref())
            .and_then(|capabilities| capabilities.content_format.as_ref())
            .and_then(|formats| {
                formats.iter().find_map(|format| {
                    if format == &lsp::MarkupKind::Markdown {
                        Some(HoverPresentation::Markdown)
                    } else if format == &lsp::MarkupKind::PlainText {
                        Some(HoverPresentation::PlainText)
                    } else {
                        None
                    }
                })
            })
            .unwrap_or_default();
        let code_action = text_document.and_then(|capabilities| capabilities.code_action.as_ref());
        let code_action_quickfix = code_action
            .and_then(|capabilities| capabilities.code_action_literal_support.as_ref())
            .is_some_and(|support| {
                support
                    .code_action_kind
                    .value_set
                    .iter()
                    .any(|kind| kind == lsp::CodeActionKind::QUICKFIX.as_str())
            });
        Self {
            hover,
            hierarchical_document_symbols: text_document
                .and_then(|capabilities| capabilities.document_symbol.as_ref())
                .and_then(|capabilities| capabilities.hierarchical_document_symbol_support)
                == Some(true),
            code_action_quickfix,
            code_action_is_preferred: code_action
                .and_then(|capabilities| capabilities.is_preferred_support)
                == Some(true),
            versioned_document_changes: workspace
                .and_then(|capabilities| capabilities.workspace_edit.as_ref())
                .and_then(|capabilities| capabilities.document_changes)
                == Some(true),
            diagnostic_version: text_document
                .and_then(|capabilities| capabilities.publish_diagnostics.as_ref())
                .and_then(|capabilities| capabilities.version_support)
                == Some(true),
            document_link_tooltip: text_document
                .and_then(|capabilities| capabilities.document_link.as_ref())
                .and_then(|capabilities| capabilities.tooltip_support)
                == Some(true),
            semantic_tokens_full: text_document
                .and_then(|capabilities| capabilities.semantic_tokens.as_ref())
                .is_some_and(|capabilities| {
                    capabilities.requests.full.as_ref().is_some_and(|full| {
                        matches!(
                            full,
                            lsp::SemanticTokensFullOptions::Bool(true)
                                | lsp::SemanticTokensFullOptions::Delta { .. }
                        )
                    }) && capabilities.formats.contains(&lsp::TokenFormat::RELATIVE)
                        && capabilities
                            .token_types
                            .contains(&lsp::SemanticTokenType::STRING)
                        && capabilities
                            .token_types
                            .contains(&lsp::SemanticTokenType::VARIABLE)
                }),
            workspace_folders: workspace.and_then(|capabilities| capabilities.workspace_folders)
                == Some(true),
            watched_files_dynamic_registration: workspace
                .and_then(|capabilities| capabilities.did_change_watched_files)
                .and_then(|capabilities| capabilities.dynamic_registration)
                == Some(true),
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
    client: ClientProfile,
    settings: ServerSettings,
    host_index: Arc<dyn HostReferenceIndex>,
    workspace: WorkspaceResources,
    workspace_roots: std::collections::BTreeMap<String, lsp::Url>,
    workspace_error: Option<String>,
}

impl fmt::Debug for LanguageService {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("LanguageService")
            .field("documents", &self.documents)
            .field("position_encoding", &self.position_encoding)
            .field("client", &self.client)
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
            client: ClientProfile::default(),
            settings: ServerSettings::default(),
            host_index: Arc::new(NoHostReferenceIndex),
            workspace: WorkspaceResources::default(),
            workspace_roots: std::collections::BTreeMap::new(),
            workspace_error: None,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(default, rename_all = "camelCase", deny_unknown_fields)]
struct ServerSettings {
    debounce_ms: u64,
    enabled_rules: Vec<String>,
}

impl Default for ServerSettings {
    fn default() -> Self {
        Self {
            debounce_ms: 30,
            enabled_rules: Vec::new(),
        }
    }
}

fn attach_workspace(
    job: &mut AnalysisJob,
    input: Result<crate::workspace::WorkspaceInput, String>,
) {
    match input {
        Ok(input) => job.workspace = Some(input),
        Err(message) => {
            job.workspace_problem = Some(WorkspaceProblem {
                source_id: Some(job.uri.clone()),
                range: adocweave::text::TextRange::new(
                    adocweave::text::TextSize::ZERO,
                    adocweave::text::TextSize::ZERO,
                )
                .expect("zero range"),
                code: "workspace-input-error".to_owned(),
                message,
            });
        }
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
        self.client = ClientProfile::from_capabilities(&params.capabilities);
        self.position_encoding = negotiate_encoding(params);
        let roots: Vec<lsp::Url> = if self.client.workspace_folders {
            params
                .workspace_folders
                .as_ref()
                .into_iter()
                .flatten()
                .map(|folder| folder.uri.clone())
                .collect()
        } else {
            #[allow(deprecated)]
            params
                .root_uri
                .clone()
                .or_else(|| {
                    params
                        .root_path
                        .as_deref()
                        .and_then(|path| lsp::Url::from_directory_path(path).ok())
                })
                .into_iter()
                .collect()
        };
        match self.workspace.load_roots(&roots) {
            Ok(()) => {
                self.workspace_roots = roots
                    .into_iter()
                    .map(|uri| (uri.to_string(), uri))
                    .collect();
                self.workspace_error = None;
            }
            Err(error) => self.workspace_error = Some(error),
        }
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
                code_action_provider: self
                    .client
                    .code_action_quickfix
                    .then_some(lsp::CodeActionProviderCapability::Simple(true)),
                document_formatting_provider: Some(lsp::OneOf::Left(true)),
                hover_provider: Some(lsp::HoverProviderCapability::Simple(true)),
                definition_provider: Some(lsp::OneOf::Left(true)),
                references_provider: Some(lsp::OneOf::Left(true)),
                rename_provider: Some(lsp::OneOf::Left(true)),
                document_link_provider: Some(lsp::DocumentLinkOptions {
                    resolve_provider: Some(false),
                    work_done_progress_options: lsp::WorkDoneProgressOptions::default(),
                }),
                semantic_tokens_provider: self.client.semantic_tokens_full.then_some(
                    lsp::SemanticTokensOptions {
                        work_done_progress_options: lsp::WorkDoneProgressOptions::default(),
                        legend: lsp::SemanticTokensLegend {
                            token_types: vec![
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
                workspace: self.client.workspace_folders.then_some(
                    lsp::WorkspaceServerCapabilities {
                        workspace_folders: Some(lsp::WorkspaceFoldersServerCapabilities {
                            supported: Some(true),
                            change_notifications: Some(lsp::OneOf::Left(true)),
                        }),
                        file_operations: None,
                    },
                ),
                ..lsp::ServerCapabilities::default()
            },
            server_info: Some(lsp::ServerInfo {
                name: SERVER_NAME.to_owned(),
                version: Some(VERSION.to_owned()),
            }),
        }
    }

    pub fn begin_open(&mut self, params: lsp::DidOpenTextDocumentParams) -> Vec<AnalysisJob> {
        let document = params.text_document;
        let affected = self
            .workspace
            .upsert_open(
                document.uri.clone(),
                i64::from(document.version),
                document.text.clone(),
            )
            .unwrap_or_else(|_| std::collections::BTreeSet::from([document.uri.to_string()]));
        let workspace = self.workspace.input(&document.uri);
        let options = self.analysis_options_for(workspace.as_ref().ok());
        let mut job = self.documents.begin_open_with_options(
            document.uri.to_string(),
            document.version,
            document.text,
            options,
        );
        attach_workspace(&mut job, workspace);
        let mut jobs = vec![job];
        self.append_dependent_jobs(&affected, document.uri.as_str(), &mut jobs);
        jobs
    }

    pub fn begin_change(
        &mut self,
        params: lsp::DidChangeTextDocumentParams,
    ) -> Result<Vec<AnalysisJob>, String> {
        let Some(current) = self.documents.get(params.text_document.uri.as_str()) else {
            return Ok(Vec::new());
        };
        if i64::from(params.text_document.version) <= current.request.revision.version {
            return Ok(Vec::new());
        }
        let mut source = current.request.source.to_string();
        for change in params.content_changes {
            match change.range {
                None => source = change.text,
                Some(range) => {
                    let index = SourceDocument::new(&source).map_err(|error| error.to_string())?;
                    let position = |position: lsp::Position| adocweave::text::Position {
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
        let affected = self.workspace.upsert_open(
            params.text_document.uri.clone(),
            i64::from(params.text_document.version),
            source.clone(),
        )?;
        let Some(mut job) = self.documents.begin_change(
            params.text_document.uri.as_str(),
            params.text_document.version,
            source,
        ) else {
            return Ok(Vec::new());
        };
        attach_workspace(&mut job, self.workspace.input(&params.text_document.uri));
        let mut jobs = vec![job];
        self.append_dependent_jobs(&affected, params.text_document.uri.as_str(), &mut jobs);
        Ok(jobs)
    }

    fn append_dependent_jobs(
        &mut self,
        affected: &std::collections::BTreeSet<String>,
        changed: &str,
        jobs: &mut Vec<AnalysisJob>,
    ) {
        for uri in affected.iter().filter(|uri| uri.as_str() != changed) {
            let Ok(parsed) = uri.parse() else {
                continue;
            };
            let Some(mut job) = self.documents.begin_reanalysis(uri) else {
                continue;
            };
            attach_workspace(&mut job, self.workspace.input(&parsed));
            jobs.push(job);
        }
    }

    fn analysis_options_for(
        &self,
        workspace: Option<&crate::workspace::WorkspaceInput>,
    ) -> adocweave::AnalysisOptions {
        let mut options = workspace.map_or_else(adocweave::AnalysisOptions::default, |input| {
            input.project_config.analysis.clone()
        });
        for code in &self.settings.enabled_rules {
            let Some(descriptor) = lint_rule(code) else {
                continue;
            };
            options.diagnostics.lint.set_rule(
                descriptor.id,
                RuleSettings {
                    enabled: true,
                    severity: options.diagnostics.lint.rule(descriptor.id).severity,
                },
            );
        }
        options
    }

    pub fn workspace_files_changed(
        &mut self,
        params: lsp::DidChangeWatchedFilesParams,
    ) -> Vec<AnalysisJob> {
        let mut affected = std::collections::BTreeSet::new();
        let mut configuration_changed = false;
        for change in params.changes {
            if change.uri.path_segments().and_then(Iterator::last)
                == Some(adocweave_config::FILE_NAME)
            {
                configuration_changed = true;
                continue;
            }
            if self.documents.get(change.uri.as_str()).is_some() {
                continue;
            }
            let changed = if change.typ == lsp::FileChangeType::DELETED {
                Ok(self.workspace.remove_disk(&change.uri))
            } else {
                self.workspace.reload_file(change.uri)
            };
            match changed {
                Ok(changed) => affected.extend(changed),
                Err(error) => self.workspace_error = Some(error),
            }
        }
        if configuration_changed {
            return self.reload_project_configuration();
        }
        let mut jobs = Vec::new();
        self.append_dependent_jobs(&affected, "", &mut jobs);
        jobs
    }

    fn reload_project_configuration(&mut self) -> Vec<AnalysisJob> {
        let roots = self.workspace_roots.values().cloned().collect::<Vec<_>>();
        let open_sources = self.documents.open_sources();
        if let Err(error) = self.workspace.load_roots(&roots) {
            self.workspace_error = Some(error);
            return Vec::new();
        }
        for (uri, version, source) in &open_sources {
            let Ok(uri) = uri.parse() else {
                continue;
            };
            if let Err(error) = self
                .workspace
                .upsert_open(uri, i64::from(*version), source.clone())
            {
                self.workspace_error = Some(error);
            }
        }
        self.workspace_error = None;
        open_sources
            .into_iter()
            .filter_map(|(uri, _, _)| {
                let parsed = uri.parse().ok()?;
                let workspace = self.workspace.input(&parsed);
                let options = self.analysis_options_for(workspace.as_ref().ok());
                let mut job = self.documents.reconfigure(&uri, options)?;
                attach_workspace(&mut job, workspace);
                Some(job)
            })
            .collect()
    }

    pub fn workspace_folders_changed(
        &mut self,
        params: lsp::DidChangeWorkspaceFoldersParams,
    ) -> Vec<AnalysisJob> {
        if !self.client.workspace_folders {
            return Vec::new();
        }
        let mut roots = self.workspace_roots.clone();
        for folder in params.event.removed {
            roots.remove(folder.uri.as_str());
        }
        for folder in params.event.added {
            roots.insert(folder.uri.to_string(), folder.uri);
        }
        let root_uris = roots.values().cloned().collect::<Vec<_>>();
        let open_sources = self.documents.open_sources();
        if let Err(error) = self.workspace.load_roots(&root_uris) {
            self.workspace_error = Some(error);
            return Vec::new();
        }
        self.workspace_roots = roots;
        self.workspace_error = None;
        for (uri, version, source) in &open_sources {
            let Ok(uri) = uri.parse() else {
                continue;
            };
            if let Err(error) = self
                .workspace
                .upsert_open(uri, i64::from(*version), source.clone())
            {
                self.workspace_error = Some(error);
            }
        }
        open_sources
            .into_iter()
            .filter_map(|(uri, _, _)| {
                let parsed = uri.parse().ok()?;
                let workspace = self.workspace.input(&parsed);
                let options = self.analysis_options_for(workspace.as_ref().ok());
                let mut job = self.documents.reconfigure(&uri, options)?;
                attach_workspace(&mut job, workspace);
                Some(job)
            })
            .collect()
    }

    pub fn watched_files_registration(&self) -> Option<lsp::RegistrationParams> {
        self.client
            .watched_files_dynamic_registration
            .then(|| lsp::RegistrationParams {
                registrations: vec![lsp::Registration {
                    id: "adocweave-watch-asciidoc".to_owned(),
                    method: "workspace/didChangeWatchedFiles".to_owned(),
                    register_options: Some(
                        serde_json::to_value(lsp::DidChangeWatchedFilesRegistrationOptions {
                            watchers: vec![
                                lsp::FileSystemWatcher {
                                    glob_pattern: lsp::GlobPattern::String("**/*.adoc".to_owned()),
                                    kind: Some(
                                        lsp::WatchKind::Create
                                            | lsp::WatchKind::Change
                                            | lsp::WatchKind::Delete,
                                    ),
                                },
                                lsp::FileSystemWatcher {
                                    glob_pattern: lsp::GlobPattern::String(format!(
                                        "**/{}",
                                        adocweave_config::FILE_NAME
                                    )),
                                    kind: Some(
                                        lsp::WatchKind::Create
                                            | lsp::WatchKind::Change
                                            | lsp::WatchKind::Delete,
                                    ),
                                },
                            ],
                        })
                        .expect("watched file registration is serializable"),
                    ),
                }],
            })
    }

    pub fn adopt(&mut self, job: &AnalysisJob, result: adocweave::AnalysisResult) -> Adoption {
        if job
            .workspace
            .as_ref()
            .is_some_and(|input| !self.workspace.input_is_current(input))
        {
            return Adoption::Stale;
        }
        let format = job
            .workspace
            .as_ref()
            .map_or_else(formatter::FormatConfig::default, |input| {
                input.project_config.format
            });
        self.documents.adopt_with_format(job, result, format)
    }

    pub fn adopt_workspace(
        &mut self,
        job: &AnalysisJob,
        analysis: adocweave_workspace::WorkspaceAnalysis,
    ) -> Adoption {
        if job
            .workspace
            .as_ref()
            .is_none_or(|input| !self.workspace.input_is_current(input))
        {
            return Adoption::Stale;
        }
        if self.workspace.accept(&analysis).is_err() {
            return Adoption::Stale;
        }
        let resource_versions = analysis
            .resource_revisions
            .iter()
            .map(|(id, revision)| (id.to_string(), revision.get()))
            .collect();
        self.documents.adopt_workspace(
            job,
            DocumentWorkspaceAnalysis {
                document: analysis.document,
                analysis: analysis.analysis,
                projection: analysis.projection,
                resource_versions,
            },
        )
    }

    pub fn adopt_workspace_problem(
        &mut self,
        job: &AnalysisJob,
        problem: WorkspaceProblem,
    ) -> Adoption {
        if job.workspace_problem.is_none()
            && job
                .workspace
                .as_ref()
                .is_none_or(|input| !self.workspace.input_is_current(input))
        {
            return Adoption::Stale;
        }
        self.documents.adopt_workspace_problem(job, problem)
    }

    pub fn close(&mut self, uri: &lsp::Url) -> (bool, Vec<AnalysisJob>) {
        let closed = self.documents.close(uri.as_str());
        let affected = self.workspace.close_open(uri).unwrap_or_else(|error| {
            self.workspace_error = Some(error);
            std::collections::BTreeSet::new()
        });
        let mut jobs = Vec::new();
        self.append_dependent_jobs(&affected, uri.as_str(), &mut jobs);
        (closed, jobs)
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

    pub fn update_configuration(
        &mut self,
        settings: serde_json::Value,
    ) -> Result<Vec<AnalysisJob>, String> {
        let settings = settings.get("adocweave").cloned().unwrap_or(settings);
        let mut settings: ServerSettings =
            serde_json::from_value(settings).map_err(|error| error.to_string())?;
        settings.debounce_ms = settings.debounce_ms.min(1_000);
        for code in &settings.enabled_rules {
            let descriptor =
                lint_rule(code).ok_or_else(|| format!("unknown diagnostic rule: {code}"))?;
            if descriptor.default_enabled {
                return Err(format!(
                    "diagnostic rule cannot be enabled explicitly: {code}"
                ));
            }
        }
        let diagnostics_changed = self.settings.enabled_rules != settings.enabled_rules;
        self.settings = settings;
        if !diagnostics_changed {
            return Ok(Vec::new());
        }
        Ok(self
            .documents
            .open_sources()
            .into_iter()
            .filter_map(|(uri, _, _)| {
                let parsed = uri.parse().ok()?;
                let workspace = self.workspace.input(&parsed);
                let options = self.analysis_options_for(workspace.as_ref().ok());
                let mut job = self.documents.reconfigure(&uri, options)?;
                attach_workspace(&mut job, workspace);
                Some(job)
            })
            .collect())
    }

    pub const fn debounce_ms(&self) -> u64 {
        self.settings.debounce_ms
    }

    pub fn diagnostics(&self, uri: &lsp::Url) -> Result<lsp::PublishDiagnosticsParams, String> {
        let document = self.documents.get(uri.as_str());
        let resource = self.workspace.get(uri);
        let source = document
            .map(|document| document.request.source.as_ref())
            .or_else(|| resource.map(|resource| resource.text().as_ref()));
        let Some(source) = source else {
            return Ok(lsp::PublishDiagnosticsParams::new(
                uri.clone(),
                Vec::new(),
                None,
            ));
        };
        let source_document = SourceDocument::new(source).map_err(|error| error.to_string())?;
        let version = self
            .client
            .diagnostic_version
            .then(|| document.map(|document| revision_version_i32(&document.request.revision)))
            .flatten();
        let mut diagnostics = document
            .and_then(|document| {
                if document
                    .workspace_problem
                    .as_ref()
                    .is_some_and(|problem| problem.code == "workspace-input-error")
                {
                    None
                } else {
                    document.view.as_ref().map(|view| view.root.as_ref())
                }
            })
            .iter()
            .flat_map(|analysis| analysis.diagnostics().iter())
            .map(|diagnostic| {
                Ok(lsp::Diagnostic {
                    range: range_to_lsp(
                        diagnostic.range,
                        &source_document,
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
        if let Some(error) = &self.workspace_error {
            diagnostics.push(lsp::Diagnostic {
                range: lsp::Range::default(),
                severity: Some(lsp::DiagnosticSeverity::ERROR),
                code: Some(lsp::NumberOrString::String(
                    "workspace-resource-error".to_owned(),
                )),
                source: Some("adocweave-project".to_owned()),
                message: error.clone(),
                ..lsp::Diagnostic::default()
            });
        }
        for workspace in self.documents.workspace_analyses() {
            let current_version = workspace.resource_versions.get(uri.as_str()).copied();
            let is_root = workspace
                .analysis
                .source_id()
                .is_some_and(|source_id| source_id.as_str() == uri.as_str());
            if is_root {
                continue;
            }
            if current_version
                != document
                    .map(|document| document.request.revision.version)
                    .or_else(|| resource.map(|resource| resource.revision().get()))
            {
                continue;
            }
            // Reading the map here is intentional: the projection and its source map are one
            // adopted snapshot and must never be mixed with a later workspace generation.
            let _source_map = workspace.document.source_map();
            for projected in &workspace.projection.diagnostics {
                for origin in &projected.origins {
                    if origin
                        .source_id
                        .as_ref()
                        .is_none_or(|source_id| source_id.as_str() != uri.as_str())
                    {
                        continue;
                    }
                    diagnostics.push(lsp::Diagnostic {
                        range: range_to_lsp(
                            origin.range.text_range(),
                            &source_document,
                            self.position_encoding,
                        )?,
                        severity: Some(match projected.diagnostic.severity {
                            Severity::Error => lsp::DiagnosticSeverity::ERROR,
                            Severity::Warning => lsp::DiagnosticSeverity::WARNING,
                            Severity::Information => lsp::DiagnosticSeverity::INFORMATION,
                            Severity::Hint => lsp::DiagnosticSeverity::HINT,
                        }),
                        code: Some(lsp::NumberOrString::String(
                            projected.diagnostic.code.as_str().to_owned(),
                        )),
                        source: Some("adocweave".to_owned()),
                        message: projected.diagnostic.message.clone(),
                        ..lsp::Diagnostic::default()
                    });
                }
            }
        }
        for problem in self.documents.workspace_problems() {
            if problem.source_id.as_deref() != Some(uri.as_str()) {
                continue;
            }
            diagnostics.push(lsp::Diagnostic {
                range: range_to_lsp(problem.range, &source_document, self.position_encoding)?,
                severity: Some(lsp::DiagnosticSeverity::ERROR),
                code: Some(lsp::NumberOrString::String(problem.code.clone())),
                source: Some("adocweave-project".to_owned()),
                message: problem.message.clone(),
                ..lsp::Diagnostic::default()
            });
        }
        diagnostics.sort_by(|left, right| {
            (
                left.range.start.line,
                left.range.start.character,
                left.range.end.line,
                left.range.end.character,
                &left.message,
            )
                .cmp(&(
                    right.range.start.line,
                    right.range.start.character,
                    right.range.end.line,
                    right.range.end.character,
                    &right.message,
                ))
        });
        diagnostics.dedup_by(|left, right| {
            left.range == right.range && left.code == right.code && left.message == right.message
        });
        Ok(lsp::PublishDiagnosticsParams::new(
            uri.clone(),
            diagnostics,
            version,
        ))
    }

    pub fn document_symbols(
        &self,
        uri: &lsp::Url,
    ) -> Result<Option<lsp::DocumentSymbolResponse>, String> {
        let presentation = if self.client.hierarchical_document_symbols {
            SymbolPresentation::Hierarchical
        } else {
            SymbolPresentation::Flat
        };
        let Some(document) = self.documents.snapshot(uri.as_str()) else {
            return Ok(Some(match presentation {
                SymbolPresentation::Hierarchical => lsp::DocumentSymbolResponse::Nested(Vec::new()),
                SymbolPresentation::Flat => lsp::DocumentSymbolResponse::Flat(Vec::new()),
            }));
        };
        crate::document_symbols::symbols(
            &document.analysis,
            uri,
            self.position_encoding,
            presentation,
        )
        .map(Some)
    }

    pub fn code_actions(
        &self,
        uri: &lsp::Url,
        range: lsp::Range,
        context: &lsp::CodeActionContext,
    ) -> Result<Option<Vec<lsp::CodeActionOrCommand>>, String> {
        if !self.client.code_action_quickfix
            || !code_action_kind_requested(context.only.as_deref(), &lsp::CodeActionKind::QUICKFIX)
        {
            return Ok(Some(Vec::new()));
        }
        let Some(document) = self.documents.snapshot(uri.as_str()) else {
            return Ok(Some(Vec::new()));
        };
        let mut actions = Vec::new();
        for diagnostic in document.analysis.diagnostics() {
            let diagnostic_range = range_to_lsp(
                diagnostic.range,
                document.analysis.source_document(),
                self.position_encoding,
            )?;
            if !ranges_intersect(range, diagnostic_range) {
                continue;
            }
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
                let edit = if self.client.versioned_document_changes {
                    lsp::WorkspaceEdit {
                        document_changes: Some(lsp::DocumentChanges::Edits(vec![
                            lsp::TextDocumentEdit {
                                text_document: lsp::OptionalVersionedTextDocumentIdentifier {
                                    uri: uri.clone(),
                                    version: Some(revision_version_i32(&document.revision)),
                                },
                                edits,
                            },
                        ])),
                        ..lsp::WorkspaceEdit::default()
                    }
                } else {
                    lsp::WorkspaceEdit {
                        changes: Some(std::collections::HashMap::from([(
                            uri.clone(),
                            edits
                                .into_iter()
                                .map(|edit| match edit {
                                    lsp::OneOf::Left(edit) => edit,
                                    lsp::OneOf::Right(_) => {
                                        unreachable!("AdocWeave emits plain text edits")
                                    }
                                })
                                .collect(),
                        )])),
                        ..lsp::WorkspaceEdit::default()
                    }
                };
                actions.push(lsp::CodeActionOrCommand::CodeAction(lsp::CodeAction {
                    title: fix.title.clone(),
                    kind: Some(lsp::CodeActionKind::QUICKFIX),
                    edit: Some(edit),
                    is_preferred: self
                        .client
                        .code_action_is_preferred
                        .then_some(fix.applicability == Applicability::Always),
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
        let output = formatter::format_analysis(&document.analysis, &document.format)
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
        let offset = request_offset(
            document.analysis.source_document(),
            position,
            self.position_encoding,
        )?;
        if let Some(attribute) = document
            .analysis
            .document_attribute_occurrences()
            .iter()
            .find(|attribute| range_contains_offset(attribute.range, offset))
        {
            return make_hover(
                format!(
                    "**document attribute**  \nName: `{}`\n\nSource value:\n\n    {}\n\nFolded value:\n\n    {}",
                    attribute.name,
                    attribute.value.source_text.replace('\n', "\n    "),
                    attribute.value.folded_text.replace('\n', "\n    ")
                ),
                attribute.range,
                &document,
                self.position_encoding,
                self.client.hover,
            );
        }
        for workspace in self.documents.workspace_analyses() {
            if let Some((reference, origin)) =
                projected_attribute_reference_at(workspace, uri, offset)
            {
                return make_hover(
                    attribute_reference_hover(reference),
                    origin.range.text_range(),
                    &document,
                    self.position_encoding,
                    self.client.hover,
                );
            }
        }
        if let Some(reference) = document
            .analysis
            .attribute_references()
            .iter()
            .find(|reference| range_contains_offset(reference.range, offset))
        {
            return make_hover(
                attribute_reference_hover(reference),
                reference.range,
                &document,
                self.position_encoding,
                self.client.hover,
            );
        }
        if let Some(target) = document.analysis.reference_targets().iter().find(|target| {
            range_contains_offset(target.id_range, offset)
                && !document.analysis.document().blocks().iter().any(|block| {
                    matches!(
                        block,
                        parser::Block::Heading(heading)
                            if heading.text_range == target.id_range
                    )
                })
        }) {
            return make_hover(
                format!("**reference target**  \nID: `{}`", target.id),
                target.id_range,
                &document,
                self.position_encoding,
                self.client.hover,
            );
        }
        if let Some((value, range)) = inline_hover(document.analysis.document(), offset) {
            return make_hover(
                value,
                range,
                &document,
                self.position_encoding,
                self.client.hover,
            );
        }
        if let Some((value, range)) = block_presentation_hover(document.analysis.document(), offset)
        {
            return make_hover(
                value,
                range,
                &document,
                self.position_encoding,
                self.client.hover,
            );
        }
        for author in &document.analysis.document().header().authors {
            if range_contains_offset(author.range, offset) {
                let value = author.email.as_ref().map_or_else(
                    || format!("**author**  \nName: `{}`", author.name),
                    |email| format!("**author**  \nName: `{}`  \nEmail: `{email}`", author.name),
                );
                return make_hover(
                    value,
                    author.range,
                    &document,
                    self.position_encoding,
                    self.client.hover,
                );
            }
        }
        if let Some(revision) = &document.analysis.document().header().revision
            && range_contains_offset(revision.range, offset)
        {
            return make_hover(
                "**document revision**".to_owned(),
                revision.range,
                &document,
                self.position_encoding,
                self.client.hover,
            );
        }
        let Some(element) = document_element_at(document.analysis.document(), offset) else {
            return Ok(None);
        };
        let metadata_hover = match element {
            DocumentElement::MetadataTitle(value) => {
                Some(("block title", value.value.as_str(), value.range))
            }
            DocumentElement::MetadataId(value) => {
                Some(("block ID", value.value.as_str(), value.range))
            }
            DocumentElement::MetadataRole(value) => {
                Some(("block role", value.value.as_str(), value.range))
            }
            DocumentElement::MetadataOption(value) => {
                Some(("block option", value.value.as_str(), value.range))
            }
            DocumentElement::ElementAttribute(attribute) => Some((
                attribute.name.as_deref().unwrap_or("positional attribute"),
                attribute.value.as_str(),
                attribute.range,
            )),
            _ => None,
        };
        if let Some((kind, value, range)) = metadata_hover {
            return make_hover(
                format!("**{kind}**  \nValue: `{value}`"),
                range,
                &document,
                self.position_encoding,
                self.client.hover,
            );
        }
        let (heading, range, part) = match element {
            DocumentElement::HeadingMarker(heading) => (heading, heading.marker_range, "marker"),
            DocumentElement::HeadingText(heading) => (heading, heading.text_range, "text"),
            DocumentElement::SourceLanguage(_) | DocumentElement::SourceAttribute(_) => {
                return Ok(None);
            }
            DocumentElement::MetadataTitle(_)
            | DocumentElement::MetadataId(_)
            | DocumentElement::MetadataRole(_)
            | DocumentElement::MetadataOption(_)
            | DocumentElement::ElementAttribute(_) => unreachable!(),
        };
        let id = generate_heading_ids(document.analysis.document())
            .iter()
            .find(|candidate| candidate.range == heading.text_range)
            .map(|candidate| candidate.id.clone())
            .unwrap_or_else(|| "_section".to_owned());
        let level = match heading.kind {
            parser::HeadingKind::DocumentTitle => "document title".to_owned(),
            parser::HeadingKind::Part => "book part".to_owned(),
            parser::HeadingKind::Section { level } => format!("section level {level}"),
            parser::HeadingKind::Discrete { level } => format!("discrete heading level {level}"),
        };
        make_hover(
            format!("**{level}**  \nGenerated ID: `{id}`  \nPart: {part}"),
            range,
            &document,
            self.position_encoding,
            self.client.hover,
        )
    }

    pub fn completion(
        &self,
        uri: &lsp::Url,
        position: lsp::Position,
    ) -> Result<Option<lsp::CompletionResponse>, String> {
        let Some(document) = self.documents.snapshot(uri.as_str()) else {
            return Ok(Some(lsp::CompletionResponse::Array(Vec::new())));
        };
        let offset = request_offset(
            document.analysis.source_document(),
            position,
            self.position_encoding,
        )?;
        if attribute_completion_context(document.analysis.source(), offset as usize) {
            let values =
                self.documents
                    .workspace_analyses()
                    .find_map(|workspace| {
                        expanded_offset_for_origin(workspace, uri, offset).map(|expanded| {
                            workspace
                                .analysis
                                .attribute_environment()
                                .values_at(expanded)
                        })
                    })
                    .unwrap_or_else(|| {
                        document.analysis.attribute_environment().values_at(
                            adocweave::text::TextSize::new(offset as usize).expect("offset"),
                        )
                    });
            let items = values
                .into_iter()
                .map(|(name, value)| lsp::CompletionItem {
                    label: name,
                    detail: Some(value),
                    kind: Some(lsp::CompletionItemKind::VARIABLE),
                    ..lsp::CompletionItem::default()
                })
                .collect();
            return Ok(Some(lsp::CompletionResponse::Array(items)));
        }
        if document
            .analysis
            .references()
            .iter()
            .any(|reference| cursor_touches_range(reference.target_range, offset))
        {
            let items = document
                .analysis
                .reference_targets()
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
        let Some(element) = document_element_at(document.analysis.document(), offset) else {
            return Ok(Some(lsp::CompletionResponse::Array(Vec::new())));
        };
        let metadata_candidates: Option<(&[&str], lsp::CompletionItemKind)> = match element {
            DocumentElement::MetadataRole(_) => {
                Some((&["lead", "discrete"], lsp::CompletionItemKind::VALUE))
            }
            DocumentElement::MetadataOption(_) => Some((
                &[
                    "autowidth",
                    "collapsible",
                    "footer",
                    "header",
                    "interactive",
                    "nowrap",
                ],
                lsp::CompletionItemKind::VALUE,
            )),
            DocumentElement::ElementAttribute(_) => Some((
                &[
                    "CAUTION",
                    "IMPORTANT",
                    "NOTE",
                    "TIP",
                    "WARNING",
                    "cols",
                    "frame",
                    "grid",
                    "id",
                    "options",
                    "quote",
                    "role",
                    "stripes",
                    "subs",
                    "verse",
                    "width",
                ],
                lsp::CompletionItemKind::PROPERTY,
            )),
            DocumentElement::MetadataTitle(_) | DocumentElement::MetadataId(_) => {
                return Ok(Some(lsp::CompletionResponse::Array(Vec::new())));
            }
            _ => None,
        };
        if let Some((candidates, kind)) = metadata_candidates {
            let items = candidates
                .iter()
                .map(|candidate| lsp::CompletionItem {
                    label: (*candidate).to_owned(),
                    kind: Some(kind),
                    ..lsp::CompletionItem::default()
                })
                .collect();
            return Ok(Some(lsp::CompletionResponse::Array(items)));
        }
        let source = match element {
            DocumentElement::SourceLanguage(source) | DocumentElement::SourceAttribute(source) => {
                source
            }
            DocumentElement::HeadingMarker(_) | DocumentElement::HeadingText(_) => {
                return Ok(Some(lsp::CompletionResponse::Array(Vec::new())));
            }
            DocumentElement::MetadataTitle(_)
            | DocumentElement::MetadataId(_)
            | DocumentElement::MetadataRole(_)
            | DocumentElement::MetadataOption(_)
            | DocumentElement::ElementAttribute(_) => unreachable!(),
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
            .iter()
            .map(|language| lsp::CompletionItem {
                label: language.to_string(),
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
        let offset = request_offset(
            document.analysis.source_document(),
            position,
            self.position_encoding,
        )?;
        for workspace in self.documents.workspace_analyses() {
            if let Some((reference, _)) = projected_attribute_reference_at(workspace, uri, offset)
                && let Some(binding_id) = reference.binding_id
                && let Some(binding) = workspace
                    .projection
                    .attribute_bindings
                    .iter()
                    .find(|binding| binding.value.id() == binding_id)
                && let Some(origin) = binding.name_origins.first()
            {
                return Ok(Some(lsp::GotoDefinitionResponse::Scalar(
                    self.attribute_origin_location(origin)?,
                )));
            }
        }
        if let Some(reference) = document
            .analysis
            .attribute_references()
            .iter()
            .find(|reference| range_contains_offset(reference.range, offset))
            && let Some(binding) = reference
                .binding_id
                .and_then(|id| document.analysis.attribute_environment().binding(id))
        {
            return Ok(Some(lsp::GotoDefinitionResponse::Scalar(
                lsp::Location::new(
                    uri.clone(),
                    range_to_lsp(
                        binding.occurrence().name_range,
                        document.analysis.source_document(),
                        self.position_encoding,
                    )?,
                ),
            )));
        }
        for workspace in self.documents.workspace_analyses() {
            if let Some(directive) = workspace.projection.directives.iter().find(|directive| {
                directive
                    .source_id
                    .as_ref()
                    .is_some_and(|source_id| source_id.as_str() == uri.as_str())
                    && range_contains_offset(directive.target_range, offset)
            }) && let Some(target) = directive.resource_source_id.as_ref()
            {
                let target: lsp::Url = target
                    .as_str()
                    .parse()
                    .map_err(|error| format!("invalid include resource URI: {error}"))?;
                return Ok(Some(lsp::GotoDefinitionResponse::Scalar(
                    lsp::Location::new(target, lsp::Range::default()),
                )));
            }
        }
        let Some(reference) = document
            .analysis
            .references()
            .iter()
            .find(|reference| range_contains_offset(reference.range, offset))
        else {
            return Ok(None);
        };
        let Some(key) = reference.target.clone() else {
            return Ok(None);
        };
        if let Some(identity) = reference_identity(uri, reference.target.as_ref())
            && let Some(location) =
                self.target_location(&identity.uri, identity.anchor.as_deref())?
        {
            return Ok(Some(lsp::GotoDefinitionResponse::Scalar(location)));
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
        let offset = request_offset(
            document.analysis.source_document(),
            position,
            self.position_encoding,
        )?;
        let projected_binding_origin = self.documents.workspace_analyses().find_map(|workspace| {
            let binding_id = projected_attribute_reference_at(workspace, uri, offset)
                .and_then(|(reference, _)| reference.binding_id)
                .or_else(|| projected_attribute_binding_at(workspace, uri, offset))?;
            workspace
                .projection
                .attribute_bindings
                .iter()
                .find(|binding| binding.value.id() == binding_id)?
                .name_origins
                .first()
                .cloned()
        });
        if let Some(binding_origin) = projected_binding_origin {
            let mut locations = Vec::new();
            for workspace in self.documents.workspace_analyses() {
                let Some(binding) =
                    workspace
                        .projection
                        .attribute_bindings
                        .iter()
                        .find(|binding| {
                            binding
                                .name_origins
                                .iter()
                                .any(|origin| same_origin(origin, &binding_origin))
                        })
                else {
                    continue;
                };
                if include_declaration && let Some(origin) = binding.name_origins.first() {
                    locations.push(self.attribute_origin_location(origin)?);
                }
                for reference in &workspace.projection.attribute_references {
                    if reference.value.binding_id != Some(binding.value.id()) {
                        continue;
                    }
                    for origin in &reference.name_origins {
                        locations.push(self.attribute_origin_location(origin)?);
                    }
                }
            }
            locations.sort_by(|left, right| {
                (
                    left.uri.as_str(),
                    left.range.start.line,
                    left.range.start.character,
                    left.range.end.line,
                    left.range.end.character,
                )
                    .cmp(&(
                        right.uri.as_str(),
                        right.range.start.line,
                        right.range.start.character,
                        right.range.end.line,
                        right.range.end.character,
                    ))
            });
            locations.dedup();
            return Ok(Some(locations));
        }
        let local_binding_id = document
            .analysis
            .attribute_references()
            .iter()
            .find(|reference| range_contains_offset(reference.range, offset))
            .and_then(|reference| reference.binding_id)
            .or_else(|| {
                document
                    .analysis
                    .attribute_environment()
                    .bindings()
                    .iter()
                    .find(|binding| range_contains_offset(binding.occurrence().name_range, offset))
                    .map(adocweave::semantic::AttributeBinding::id)
            });
        if let Some(binding_id) = local_binding_id {
            let mut locations = Vec::new();
            if include_declaration
                && let Some(binding) = document
                    .analysis
                    .attribute_environment()
                    .binding(binding_id)
            {
                locations.push(lsp::Location::new(
                    uri.clone(),
                    range_to_lsp(
                        binding.occurrence().name_range,
                        document.analysis.source_document(),
                        self.position_encoding,
                    )?,
                ));
            }
            for reference in document.analysis.attribute_references() {
                if reference.binding_id == Some(binding_id) {
                    locations.push(lsp::Location::new(
                        uri.clone(),
                        range_to_lsp(
                            reference.name_range,
                            document.analysis.source_document(),
                            self.position_encoding,
                        )?,
                    ));
                }
            }
            return Ok(Some(locations));
        }
        let reference_at_position = document
            .analysis
            .references()
            .iter()
            .find(|reference| range_contains_offset(reference.range, offset));
        let key = reference_at_position
            .and_then(|reference| reference.target.clone())
            .or_else(|| {
                document
                    .analysis
                    .reference_targets()
                    .iter()
                    .find(|target| range_contains_offset(target.id_range, offset))
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
            .and_then(|reference| reference_identity(uri, reference.target.as_ref()))
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
        if include_declaration
            && let Some(location) =
                self.target_location(&identity.uri, identity.anchor.as_deref())?
        {
            locations.push(location);
        }
        for candidate in self.documents.snapshots() {
            let candidate_uri: lsp::Url = candidate
                .uri
                .parse()
                .map_err(|error| format!("invalid open document URI {}: {error}", candidate.uri))?;
            for reference in candidate.analysis.references() {
                if reference_identity(&candidate_uri, reference.target.as_ref()).as_ref()
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
        for workspace in self.documents.workspace_analyses() {
            for reference in &workspace.projection.references {
                let Some(source_origin) = reference.origins.first() else {
                    continue;
                };
                let Some(source_id) = &source_origin.source_id else {
                    continue;
                };
                let source_uri: lsp::Url = source_id
                    .as_str()
                    .parse()
                    .map_err(|error| format!("invalid projected reference URI: {error}"))?;
                if reference_identity(&source_uri, reference.value.target.as_ref()).as_ref()
                    != Some(&identity)
                {
                    continue;
                }
                let Some(target_origin) = reference
                    .target_origins
                    .iter()
                    .find(|origin| origin.source_id.as_ref() == Some(source_id))
                else {
                    continue;
                };
                let source_document = self.source_document(&source_uri)?;
                locations.push(lsp::Location::new(
                    source_uri,
                    range_to_lsp(
                        target_origin.range.text_range(),
                        &source_document,
                        self.position_encoding,
                    )?,
                ));
            }
        }
        locations.sort_by(|left, right| {
            (
                left.uri.as_str(),
                left.range.start.line,
                left.range.start.character,
                left.range.end.line,
                left.range.end.character,
            )
                .cmp(&(
                    right.uri.as_str(),
                    right.range.start.line,
                    right.range.start.character,
                    right.range.end.line,
                    right.range.end.character,
                ))
        });
        locations.dedup();
        Ok(Some(locations))
    }

    pub fn rename(
        &self,
        uri: &lsp::Url,
        position: lsp::Position,
        new_name: &str,
    ) -> Result<Option<lsp::WorkspaceEdit>, String> {
        if !valid_anchor_name(new_name) {
            return Ok(None);
        }
        let Some(document) = self.documents.snapshot(uri.as_str()) else {
            return Ok(None);
        };
        let offset = request_offset(
            document.analysis.source_document(),
            position,
            self.position_encoding,
        )?;
        let Some(target) = document
            .analysis
            .reference_targets()
            .iter()
            .find(|target| range_contains_offset(target.id_range, offset))
        else {
            return Ok(None);
        };
        let key = ReferenceKey::Local {
            anchor: target.id.clone(),
        };
        let host_request = host_reference_request(&document, uri, key, self.position_encoding);
        let locations = if let Some(locations) = self.host_index.references(&host_request, true)? {
            locations
        } else {
            self.references(uri, position, true)?.unwrap_or_default()
        };
        if locations.is_empty() {
            return Ok(None);
        }
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
        for link in project(
            &document.analysis,
            &adocweave::resolution::RenderInputs::default(),
        )
        .external_links
        {
            if !adocweave::resolution::AuthoredUrlPolicy::default().allows(&link.target) {
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
                tooltip: self
                    .client
                    .document_link_tooltip
                    .then(|| "外部リンクを開く".to_owned()),
                data: None,
            });
        }
        for reference in document.analysis.references() {
            let target = if let Some(identity) = reference_identity(uri, reference.target.as_ref())
            {
                let mut target = identity.uri;
                target.set_fragment(identity.anchor.as_deref());
                Some(target)
            } else if let Some(key) = reference.target.clone() {
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
                tooltip: self
                    .client
                    .document_link_tooltip
                    .then(|| "参照先を開く".to_owned()),
                data: None,
            });
        }
        for workspace in self.documents.workspace_analyses() {
            for directive in &workspace.projection.directives {
                if directive
                    .source_id
                    .as_ref()
                    .is_none_or(|source_id| source_id.as_str() != uri.as_str())
                {
                    continue;
                }
                let Some(target) = directive.resource_source_id.as_ref() else {
                    continue;
                };
                let Ok(target) = target.as_str().parse() else {
                    continue;
                };
                links.push(lsp::DocumentLink {
                    range: range_to_lsp(
                        directive.target_range,
                        document.analysis.source_document(),
                        self.position_encoding,
                    )?,
                    target: Some(target),
                    tooltip: self
                        .client
                        .document_link_tooltip
                        .then(|| "include先を開く".to_owned()),
                    data: None,
                });
            }
        }
        links.sort_by_key(|link| {
            (
                link.range.start.line,
                link.range.start.character,
                link.range.end.line,
                link.range.end.character,
            )
        });
        links.dedup_by(|left, right| left.range == right.range && left.target == right.target);
        Ok(Some(links))
    }

    fn source_document(&self, uri: &lsp::Url) -> Result<SourceDocument, String> {
        let source = self
            .documents
            .get(uri.as_str())
            .map(|document| document.request.source.as_ref())
            .or_else(|| {
                self.workspace
                    .get(uri)
                    .map(|resource| resource.text().as_ref())
            })
            .ok_or_else(|| format!("projected source is missing: {uri}"))?;
        SourceDocument::new(source).map_err(|error| error.to_string())
    }

    fn attribute_origin_location(
        &self,
        origin: &adocweave::preprocess::SourceOrigin,
    ) -> Result<lsp::Location, String> {
        let source_id = origin
            .source_id
            .as_ref()
            .ok_or_else(|| "attribute origin has no source ID".to_owned())?;
        let uri: lsp::Url = source_id
            .as_str()
            .parse()
            .map_err(|error| format!("invalid attribute origin URI: {error}"))?;
        let source_document = self.source_document(&uri)?;
        Ok(lsp::Location::new(
            uri,
            range_to_lsp(
                origin.range.text_range(),
                &source_document,
                self.position_encoding,
            )?,
        ))
    }

    pub fn semantic_tokens(
        &self,
        uri: &lsp::Url,
    ) -> Result<Option<lsp::SemanticTokensResult>, String> {
        if !self.client.semantic_tokens_full {
            return Ok(None);
        }
        let Some(document) = self.documents.snapshot(uri.as_str()) else {
            return Ok(Some(lsp::SemanticTokensResult::Tokens(
                lsp::SemanticTokens {
                    result_id: None,
                    data: Vec::new(),
                },
            )));
        };
        Ok(Some(lsp::SemanticTokensResult::Tokens(
            crate::semantic_tokens::tokens(&document.analysis, self.position_encoding)?,
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
                    .reference_targets()
                    .iter()
                    .find(|target| target.id == anchor)
            })
            .or_else(|| document.analysis.reference_targets().first());
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

fn make_hover(
    value: String,
    range: CoreTextRange,
    document: &DocumentSnapshot,
    encoding: PositionEncoding,
    presentation: HoverPresentation,
) -> Result<Option<lsp::Hover>, String> {
    let contents = match presentation {
        HoverPresentation::Markdown => lsp::HoverContents::Markup(lsp::MarkupContent {
            kind: lsp::MarkupKind::Markdown,
            value,
        }),
        HoverPresentation::PlainText => lsp::HoverContents::Markup(lsp::MarkupContent {
            kind: lsp::MarkupKind::PlainText,
            value: hover_plain_text(&value),
        }),
        HoverPresentation::Legacy => {
            lsp::HoverContents::Scalar(lsp::MarkedString::String(hover_plain_text(&value)))
        }
    };
    Ok(Some(lsp::Hover {
        contents,
        range: Some(range_to_lsp(
            range,
            document.analysis.source_document(),
            encoding,
        )?),
    }))
}

fn code_action_kind_requested(
    only: Option<&[lsp::CodeActionKind]>,
    offered: &lsp::CodeActionKind,
) -> bool {
    only.is_none_or(|requested| {
        requested.iter().any(|kind| {
            offered == kind
                || offered
                    .as_str()
                    .strip_prefix(kind.as_str())
                    .is_some_and(|suffix| suffix.starts_with('.'))
        })
    })
}

fn hover_plain_text(markdown: &str) -> String {
    markdown
        .replace("  \n", "\n")
        .replace("**", "")
        .replace('`', "")
}

fn attribute_reference_hover(reference: &adocweave::semantic::AttributeReference) -> String {
    match &reference.value {
        Ok(Some(value)) => format!(
            "**attribute reference**  \nName: `{}`  \nValue: `{}`",
            reference.name, value
        ),
        Ok(None) => format!(
            "**attribute reference**  \nName: `{}`  \nValue: _unset_",
            reference.name
        ),
        Err(error) => format!(
            "**attribute reference**  \nName: `{}`  \nResolution: `{}`",
            reference.name,
            match error {
                adocweave::semantic::AttributeExpansionError::Undefined => "undefined",
                adocweave::semantic::AttributeExpansionError::Cycle => "cycle",
                adocweave::semantic::AttributeExpansionError::DepthLimitExceeded => "depth limit",
                adocweave::semantic::AttributeExpansionError::SizeLimitExceeded => "size limit",
            }
        ),
    }
}

fn projected_attribute_reference_at<'a>(
    workspace: &'a DocumentWorkspaceAnalysis,
    uri: &lsp::Url,
    offset: u32,
) -> Option<(
    &'a adocweave::semantic::AttributeReference,
    &'a adocweave::preprocess::SourceOrigin,
)> {
    workspace
        .projection
        .attribute_references
        .iter()
        .find_map(|reference| {
            reference
                .origins
                .iter()
                .find(|origin| {
                    origin
                        .source_id
                        .as_ref()
                        .is_some_and(|source_id| source_id.as_str() == uri.as_str())
                        && range_contains_offset(origin.range.text_range(), offset)
                })
                .map(|origin| (&reference.value, origin))
        })
}

fn projected_attribute_binding_at(
    workspace: &DocumentWorkspaceAnalysis,
    uri: &lsp::Url,
    offset: u32,
) -> Option<adocweave::semantic::AttributeBindingId> {
    workspace
        .projection
        .attribute_bindings
        .iter()
        .find(|binding| {
            binding.name_origins.iter().any(|origin| {
                origin
                    .source_id
                    .as_ref()
                    .is_some_and(|source_id| source_id.as_str() == uri.as_str())
                    && range_contains_offset(origin.range.text_range(), offset)
            })
        })
        .map(|binding| binding.value.id())
}

fn same_origin(
    left: &adocweave::preprocess::SourceOrigin,
    right: &adocweave::preprocess::SourceOrigin,
) -> bool {
    left.source_id == right.source_id && left.range == right.range
}

fn expanded_offset_for_origin(
    workspace: &DocumentWorkspaceAnalysis,
    uri: &lsp::Url,
    offset: u32,
) -> Option<adocweave::text::TextSize> {
    workspace.document.source_map().iter().find_map(|segment| {
        if segment.mapping != adocweave::preprocess::SourceMapping::Identity
            || segment
                .origin
                .source_id
                .as_ref()
                .is_none_or(|source_id| source_id.as_str() != uri.as_str())
        {
            return None;
        }
        let origin = segment.origin.range.text_range();
        if !(origin.start().to_u32() <= offset && offset <= origin.end().to_u32()) {
            return None;
        }
        let relative = offset.checked_sub(origin.start().to_u32())?;
        adocweave::text::TextSize::new(segment.output_range.start().to_usize() + relative as usize)
            .ok()
    })
}

fn attribute_completion_context(source: &str, offset: usize) -> bool {
    if offset > source.len() || !source.is_char_boundary(offset) {
        return false;
    }
    let line_start = source[..offset].rfind('\n').map_or(0, |index| index + 1);
    let bytes = &source.as_bytes()[line_start..offset];
    let mut open = None;
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'\\' && bytes.get(index + 1) == Some(&b'{') {
            index += 2;
            continue;
        }
        match bytes[index] {
            b'{' => open = Some(index),
            b'}' => open = None,
            _ => {}
        }
        index += 1;
    }
    open.is_some_and(|open| {
        bytes[open + 1..]
            .iter()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.'))
    })
}

fn inline_hover(
    document: &adocweave::semantic::Document,
    offset: u32,
) -> Option<(String, CoreTextRange)> {
    let mut found = None;
    adocweave::semantic::walk(document, |node| {
        let adocweave::semantic::SemanticNode::Inline(inline) = node else {
            return;
        };
        if range_contains_offset(inline.range(), offset) {
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
                Inline::Passthrough { value, .. } => {
                    Some(format!("**passthrough**  \nLiteral content: `{value}`"))
                }
                Inline::Macro(node) => match node.kind {
                    adocweave::semantic::StandardMacroKind::Footnote => document
                        .catalogs()
                        .footnote_occurrence(node.range)
                        .map(|(footnote, _)| {
                            format!(
                                "**footnote {}**  \nID: `{}`  \nText: `{}`",
                                footnote.number,
                                footnote.id.as_deref().unwrap_or("anonymous"),
                                footnote.text
                            )
                        }),
                    adocweave::semantic::StandardMacroKind::BibliographyAnchor => document
                        .catalogs()
                        .bibliography()
                        .iter()
                        .find(|entry| entry.definition_range == node.range)
                        .map(|entry| {
                            format!(
                                "**bibliography entry**  \nID: `{}`  \nReferences: {}",
                                entry.id,
                                entry.references.len()
                            )
                        }),
                    adocweave::semantic::StandardMacroKind::IndexTerm => document
                        .catalogs()
                        .index()
                        .iter()
                        .find(|entry| entry.occurrences.contains(&node.range))
                        .map(|entry| {
                            format!("**index term**  \nPath: `{}`", entry.terms.join(" > "))
                        }),
                    _ => Some(format!(
                        "**{:?} macro**  \nTarget: `{}`",
                        node.kind, node.target
                    )),
                },
                Inline::Text(_)
                | Inline::Literal { .. }
                | Inline::Styled { .. }
                | Inline::HardBreak { .. } => None,
            };
            if let Some(value) = value {
                found = Some((value, inline.range()));
            }
        }
    });
    found
}

fn block_presentation_hover(
    document: &adocweave::semantic::Document,
    offset: u32,
) -> Option<(String, CoreTextRange)> {
    let mut found = None;
    adocweave::semantic::walk(document, |node| {
        let adocweave::semantic::SemanticNode::Block(block) = node else {
            return;
        };
        match block {
            parser::Block::Paragraph(value)
                if value
                    .admonition
                    .as_ref()
                    .is_some_and(|item| range_contains_offset(item.label_range, offset)) =>
            {
                let item = value.admonition.as_ref().expect("guarded admonition");
                found = Some((
                    format!("**{} admonition**", item.kind.label()),
                    item.label_range,
                ));
            }
            parser::Block::Delimited(value) => match &value.presentation {
                Some(parser::DelimitedPresentation::Admonition(item))
                    if range_contains_offset(item.label_range, offset) =>
                {
                    found = Some((
                        format!("**{} admonition**", item.kind.label()),
                        item.label_range,
                    ));
                }
                Some(parser::DelimitedPresentation::Quote(item))
                    if range_contains_offset(
                        value.metadata.range.unwrap_or(value.range),
                        offset,
                    ) =>
                {
                    let kind = match item.kind {
                        parser::QuoteKind::Quote => "quote",
                        parser::QuoteKind::Verse => "verse",
                    };
                    found = Some((
                        format!(
                            "**{kind} block**  \nAttribution: `{}`  \nCitation: `{}`",
                            item.attribution.as_ref().map_or("", |value| &value.value),
                            item.citation.as_ref().map_or("", |value| &value.value)
                        ),
                        value.metadata.range.unwrap_or(value.range),
                    ));
                }
                _ => {}
            },
            _ => {}
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
    destination: Option<&ReferenceKey>,
) -> Option<TargetIdentity> {
    match destination {
        Some(ReferenceKey::Local { anchor }) => Some(TargetIdentity {
            uri: source_uri.clone(),
            anchor: Some(anchor.clone()),
        }),
        Some(ReferenceKey::Document { document, anchor }) => {
            source_uri.join(document).ok().map(|uri| TargetIdentity {
                uri,
                anchor: anchor.clone(),
            })
        }
        Some(ReferenceKey::Scheme { .. }) | None => None,
    }
}

fn host_reference_request(
    document: &DocumentSnapshot,
    uri: &lsp::Url,
    target: ReferenceKey,
    encoding: PositionEncoding,
) -> HostReferenceRequest {
    HostReferenceRequest {
        source: uri.clone(),
        source_version: revision_version_i32(&document.revision),
        source_generation: document.revision.generation,
        target,
        encoding,
    }
}

fn revision_version_i32(revision: &adocweave::DocumentRevision) -> i32 {
    i32::try_from(revision.version).expect("LSP document versions originate as i32")
}

fn valid_anchor_name(value: &str) -> bool {
    !value.is_empty()
        && !value.chars().any(|character| {
            character.is_whitespace()
                || character.is_control()
                || matches!(character, '[' | ']' | '<' | '>' | '#')
        })
}
