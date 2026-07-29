//! Runtime-independent language features over owned document analyses.

use std::fmt;
use std::sync::Arc;

use adocweave::output::diagnostics::{RuleSettings, lint_rule};
use adocweave::output::formatter;
use adocweave::resolution::ReferenceKey;
use adocweave::text::SourceDocument;
use async_lsp::lsp_types as lsp;
use serde::Deserialize;

use crate::diagnostics::QuickFixCapabilities;
use crate::document_symbols::SymbolPresentation;
use crate::editing;
use crate::hover::HoverPresentation;
use crate::navigation::{self, NavigationInput};
use crate::position::{PositionEncoding, lsp_position_to_core, negotiate_encoding, request_offset};
use crate::presentation;
use crate::state::DocumentStore;
use crate::state::{
    Adoption, AnalysisJob, DocumentSnapshot, WorkspaceAnalysis as DocumentWorkspaceAnalysis,
    WorkspaceProblem,
};
use crate::workspace::WorkspaceResources;
use crate::{SERVER_NAME, VERSION};

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
                    let start = index
                        .position_to_offset(
                            lsp_position_to_core(range.start),
                            self.position_encoding.core(),
                        )
                        .map_err(|error| error.to_string())?
                        .to_usize();
                    let end = index
                        .position_to_offset(
                            lsp_position_to_core(range.end),
                            self.position_encoding.core(),
                        )
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
                crate::diagnostics::analysis_diagnostic(
                    uri,
                    diagnostic,
                    &source_document,
                    self.position_encoding,
                )
            })
            .collect::<Result<Vec<_>, String>>()?;
        if let Some(error) = &self.workspace_error {
            diagnostics.push(crate::diagnostics::workspace_error(error));
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
                    diagnostics.push(crate::diagnostics::projected_diagnostic(
                        origin.range.text_range(),
                        &projected.diagnostic,
                        &source_document,
                        self.position_encoding,
                    )?);
                }
            }
        }
        for problem in self.documents.workspace_problems() {
            if problem.source_id.as_deref() != Some(uri.as_str()) {
                continue;
            }
            diagnostics.push(crate::diagnostics::project_problem(
                problem.range,
                &problem.code,
                &problem.message,
                &source_document,
                self.position_encoding,
            )?);
        }
        crate::diagnostics::canonicalize(&mut diagnostics);
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
        crate::diagnostics::quick_fixes(
            uri,
            revision_version_i32(&document.revision),
            &document.analysis,
            range,
            self.position_encoding,
            QuickFixCapabilities {
                versioned_document_changes: self.client.versioned_document_changes,
                is_preferred: self.client.code_action_is_preferred,
            },
        )
        .map(Some)
    }

    pub fn formatting(&self, uri: &lsp::Url) -> Result<Option<Vec<lsp::TextEdit>>, String> {
        let Some(document) = self.documents.snapshot(uri.as_str()) else {
            return Ok(Some(Vec::new()));
        };
        editing::formatting(&document.analysis, &document.format, self.position_encoding).map(Some)
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
        crate::hover::hover(
            &document.analysis,
            uri,
            offset,
            self.documents
                .workspace_analyses()
                .map(|workspace| workspace.projection.as_ref()),
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
            return Ok(Some(presentation::empty_completion()));
        };
        let workspaces = self.documents.workspace_analyses().collect::<Vec<_>>();
        presentation::completion(
            &document.analysis,
            &workspaces,
            uri,
            position,
            self.position_encoding,
        )
        .map(Some)
    }

    pub fn definition(
        &self,
        uri: &lsp::Url,
        position: lsp::Position,
    ) -> Result<Option<lsp::GotoDefinitionResponse>, String> {
        let Some(document) = self.documents.snapshot(uri.as_str()) else {
            return Ok(None);
        };
        let snapshots = self.documents.snapshots();
        let workspaces = self.documents.workspace_analyses().collect::<Vec<_>>();
        let source_document = |source_uri: &lsp::Url| self.source_document(source_uri);
        let input = NavigationInput {
            document: &document,
            snapshots: &snapshots,
            workspaces: &workspaces,
            encoding: self.position_encoding,
            source_document: &source_document,
        };
        match navigation::definition(&input, uri, position)? {
            navigation::Definition::Resolved(response) => Ok(response),
            navigation::Definition::Host(target) => {
                let request =
                    host_reference_request(&document, uri, target, self.position_encoding);
                self.host_index
                    .definition(&request)
                    .map(|location| location.map(lsp::GotoDefinitionResponse::Scalar))
            }
        }
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
        let snapshots = self.documents.snapshots();
        let workspaces = self.documents.workspace_analyses().collect::<Vec<_>>();
        let source_document = |source_uri: &lsp::Url| self.source_document(source_uri);
        let input = NavigationInput {
            document: &document,
            snapshots: &snapshots,
            workspaces: &workspaces,
            encoding: self.position_encoding,
            source_document: &source_document,
        };
        let result = navigation::references(&input, uri, position, include_declaration)?;
        if let Some(target) = result.host_target {
            let request = host_reference_request(&document, uri, target, self.position_encoding);
            if let Some(locations) = self.host_index.references(&request, include_declaration)? {
                return Ok(Some(locations));
            }
        }
        Ok(Some(result.fallback))
    }

    pub fn rename(
        &self,
        uri: &lsp::Url,
        position: lsp::Position,
        new_name: &str,
    ) -> Result<Option<lsp::WorkspaceEdit>, String> {
        let Some(document) = self.documents.snapshot(uri.as_str()) else {
            return Ok(None);
        };
        let Some(key) = editing::rename_target(
            &document.analysis,
            position,
            new_name,
            self.position_encoding,
        )?
        else {
            return Ok(None);
        };
        let host_request = host_reference_request(&document, uri, key, self.position_encoding);
        let locations = if let Some(locations) = self.host_index.references(&host_request, true)? {
            locations
        } else {
            self.references(uri, position, true)?.unwrap_or_default()
        };
        Ok(editing::rename_edit(locations, new_name))
    }

    pub fn document_links(&self, uri: &lsp::Url) -> Result<Option<Vec<lsp::DocumentLink>>, String> {
        let Some(document) = self.documents.snapshot(uri.as_str()) else {
            return Ok(Some(Vec::new()));
        };
        let snapshots = self.documents.snapshots();
        let workspaces = self.documents.workspace_analyses().collect::<Vec<_>>();
        let source_document = |source_uri: &lsp::Url| self.source_document(source_uri);
        let input = NavigationInput {
            document: &document,
            snapshots: &snapshots,
            workspaces: &workspaces,
            encoding: self.position_encoding,
            source_document: &source_document,
        };
        let tooltips = self.client.document_link_tooltip;
        let mut links = navigation::document_links(&input, uri, tooltips)?;
        for unresolved in std::mem::take(&mut links.unresolved) {
            let request = host_reference_request(
                &document,
                uri,
                unresolved.target.clone(),
                self.position_encoding,
            );
            let location = self.host_index.definition(&request).ok().flatten();
            links.resolve(unresolved, location, tooltips);
        }
        Ok(Some(links.finish()))
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

    pub fn semantic_tokens(
        &self,
        uri: &lsp::Url,
    ) -> Result<Option<lsp::SemanticTokensResult>, String> {
        if !self.client.semantic_tokens_full {
            return Ok(None);
        }
        let document = self.documents.snapshot(uri.as_str());
        crate::semantic_tokens::response(
            document.as_ref().map(|document| document.analysis.as_ref()),
            self.position_encoding,
        )
        .map(Some)
    }
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
