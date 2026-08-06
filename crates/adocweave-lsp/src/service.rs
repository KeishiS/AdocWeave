//! Runtime-independent language features over owned document analyses.

use std::fmt;
use std::sync::Arc;

use adocweave::CancellationCheck;
use adocweave::output::diagnostics::{RuleSettings, lint_rule};
use adocweave::output::formatter;
use adocweave::resolution::ReferenceKey;
use adocweave::text::SourceDocument;
use async_lsp::lsp_types as lsp;
use serde::Deserialize;

use crate::cancellation::{QueryCancellation, QueryResult};
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
use crate::workspace::{WatchedFileKind, WorkspaceResources};
use crate::{SERVER_NAME, VERSION};

const MAX_WORKSPACE_WATCH_ERRORS: usize = 128;
const MAX_WORKSPACE_WATCH_ERROR_BYTES: usize = 64 * 1024;
const MAX_WORKSPACE_WATCH_CHANGES: usize = 10_000;
const MAX_WORKSPACE_WATCH_URI_BYTES: usize = 1024 * 1024;

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
    rename_prepare_support: bool,
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
            rename_prepare_support: true,
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
            rename_prepare_support: text_document
                .and_then(|capabilities| capabilities.rename.as_ref())
                .and_then(|capabilities| capabilities.prepare_support)
                == Some(true),
            workspace_folders: workspace.and_then(|capabilities| capabilities.workspace_folders)
                == Some(true),
            watched_files_dynamic_registration: workspace
                .and_then(|capabilities| capabilities.did_change_watched_files)
                .and_then(|capabilities| capabilities.dynamic_registration)
                == Some(true),
        }
    }
}

/// A completed read of the workspace roots, waiting to be installed.
///
/// Carries no borrow of the service, so it can be produced on a worker and
/// handed back to the event loop.
#[derive(Clone, Debug)]
pub struct WorkspaceScan {
    loaded: crate::workspace::LoadedRoots,
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
    workspace_watch_errors: std::collections::BTreeMap<String, String>,
    workspace_watch_error_bytes: usize,
    workspace_watch_errors_overflowed: bool,
    workspace_watch_recovery_required: bool,
    workspace_input_error: Option<String>,
}

pub(crate) struct WorkspaceFileChanges {
    pub(crate) jobs: Vec<AnalysisJob>,
    pub(crate) journal: Vec<lsp::FileEvent>,
    /// Whether `journal` can reproduce every change from this notification
    /// after an in-flight workspace snapshot is installed.
    pub(crate) replay_complete: bool,
    pub(crate) recovery_required: bool,
}

pub(crate) struct WorkspaceScanApplication {
    pub(crate) jobs: Vec<AnalysisJob>,
    pub(crate) installed: bool,
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
            workspace_watch_errors: std::collections::BTreeMap::new(),
            workspace_watch_error_bytes: 0,
            workspace_watch_errors_overflowed: false,
            workspace_watch_recovery_required: false,
            workspace_input_error: None,
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

fn parse_open_sources(sources: &[(String, i32, String)]) -> Vec<(lsp::Url, i64, Arc<str>)> {
    sources
        .iter()
        .filter_map(|(uri, version, source)| {
            Some((
                uri.parse().ok()?,
                i64::from(*version),
                Arc::<str>::from(source.as_str()),
            ))
        })
        .collect()
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
        // The roots are recorded here, but reading them is left to
        // `plan_workspace_scan`. Scanning a large workspace takes long enough
        // to be noticed, and the protocol does not require it to finish before
        // the capabilities are returned.
        self.workspace_roots = roots
            .into_iter()
            .map(|uri| (uri.to_string(), uri))
            .collect();
        self.workspace_error = None;
        self.clear_workspace_watch_errors();
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
                // Not every position holds a renameable anchor. Clients that
                // support `prepareRename` are told so, and ask before starting
                // a rename that would return no edit. Clients that do not keep
                // the plain declaration and the behaviour they had before.
                rename_provider: Some(if self.client.rename_prepare_support {
                    lsp::OneOf::Right(lsp::RenameOptions {
                        prepare_provider: Some(true),
                        work_done_progress_options: lsp::WorkDoneProgressOptions::default(),
                    })
                } else {
                    lsp::OneOf::Left(true)
                }),
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
        if let Some(error) = self.workspace_error.clone() {
            let options = self.analysis_options_for(None);
            let mut job = self.documents.begin_open_with_options(
                document.uri.to_string(),
                document.version,
                document.text,
                options,
            );
            attach_workspace(&mut job, Err(error));
            return vec![job];
        }
        let affected = match self.workspace.upsert_open(
            document.uri.clone(),
            i64::from(document.version),
            document.text.clone(),
        ) {
            Ok(affected) => affected,
            Err(error) => {
                self.workspace_input_error = Some(error);
                return Vec::new();
            }
        };
        self.workspace_input_error = None;
        let workspace = self.workspace.input(&document.uri);
        let options = self.analysis_options_for(workspace.as_ref().ok());
        let mut job = self.documents.begin_open_with_options(
            document.uri.to_string(),
            document.version,
            document.text,
            options,
        );
        let mut affected = affected;
        if let Ok(pruned) = self
            .workspace
            .begin_document_revision(&document.uri, job.request.revision.generation)
        {
            affected.extend(pruned);
        }
        attach_workspace(&mut job, self.workspace.input(&document.uri));
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
        let mut affected = affected;
        affected.extend(
            self.workspace.begin_document_revision(
                &params.text_document.uri,
                job.request.revision.generation,
            )?,
        );
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

    fn clear_workspace_watch_errors(&mut self) -> bool {
        let changed = !self.workspace_watch_errors.is_empty()
            || self.workspace_watch_errors_overflowed
            || self.workspace_watch_recovery_required;
        self.workspace_watch_errors.clear();
        self.workspace_watch_error_bytes = 0;
        self.workspace_watch_errors_overflowed = false;
        self.workspace_watch_recovery_required = false;
        changed
    }

    fn clear_workspace_watch_error(&mut self, uri: &lsp::Url) -> bool {
        let Some(error) = self.workspace_watch_errors.remove(uri.as_str()) else {
            return false;
        };
        self.workspace_watch_error_bytes = self
            .workspace_watch_error_bytes
            .saturating_sub(uri.as_str().len().saturating_add(error.len()));
        true
    }

    fn record_workspace_watch_error(&mut self, uri: &lsp::Url, error: String) -> bool {
        if let Some(previous) = self.workspace_watch_errors.get_mut(uri.as_str()) {
            if *previous == error {
                return false;
            }
            let next_bytes = self
                .workspace_watch_error_bytes
                .saturating_sub(previous.len())
                .saturating_add(error.len());
            if next_bytes > MAX_WORKSPACE_WATCH_ERROR_BYTES {
                self.workspace_watch_errors_overflowed = true;
                return true;
            }
            self.workspace_watch_error_bytes = next_bytes;
            *previous = error;
            return true;
        }
        let additional_bytes = uri.as_str().len().saturating_add(error.len());
        if self.workspace_watch_errors.len() >= MAX_WORKSPACE_WATCH_ERRORS
            || self
                .workspace_watch_error_bytes
                .saturating_add(additional_bytes)
                > MAX_WORKSPACE_WATCH_ERROR_BYTES
        {
            let changed = !self.workspace_watch_errors_overflowed;
            self.workspace_watch_errors_overflowed = true;
            return changed;
        }
        self.workspace_watch_error_bytes = self
            .workspace_watch_error_bytes
            .saturating_add(additional_bytes);
        self.workspace_watch_errors.insert(uri.to_string(), error);
        true
    }

    fn workspace_watch_error_message(&self) -> Option<String> {
        if self.workspace_watch_errors.is_empty() && !self.workspace_watch_errors_overflowed {
            return None;
        }
        let mut messages = self
            .workspace_watch_errors
            .iter()
            .map(|(uri, error)| format!("{uri}: {error}"))
            .collect::<Vec<_>>();
        if self.workspace_watch_errors_overflowed {
            messages.push(format!(
                "additional workspace watch errors exceed the retained limit of {MAX_WORKSPACE_WATCH_ERRORS}"
            ));
        }
        Some(messages.join("; "))
    }

    pub(crate) fn workspace_files_changed_with_journal(
        &mut self,
        params: lsp::DidChangeWatchedFilesParams,
    ) -> WorkspaceFileChanges {
        if params.changes.iter().any(|change| {
            change.uri.path_segments().and_then(Iterator::last) == Some(adocweave_config::FILE_NAME)
        }) {
            // Project files are handled by the backend's asynchronous full
            // scan. Mixing an incremental resource reload with a new resource
            // plan would admit files under two different configurations.
            return WorkspaceFileChanges {
                jobs: Vec::new(),
                journal: Vec::new(),
                replay_complete: false,
                recovery_required: false,
            };
        }
        let mut coalesced = std::collections::BTreeMap::<lsp::Url, lsp::FileChangeType>::new();
        let mut uri_bytes = 0_usize;
        for change in params.changes {
            if !coalesced.contains_key(&change.uri) {
                if coalesced.len() >= MAX_WORKSPACE_WATCH_CHANGES
                    || uri_bytes.saturating_add(change.uri.as_str().len())
                        > MAX_WORKSPACE_WATCH_URI_BYTES
                {
                    self.workspace_watch_recovery_required = true;
                    return WorkspaceFileChanges {
                        jobs: Vec::new(),
                        journal: Vec::new(),
                        replay_complete: false,
                        recovery_required: true,
                    };
                }
                uri_bytes = uri_bytes.saturating_add(change.uri.as_str().len());
            }
            coalesced.insert(change.uri, change.typ);
        }
        let changes = coalesced
            .into_iter()
            .map(|(uri, typ)| lsp::FileEvent { uri, typ });
        let mut affected = std::collections::BTreeSet::new();
        let mut journal = Vec::new();
        let mut watch_errors_changed = false;
        for change in changes {
            let kind = if change.typ == lsp::FileChangeType::DELETED {
                WatchedFileKind::Delete
            } else {
                WatchedFileKind::Upsert
            };
            match self.workspace.apply_watched_file(change.uri.clone(), kind) {
                Ok(update) => {
                    watch_errors_changed |= self.clear_workspace_watch_error(&change.uri);
                    affected.extend(update.affected);
                    if update.journal_relevant {
                        journal.push(change);
                    }
                }
                Err(error) => {
                    watch_errors_changed |=
                        self.record_workspace_watch_error(&change.uri, error.message);
                    if error.journal_relevant {
                        journal.push(change);
                    }
                }
            }
        }
        let mut jobs = Vec::new();
        self.append_dependent_jobs(&affected, "", &mut jobs);
        if watch_errors_changed {
            let queued = jobs
                .iter()
                .map(|job| job.uri.clone())
                .collect::<std::collections::BTreeSet<_>>();
            for (uri, _, _) in self.documents.open_sources() {
                if queued.contains(&uri) {
                    continue;
                }
                let Ok(parsed) = uri.parse() else {
                    continue;
                };
                let workspace = self.workspace.input(&parsed);
                let options = self.analysis_options_for(workspace.as_ref().ok());
                if let Some(mut job) = self.documents.reconfigure(&uri, options) {
                    attach_workspace(&mut job, workspace);
                    jobs.push(job);
                }
            }
        }
        WorkspaceFileChanges {
            jobs,
            journal,
            replay_complete: true,
            recovery_required: self.workspace_watch_errors_overflowed
                || self.workspace_watch_recovery_required,
        }
    }

    /// Reads the workspace roots without touching service state.
    ///
    /// This is the half of a reload whose cost grows with the workspace: it
    /// walks every directory below each root and reads the `.adoc` files it
    /// finds. It takes `&self` so a caller can run it on a worker while the
    /// event loop keeps answering requests. Installing the result is
    /// [`Self::apply_workspace_scan`], which is cheap and runs in order.
    #[cfg(test)]
    pub fn plan_workspace_scan(&self, cancellation: &dyn CancellationCheck) -> WorkspaceScan {
        let roots = self.workspace_roots.values().cloned().collect::<Vec<_>>();
        WorkspaceScan {
            loaded: self
                .workspace
                .load_roots_detached_with_cancellation(&roots, cancellation),
        }
    }

    pub(crate) fn plan_workspace_scan_with_job(
        &self,
        cancellation: &dyn CancellationCheck,
        job: &adocweave_host::FilesystemJobCoordinator,
    ) -> WorkspaceScan {
        let roots = self.workspace_roots.values().cloned().collect::<Vec<_>>();
        WorkspaceScan {
            loaded: self
                .workspace
                .load_roots_detached_with_job(&roots, cancellation, job),
        }
    }

    /// Installs a completed scan and returns the analyses it makes stale.
    ///
    /// The documents open at this moment are overlaid onto the read, not the
    /// ones open when it started, so a document opened during the walk is kept.
    pub(crate) fn apply_workspace_scan(&mut self, scan: WorkspaceScan) -> WorkspaceScanApplication {
        let open_sources = self.documents.open_sources();
        let parsed_open_sources = parse_open_sources(&open_sources);
        let outcome = self
            .workspace
            .apply_loaded_roots(scan.loaded, &parsed_open_sources);
        let installed = outcome.is_ok();
        WorkspaceScanApplication {
            jobs: self.finish_reload(outcome, open_sources),
            installed,
        }
    }

    /// Records an internal scan worker failure without replacing the last
    /// coherent workspace snapshot.
    pub fn workspace_scan_failed(&mut self, error: String) -> Vec<AnalysisJob> {
        self.workspace_error = Some(error);
        self.workspace_watch_recovery_required = true;
        self.documents
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
            .collect()
    }

    /// Turns the result of a reload into the reanalyses it requires.
    fn finish_reload(
        &mut self,
        outcome: Result<(), String>,
        open_sources: Vec<(String, i32, String)>,
    ) -> Vec<AnalysisJob> {
        if let Err(error) = outcome {
            let failed_closed = self.workspace.last_load_failed_closed();
            self.workspace_error = Some(error.clone());
            self.workspace_watch_recovery_required = true;
            return open_sources
                .into_iter()
                .filter_map(|(uri, _, _)| {
                    let workspace = if failed_closed {
                        Err(error.clone())
                    } else {
                        let parsed = uri.parse().ok()?;
                        self.workspace.input(&parsed)
                    };
                    let options = self.analysis_options_for(workspace.as_ref().ok());
                    let mut job = self.documents.reconfigure(&uri, options)?;
                    attach_workspace(&mut job, workspace);
                    Some(job)
                })
                .collect();
        }
        self.workspace_error = None;
        self.clear_workspace_watch_errors();
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
    ) -> bool {
        if !self.client.workspace_folders {
            return false;
        }
        let mut roots = self.workspace_roots.clone();
        for folder in params.event.removed {
            roots.remove(folder.uri.as_str());
        }
        for folder in params.event.added {
            roots.insert(folder.uri.to_string(), folder.uri);
        }
        if roots == self.workspace_roots {
            return false;
        }
        self.workspace_roots = roots;
        true
    }

    /// Returns the effective text a workspace resource holds, if it is known.
    #[cfg(test)]
    pub fn workspace_resource(&self, uri: &lsp::Url) -> Option<std::sync::Arc<str>> {
        self.workspace.resource_text(uri)
    }

    /// Returns how many documents the workspace currently holds.
    #[cfg(test)]
    pub fn workspace_analysis_count(&self) -> usize {
        self.workspace.resource_count()
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
                            watchers: vec![lsp::FileSystemWatcher {
                                // Includes may use any extension. The handler
                                // reads only known dependencies or new `.adoc`
                                // roots, so unrelated notifications stay I/O-free.
                                glob_pattern: lsp::GlobPattern::String("**/*".to_owned()),
                                kind: Some(
                                    lsp::WatchKind::Create
                                        | lsp::WatchKind::Change
                                        | lsp::WatchKind::Delete,
                                ),
                            }],
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
        let root = job
            .workspace
            .as_ref()
            .expect("current workspace analysis has an input")
            .root
            .clone();
        if self.workspace.accept_for_root(&root, &analysis).is_err() {
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

    /// Rebuilds a current document job when only its workspace input became
    /// stale while another document loaded or changed a resource.
    pub fn refresh_stale_workspace(&mut self, job: &AnalysisJob) -> Option<AnalysisJob> {
        if !self.documents.job_is_current(job) {
            return None;
        }
        let input = job.workspace.as_ref()?;
        if self.workspace.input_is_current(input) {
            return None;
        }
        let uri = job.uri.parse().ok()?;
        let workspace = self.workspace.input(&uri);
        let options = self.analysis_options_for(workspace.as_ref().ok());
        let mut retry = self.documents.reconfigure(&job.uri, options)?;
        retry.include_resolution_attempts = job.include_resolution_attempts.clone();
        attach_workspace(&mut retry, workspace);
        Some(retry)
    }

    /// Loads one missing include requested by the current analysis and creates
    /// a replacement job against the resulting workspace generation.
    pub fn resolve_missing_include(
        &mut self,
        job: &AnalysisJob,
        target: &adocweave_workspace::ResourceId,
    ) -> Result<Option<AnalysisJob>, String> {
        if !self.documents.job_is_current(job) {
            return Ok(None);
        }
        let uri = job
            .uri
            .parse()
            .map_err(|error| format!("analysis root URI is invalid: {error}"))?;
        if !job
            .workspace
            .as_ref()
            .is_some_and(|input| self.workspace.input_is_current(input))
        {
            return Ok(None);
        }
        let mut attempts = job.include_resolution_attempts.clone();
        let limit = job
            .workspace
            .as_ref()
            .map_or(0, |input| input.options.max_includes as usize);
        if attempts.len() >= limit || !attempts.insert(target.clone()) {
            return Ok(None);
        }
        let pending_generation = job.request.revision.generation;
        if !self
            .workspace
            .load_missing_include(&uri, target, pending_generation)?
        {
            return Ok(None);
        }
        let workspace = self.workspace.input(&uri);
        let options = self.analysis_options_for(workspace.as_ref().ok());
        let Some(mut retry) = self.documents.reconfigure(&job.uri, options) else {
            self.workspace
                .discard_pending_include_dependencies(&uri, pending_generation)?;
            return Ok(None);
        };
        self.workspace.retag_pending_include_dependencies(
            &uri,
            pending_generation,
            retry.request.revision.generation,
        )?;
        retry.include_resolution_attempts = attempts;
        attach_workspace(&mut retry, workspace);
        Ok(Some(retry))
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
        let mut affected = self.workspace.close_open(uri).unwrap_or_else(|error| {
            self.workspace_error = Some(error);
            std::collections::BTreeSet::new()
        });
        match self.workspace.forget_include_dependencies(uri) {
            Ok(pruned) => affected.extend(pruned),
            Err(error) => self.workspace_error = Some(error),
        }
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
        let workspace_watch_error = self.workspace_watch_error_message();
        let document = self.documents.get(uri.as_str());
        let resource = self.workspace.get(uri);
        let source = document
            .map(|document| document.request.source.as_ref())
            .or_else(|| resource.map(|resource| resource.text().as_ref()));
        let Some(source) = source else {
            return Ok(lsp::PublishDiagnosticsParams::new(
                uri.clone(),
                self.workspace_error
                    .as_deref()
                    .or(workspace_watch_error.as_deref())
                    .or(self.workspace_input_error.as_deref())
                    .map(crate::diagnostics::workspace_error)
                    .into_iter()
                    .collect(),
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
        if let Some(error) = &workspace_watch_error {
            diagnostics.push(crate::diagnostics::workspace_error(error));
        }
        if let Some(error) = &self.workspace_input_error {
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

    pub fn document_symbols_cancellable(
        &self,
        uri: &lsp::Url,
        cancellation: &QueryCancellation,
    ) -> QueryResult<Option<lsp::DocumentSymbolResponse>> {
        cancellation.check_now()?;
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
        let response = crate::document_symbols::symbols(
            &document.analysis,
            uri,
            self.position_encoding,
            presentation,
        )?;
        cancellation.check_now()?;
        Ok(Some(response))
    }

    pub fn code_actions_cancellable(
        &self,
        uri: &lsp::Url,
        range: lsp::Range,
        context: &lsp::CodeActionContext,
        cancellation: &QueryCancellation,
    ) -> QueryResult<Option<Vec<lsp::CodeActionOrCommand>>> {
        cancellation.check_now()?;
        if !self.client.code_action_quickfix
            || !code_action_kind_requested(context.only.as_deref(), &lsp::CodeActionKind::QUICKFIX)
        {
            return Ok(Some(Vec::new()));
        }
        let Some(document) = self.documents.snapshot(uri.as_str()) else {
            return Ok(Some(Vec::new()));
        };
        let actions = crate::diagnostics::quick_fixes(
            uri,
            revision_version_i32(&document.revision),
            &document.analysis,
            range,
            self.position_encoding,
            QuickFixCapabilities {
                versioned_document_changes: self.client.versioned_document_changes,
                is_preferred: self.client.code_action_is_preferred,
            },
        )?;
        cancellation.check_now()?;
        Ok(Some(actions))
    }

    pub fn formatting_cancellable(
        &self,
        uri: &lsp::Url,
        cancellation: &QueryCancellation,
    ) -> QueryResult<Option<Vec<lsp::TextEdit>>> {
        cancellation.check_now()?;
        let Some(document) = self.documents.snapshot(uri.as_str()) else {
            return Ok(Some(Vec::new()));
        };
        editing::formatting(
            &document.analysis,
            &document.format,
            self.position_encoding,
            cancellation,
        )
        .map(Some)
    }

    pub fn hover_cancellable(
        &self,
        uri: &lsp::Url,
        position: lsp::Position,
        cancellation: &QueryCancellation,
    ) -> QueryResult<Option<lsp::Hover>> {
        cancellation.check_now()?;
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
        .map_err(Into::into)
    }

    pub fn completion_cancellable(
        &self,
        uri: &lsp::Url,
        position: lsp::Position,
        cancellation: &QueryCancellation,
    ) -> QueryResult<Option<lsp::CompletionResponse>> {
        cancellation.check_now()?;
        let Some(document) = self.documents.snapshot(uri.as_str()) else {
            return Ok(Some(presentation::empty_completion()));
        };
        let workspaces = self.documents.workspace_analyses().collect::<Vec<_>>();
        let response = presentation::completion(
            &document.analysis,
            &workspaces,
            uri,
            position,
            self.position_encoding,
        )?;
        cancellation.check_now()?;
        Ok(Some(response))
    }

    pub fn definition_cancellable(
        &self,
        uri: &lsp::Url,
        position: lsp::Position,
        cancellation: &QueryCancellation,
    ) -> QueryResult<Option<lsp::GotoDefinitionResponse>> {
        cancellation.check_now()?;
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
        match navigation::definition(&input, uri, position, cancellation)? {
            navigation::Definition::Resolved(response) => Ok(response),
            navigation::Definition::Host(target) => {
                let request =
                    host_reference_request(&document, uri, target, self.position_encoding);
                cancellation.check_now()?;
                let result = self
                    .host_index
                    .definition(&request)
                    .map(|location| location.map(lsp::GotoDefinitionResponse::Scalar));
                cancellation.check_now()?;
                Ok(result?)
            }
        }
    }

    pub fn references_cancellable(
        &self,
        uri: &lsp::Url,
        position: lsp::Position,
        include_declaration: bool,
        cancellation: &QueryCancellation,
    ) -> QueryResult<Option<Vec<lsp::Location>>> {
        cancellation.check_now()?;
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
        let result =
            navigation::references(&input, uri, position, include_declaration, cancellation)?;
        if let Some(target) = result.host_target {
            let request = host_reference_request(&document, uri, target, self.position_encoding);
            cancellation.check_now()?;
            let locations = self.host_index.references(&request, include_declaration);
            cancellation.check_now()?;
            let locations = locations?;
            if let Some(locations) = locations {
                return Ok(Some(locations));
            }
        }
        Ok(Some(result.fallback))
    }

    /// Answers whether a rename may start at this position.
    ///
    /// `None` tells the client the position cannot be renamed, so it reports
    /// that instead of opening a rename the server would answer with nothing.
    /// The anchor's current text becomes the placeholder the editor offers.
    pub fn prepare_rename_cancellable(
        &self,
        uri: &lsp::Url,
        position: lsp::Position,
        cancellation: &QueryCancellation,
    ) -> QueryResult<Option<lsp::PrepareRenameResponse>> {
        cancellation.check_now()?;
        if !self.client.rename_prepare_support {
            return Ok(None);
        }
        let Some(document) = self.documents.snapshot(uri.as_str()) else {
            return Ok(None);
        };
        Ok(
            editing::renameable_anchor(&document.analysis, position, self.position_encoding)?.map(
                |(_, range, placeholder)| lsp::PrepareRenameResponse::RangeWithPlaceholder {
                    range,
                    placeholder,
                },
            ),
        )
    }

    pub fn rename_cancellable(
        &self,
        uri: &lsp::Url,
        position: lsp::Position,
        new_name: &str,
        cancellation: &QueryCancellation,
    ) -> QueryResult<Option<lsp::WorkspaceEdit>> {
        cancellation.check_now()?;
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
        cancellation.check_now()?;
        let host_locations = self.host_index.references(&host_request, true);
        cancellation.check_now()?;
        let host_locations = host_locations?;
        let locations = if let Some(locations) = host_locations {
            locations
        } else {
            self.references_cancellable(uri, position, true, cancellation)?
                .unwrap_or_default()
        };
        cancellation.check_now()?;
        Ok(editing::rename_edit(locations, new_name))
    }

    pub fn document_links_cancellable(
        &self,
        uri: &lsp::Url,
        cancellation: &QueryCancellation,
    ) -> QueryResult<Option<Vec<lsp::DocumentLink>>> {
        cancellation.check_now()?;
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
        let mut links = navigation::document_links(&input, uri, tooltips, cancellation)?;
        for unresolved in std::mem::take(&mut links.unresolved) {
            cancellation.checkpoint()?;
            let request = host_reference_request(
                &document,
                uri,
                unresolved.target.clone(),
                self.position_encoding,
            );
            cancellation.check_now()?;
            let location = self.host_index.definition(&request).ok().flatten();
            cancellation.check_now()?;
            links.resolve(unresolved, location, tooltips);
        }
        Ok(Some(links.finish(cancellation)?))
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

    pub fn semantic_tokens_cancellable(
        &self,
        uri: &lsp::Url,
        cancellation: &QueryCancellation,
    ) -> QueryResult<Option<lsp::SemanticTokensResult>> {
        cancellation.check_now()?;
        if !self.client.semantic_tokens_full {
            return Ok(None);
        }
        let document = self.documents.snapshot(uri.as_str());
        crate::semantic_tokens::response(
            document.as_ref().map(|document| document.analysis.as_ref()),
            self.position_encoding,
            cancellation,
        )
        .map(Some)
    }

    #[cfg(test)]
    fn test_query<T>(result: QueryResult<T>) -> Result<T, String> {
        result.map_err(|error| error.to_string())
    }

    #[cfg(test)]
    pub fn document_symbols(
        &self,
        uri: &lsp::Url,
    ) -> Result<Option<lsp::DocumentSymbolResponse>, String> {
        let cancellation = crate::cancellation::test_cancellation();
        Self::test_query(self.document_symbols_cancellable(uri, &cancellation))
    }

    #[cfg(test)]
    pub fn code_actions(
        &self,
        uri: &lsp::Url,
        range: lsp::Range,
        context: &lsp::CodeActionContext,
    ) -> Result<Option<Vec<lsp::CodeActionOrCommand>>, String> {
        let cancellation = crate::cancellation::test_cancellation();
        Self::test_query(self.code_actions_cancellable(uri, range, context, &cancellation))
    }

    #[cfg(test)]
    pub fn formatting(&self, uri: &lsp::Url) -> Result<Option<Vec<lsp::TextEdit>>, String> {
        let cancellation = crate::cancellation::test_cancellation();
        Self::test_query(self.formatting_cancellable(uri, &cancellation))
    }

    #[cfg(test)]
    pub fn hover(
        &self,
        uri: &lsp::Url,
        position: lsp::Position,
    ) -> Result<Option<lsp::Hover>, String> {
        let cancellation = crate::cancellation::test_cancellation();
        Self::test_query(self.hover_cancellable(uri, position, &cancellation))
    }

    #[cfg(test)]
    pub fn completion(
        &self,
        uri: &lsp::Url,
        position: lsp::Position,
    ) -> Result<Option<lsp::CompletionResponse>, String> {
        let cancellation = crate::cancellation::test_cancellation();
        Self::test_query(self.completion_cancellable(uri, position, &cancellation))
    }

    #[cfg(test)]
    pub fn definition(
        &self,
        uri: &lsp::Url,
        position: lsp::Position,
    ) -> Result<Option<lsp::GotoDefinitionResponse>, String> {
        let cancellation = crate::cancellation::test_cancellation();
        Self::test_query(self.definition_cancellable(uri, position, &cancellation))
    }

    #[cfg(test)]
    pub fn references(
        &self,
        uri: &lsp::Url,
        position: lsp::Position,
        include_declaration: bool,
    ) -> Result<Option<Vec<lsp::Location>>, String> {
        let cancellation = crate::cancellation::test_cancellation();
        Self::test_query(self.references_cancellable(
            uri,
            position,
            include_declaration,
            &cancellation,
        ))
    }

    #[cfg(test)]
    pub fn prepare_rename(
        &self,
        uri: &lsp::Url,
        position: lsp::Position,
    ) -> Result<Option<lsp::PrepareRenameResponse>, String> {
        let cancellation = crate::cancellation::test_cancellation();
        Self::test_query(self.prepare_rename_cancellable(uri, position, &cancellation))
    }

    #[cfg(test)]
    pub fn rename(
        &self,
        uri: &lsp::Url,
        position: lsp::Position,
        new_name: &str,
    ) -> Result<Option<lsp::WorkspaceEdit>, String> {
        let cancellation = crate::cancellation::test_cancellation();
        Self::test_query(self.rename_cancellable(uri, position, new_name, &cancellation))
    }

    #[cfg(test)]
    pub fn document_links(&self, uri: &lsp::Url) -> Result<Option<Vec<lsp::DocumentLink>>, String> {
        let cancellation = crate::cancellation::test_cancellation();
        Self::test_query(self.document_links_cancellable(uri, &cancellation))
    }

    #[cfg(test)]
    pub fn semantic_tokens(
        &self,
        uri: &lsp::Url,
    ) -> Result<Option<lsp::SemanticTokensResult>, String> {
        let cancellation = crate::cancellation::test_cancellation();
        Self::test_query(self.semantic_tokens_cancellable(uri, &cancellation))
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
