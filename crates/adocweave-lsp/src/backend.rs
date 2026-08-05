//! Typed `async-lsp` adapter with generation-checked background analysis.

use std::collections::BTreeMap;
use std::num::NonZeroUsize;
use std::ops::ControlFlow;
use std::sync::Arc;

use adocweave::{CancellationCheck, CancellationToken};
use adocweave_workspace::WorkspaceAnalysis;
use async_lsp::client_monitor::ClientProcessMonitorLayer;
use async_lsp::concurrency::ConcurrencyLayer;
use async_lsp::lsp_types::{
    DidChangeWatchedFilesParams, FileChangeType, FileEvent, PublishDiagnosticsParams, Url,
    notification, request,
};
use async_lsp::panic::CatchUnwindLayer;
use async_lsp::router::Router;
use async_lsp::tracing::TracingLayer;
use async_lsp::{ClientSocket, ErrorCode, ResponseError};
use serde_json::Value;
use tokio::sync::Semaphore;
use tower::ServiceBuilder;

use crate::cancellation::{QueryCancellation, QueryError, QueryResult};
use crate::lifecycle::ProtocolLifecycleLayer;
use crate::service::{LanguageService, WorkspaceFileChanges};
use crate::state::{Adoption, AnalysisJob, WorkspaceProblem};
use crate::{HostReferenceIndex, NoHostReferenceIndex};

const MAX_CONCURRENT_REQUESTS: usize = 16;
const MAX_CONCURRENT_ANALYSES: usize = 2;
const MAX_WATCH_JOURNAL_ENTRIES: usize = 10_000;
const MAX_WATCH_JOURNAL_URI_BYTES: usize = 1024 * 1024;
const WATCH_SCAN_RECOVERY_DEBOUNCE_MS: u64 = 100;

pub(crate) struct Backend {
    client: ClientSocket,
    service: LanguageService,
    cpu_limit: Arc<Semaphore>,
    analysis_tasks: BTreeMap<String, AnalysisTask>,
    workspace_scans: WorkspaceScanCoordinator,
    workspace_scan_recovery_timer: WorkspaceScanRecoveryTimer,
}

#[derive(Default)]
enum WorkspaceScanRecoveryTimer {
    #[default]
    Idle,
    Debouncing {
        generation: u64,
        task: AbortOnDrop,
    },
}

struct AbortOnDrop {
    handle: tokio::task::JoinHandle<()>,
    abort: bool,
}

impl AbortOnDrop {
    fn new(handle: tokio::task::JoinHandle<()>) -> Self {
        Self {
            handle,
            abort: true,
        }
    }

    fn completed(mut self) {
        self.abort = false;
    }
}

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        if self.abort {
            self.handle.abort();
        }
    }
}

impl WorkspaceScanRecoveryTimer {
    fn replace(&mut self, generation: u64, handle: tokio::task::JoinHandle<()>) {
        self.cancel();
        *self = Self::Debouncing {
            generation,
            task: AbortOnDrop::new(handle),
        };
    }

    fn complete(&mut self, generation: u64) -> bool {
        if !matches!(self, Self::Debouncing { generation: current, .. } if *current == generation) {
            return false;
        }
        let Self::Debouncing { task, .. } = std::mem::take(self) else {
            unreachable!("matching debounce state was checked above");
        };
        task.completed();
        true
    }

    fn cancel(&mut self) {
        *self = Self::Idle;
    }

    #[cfg(test)]
    fn generation(&self) -> Option<u64> {
        match self {
            Self::Idle => None,
            Self::Debouncing { generation, .. } => Some(*generation),
        }
    }
}

#[derive(Default)]
enum WorkspaceScanPhase {
    #[default]
    Idle,
    Running(ActiveWorkspaceScan),
}

struct ActiveWorkspaceScan {
    sequence: u64,
    cancellation: Arc<CancellationToken>,
    accept_result: bool,
    rejection: Option<String>,
}

struct WorkspaceScanStart {
    sequence: u64,
    cancellation: Arc<CancellationToken>,
}

struct WorkspaceScanCompletion {
    accept_result: bool,
    rejection: Option<String>,
    next: Option<WorkspaceScanStart>,
}

#[derive(Default)]
enum WorkspaceRecoveryState {
    #[default]
    Idle,
    /// A timer handle exists in `Backend` for this generation.
    Debouncing {
        generation: u64,
        minimum_scan_sequence: u64,
    },
    /// The timer fired, but the active scan and its replay journal can satisfy
    /// the recovery. No timer handle exists while completion is awaited.
    AwaitingActiveCompletion {
        generation: u64,
        minimum_scan_sequence: u64,
    },
}

#[derive(Default)]
struct WorkspaceScanCoordinator {
    sequence: u64,
    phase: WorkspaceScanPhase,
    pending_replacement: bool,
    watched_changes: WatchedChangeJournal,
    recovery_generation: u64,
    recovery: WorkspaceRecoveryState,
}

impl WorkspaceScanCoordinator {
    fn request_replacement(&mut self) -> Option<WorkspaceScanStart> {
        self.disarm_recovery();
        self.watched_changes.clear();
        if let WorkspaceScanPhase::Running(active) = &mut self.phase {
            active.accept_result = false;
            active.rejection = None;
            active.cancellation.cancel();
            self.pending_replacement = true;
            return None;
        }
        Some(self.start())
    }

    fn reject_result(&mut self, reason: String) {
        if let WorkspaceScanPhase::Running(active) = &mut self.phase {
            active.accept_result = false;
            active.rejection.get_or_insert(reason);
        }
    }

    fn reject_unreplayable_watch(&mut self) {
        if let WorkspaceScanPhase::Running(active) = &mut self.phase {
            active.accept_result = false;
            active.rejection = None;
        }
    }

    fn accepts_active_result(&self) -> bool {
        matches!(&self.phase, WorkspaceScanPhase::Running(active) if active.accept_result)
    }

    fn complete_active(&mut self, sequence: u64) -> Option<WorkspaceScanCompletion> {
        let WorkspaceScanPhase::Running(active) =
            std::mem::replace(&mut self.phase, WorkspaceScanPhase::Idle)
        else {
            return None;
        };
        if active.sequence != sequence {
            self.phase = WorkspaceScanPhase::Running(active);
            return None;
        }
        let start_next = std::mem::take(&mut self.pending_replacement);
        let next = start_next.then(|| self.start());
        Some(WorkspaceScanCompletion {
            accept_result: active.accept_result,
            rejection: active.rejection,
            next,
        })
    }

    fn cancel(&mut self) {
        if let WorkspaceScanPhase::Running(active) = &mut self.phase {
            active.accept_result = false;
            active.rejection = None;
            active.cancellation.cancel();
        }
        self.pending_replacement = false;
        self.watched_changes.clear();
        self.disarm_recovery();
    }

    fn start(&mut self) -> WorkspaceScanStart {
        debug_assert!(matches!(self.phase, WorkspaceScanPhase::Idle));
        self.sequence = self.sequence.saturating_add(1);
        let cancellation = Arc::new(CancellationToken::new());
        self.phase = WorkspaceScanPhase::Running(ActiveWorkspaceScan {
            sequence: self.sequence,
            cancellation: Arc::clone(&cancellation),
            accept_result: true,
            rejection: None,
        });
        WorkspaceScanStart {
            sequence: self.sequence,
            cancellation,
        }
    }

    fn arm_recovery(&mut self, minimum_scan_sequence: u64) -> u64 {
        let minimum_scan_sequence = match self.recovery {
            WorkspaceRecoveryState::Idle => minimum_scan_sequence,
            WorkspaceRecoveryState::Debouncing {
                minimum_scan_sequence: existing,
                ..
            }
            | WorkspaceRecoveryState::AwaitingActiveCompletion {
                minimum_scan_sequence: existing,
                ..
            } => existing.max(minimum_scan_sequence),
        };
        self.recovery_generation = self.recovery_generation.saturating_add(1);
        let generation = self.recovery_generation;
        self.recovery = WorkspaceRecoveryState::Debouncing {
            generation,
            minimum_scan_sequence,
        };
        generation
    }

    fn disarm_recovery(&mut self) {
        self.recovery_generation = self.recovery_generation.saturating_add(1);
        self.recovery = WorkspaceRecoveryState::Idle;
    }

    fn recovery_generation(&self) -> Option<u64> {
        match self.recovery {
            WorkspaceRecoveryState::Idle => None,
            WorkspaceRecoveryState::Debouncing { generation, .. }
            | WorkspaceRecoveryState::AwaitingActiveCompletion { generation, .. } => {
                Some(generation)
            }
        }
    }

    fn debouncing_generation(&self) -> Option<u64> {
        match self.recovery {
            WorkspaceRecoveryState::Debouncing { generation, .. } => Some(generation),
            WorkspaceRecoveryState::Idle
            | WorkspaceRecoveryState::AwaitingActiveCompletion { .. } => None,
        }
    }

    fn recovery_minimum_scan_sequence(&self) -> Option<u64> {
        match self.recovery {
            WorkspaceRecoveryState::Idle => None,
            WorkspaceRecoveryState::Debouncing {
                minimum_scan_sequence,
                ..
            }
            | WorkspaceRecoveryState::AwaitingActiveCompletion {
                minimum_scan_sequence,
                ..
            } => Some(minimum_scan_sequence),
        }
    }

    fn active_or_next_sequence(&self) -> u64 {
        match &self.phase {
            WorkspaceScanPhase::Idle => self.sequence.saturating_add(1),
            WorkspaceScanPhase::Running(active) => active.sequence,
        }
    }

    fn sequence_after_active(&self) -> u64 {
        match &self.phase {
            WorkspaceScanPhase::Idle => self.sequence.saturating_add(1),
            WorkspaceScanPhase::Running(active) => active.sequence.saturating_add(1),
        }
    }

    fn rearm_recovery(&mut self) -> Option<u64> {
        let minimum = self.recovery_minimum_scan_sequence()?;
        Some(self.arm_recovery(minimum))
    }

    fn disarm_recovery_if_covered(&mut self, sequence: u64) -> bool {
        if self
            .recovery_minimum_scan_sequence()
            .is_some_and(|minimum| sequence < minimum)
        {
            return false;
        }
        self.disarm_recovery();
        true
    }
}

#[derive(Default)]
struct WatchedChangeJournal {
    changes: BTreeMap<Url, FileChangeType>,
    uri_bytes: usize,
    overflowed: bool,
}

impl WatchedChangeJournal {
    fn record(&mut self, changes: &[FileEvent]) -> bool {
        self.record_with_limits(
            changes,
            MAX_WATCH_JOURNAL_ENTRIES,
            MAX_WATCH_JOURNAL_URI_BYTES,
        )
    }

    fn record_with_limits(
        &mut self,
        changes: &[FileEvent],
        max_entries: usize,
        max_uri_bytes: usize,
    ) -> bool {
        if self.overflowed {
            return false;
        }
        for change in changes {
            let is_new = !self.changes.contains_key(&change.uri);
            let additional_bytes = if is_new { change.uri.as_str().len() } else { 0 };
            if self.changes.len().saturating_add(usize::from(is_new)) > max_entries
                || self.uri_bytes.saturating_add(additional_bytes) > max_uri_bytes
            {
                self.changes.clear();
                self.uri_bytes = 0;
                self.overflowed = true;
                return false;
            }
            self.uri_bytes = self.uri_bytes.saturating_add(additional_bytes);
            self.changes.insert(change.uri.clone(), change.typ);
        }
        true
    }

    fn take(&mut self) -> Option<DidChangeWatchedFilesParams> {
        if self.overflowed {
            self.clear();
            return None;
        }
        let changes = std::mem::take(&mut self.changes)
            .into_iter()
            .map(|(uri, typ)| FileEvent { uri, typ })
            .collect::<Vec<_>>();
        self.uri_bytes = 0;
        (!changes.is_empty()).then_some(DidChangeWatchedFilesParams { changes })
    }

    fn clear(&mut self) {
        self.changes.clear();
        self.uri_bytes = 0;
        self.overflowed = false;
    }
}

impl WorkspaceScanCoordinator {
    fn record_workspace_changes(&mut self, changes: &WorkspaceFileChanges) -> Option<u64> {
        let mut recovery_generation = self.record_watched_changes(&changes.journal);
        if changes.recovery_required {
            recovery_generation = Some(if changes.replay_complete {
                self.request_quiet_recovery()
            } else {
                self.request_unreplayable_recovery()
            });
        }
        recovery_generation
    }

    fn record_watched_changes(&mut self, changes: &[FileEvent]) -> Option<u64> {
        if self.accepts_active_result() && !self.watched_changes.record(changes) {
            // The journal can no longer reconstruct all changes made after the
            // worker took its snapshot. Keep the incrementally updated service
            // state and reject that snapshot instead of installing older
            // contents over it. The worker is allowed to finish and reports a
            // bounded failure instead of retrying forever under a notification
            // stream that exceeds this safety limit.
            self.reject_result(format!(
                "workspace watch journal limit exceeded: at most {MAX_WATCH_JOURNAL_ENTRIES} entries and {MAX_WATCH_JOURNAL_URI_BYTES} URI bytes may change during one scan"
            ));
            let minimum = self.sequence_after_active();
            return Some(self.arm_recovery(minimum));
        }
        if self.recovery_generation().is_some() && !changes.is_empty() {
            self.rearm_recovery()
        } else {
            None
        }
    }

    fn request_recovery(&mut self, generation: u64) -> Option<WorkspaceScanStart> {
        let minimum = match self.recovery {
            WorkspaceRecoveryState::Debouncing {
                generation: current,
                minimum_scan_sequence,
            } if current == generation => minimum_scan_sequence,
            WorkspaceRecoveryState::Idle
            | WorkspaceRecoveryState::Debouncing { .. }
            | WorkspaceRecoveryState::AwaitingActiveCompletion { .. } => return None,
        };
        if let WorkspaceScanPhase::Running(active) = &self.phase
            && active.accept_result
            && active.sequence >= minimum
        {
            // This worker can still produce a snapshot that contains the
            // recovery lineage. Keep both its replay journal and the recovery
            // reservation until completion proves that installation and replay
            // succeeded.
            self.recovery = WorkspaceRecoveryState::AwaitingActiveCompletion {
                generation,
                minimum_scan_sequence: minimum,
            };
            return None;
        }
        self.recovery = WorkspaceRecoveryState::Idle;
        self.watched_changes.clear();
        if let WorkspaceScanPhase::Running(active) = &mut self.phase {
            // Discarding the journal makes this worker's snapshot impossible to
            // reconcile, even if it was independently acceptable before the
            // recovery timer fired.
            active.accept_result = false;
            self.pending_replacement = true;
            None
        } else {
            Some(self.start())
        }
    }

    fn request_quiet_recovery(&mut self) -> u64 {
        self.arm_recovery(self.active_or_next_sequence())
    }

    fn request_unreplayable_recovery(&mut self) -> u64 {
        let minimum = self.sequence_after_active();
        self.reject_unreplayable_watch();
        self.arm_recovery(minimum)
    }

    fn complete(
        &mut self,
        service: &mut LanguageService,
        scanned: WorkspaceScanned,
    ) -> Option<WorkspaceScanTransition> {
        let completion = self.complete_active(scanned.sequence)?;
        let mut jobs = Vec::new();
        let mut recovery_timer = WorkspaceRecoveryTimerUpdate::Keep;
        if completion.accept_result {
            match scanned.scan {
                Ok(scan) => {
                    let application = service.apply_workspace_scan(scan);
                    jobs.extend(application.jobs);
                    let mut replay_requires_recovery = false;
                    if let Some(changes) = self.watched_changes.take() {
                        let replay = service.workspace_files_changed_with_journal(changes);
                        jobs.extend(replay.jobs);
                        if replay.recovery_required {
                            recovery_timer = WorkspaceRecoveryTimerUpdate::Arm(
                                self.arm_recovery(scanned.sequence.saturating_add(1)),
                            );
                            replay_requires_recovery = true;
                        }
                    }
                    if application.installed
                        && !replay_requires_recovery
                        && self.disarm_recovery_if_covered(scanned.sequence)
                    {
                        recovery_timer = WorkspaceRecoveryTimerUpdate::Cancel;
                    }
                }
                Err(error) => {
                    self.watched_changes.clear();
                    jobs.extend(service.workspace_scan_failed(error));
                }
            }
        } else {
            self.watched_changes.clear();
            if let Some(error) = completion.rejection {
                jobs.extend(service.workspace_scan_failed(error));
            }
        }
        Some(WorkspaceScanTransition {
            jobs,
            next: completion.next,
            recovery_timer,
        })
    }
}

struct AnalysisTask {
    generation: u64,
    handle: tokio::task::JoinHandle<()>,
}

struct AnalysisCompleted {
    job: AnalysisJob,
    result: Result<adocweave::AnalysisResult, String>,
    workspace_result: Option<Result<WorkspaceAnalysis, WorkspaceProblem>>,
    missing_resource: Option<adocweave_workspace::ResourceId>,
}

/// A workspace read that finished on a worker and is ready to install.
///
/// `sequence` identifies the single active worker. Cancelled results still emit
/// this event so the main loop can start one coalesced replacement without
/// allowing scan workers to overlap.
struct WorkspaceScanned {
    sequence: u64,
    scan: Result<crate::service::WorkspaceScan, String>,
}

struct WorkspaceScanRecovery {
    generation: u64,
}

struct WorkspaceScanTransition {
    jobs: Vec<AnalysisJob>,
    next: Option<WorkspaceScanStart>,
    recovery_timer: WorkspaceRecoveryTimerUpdate,
}

enum WorkspaceRecoveryTimerUpdate {
    Keep,
    Cancel,
    Arm(u64),
}

impl Backend {
    pub(crate) fn router(
        client: ClientSocket,
    ) -> impl async_lsp::LspService<Response = Value, Error = ResponseError> {
        Self::router_with_index(client, Arc::new(NoHostReferenceIndex))
    }

    pub(crate) fn router_with_index(
        client: ClientSocket,
        host_index: Arc<dyn HostReferenceIndex>,
    ) -> impl async_lsp::LspService<Response = Value, Error = ResponseError> {
        let process_monitor = client.clone();
        let mut router = Router::new(Self {
            client,
            service: LanguageService::with_host_index(host_index),
            cpu_limit: Arc::new(Semaphore::new(MAX_CONCURRENT_ANALYSES)),
            analysis_tasks: BTreeMap::new(),
            workspace_scans: WorkspaceScanCoordinator::default(),
            workspace_scan_recovery_timer: WorkspaceScanRecoveryTimer::default(),
        });

        router
            .request::<request::Initialize, _>(|state, params| {
                let response = state.service.initialize(&params);
                async move { Ok(response) }
            })
            .notification::<notification::Initialized>(|state, _| {
                state.register_dynamic_capabilities();
                // The workspace walk runs on a worker rather than here, so the
                // event loop answers requests while every `.adoc` file below
                // the roots is read.
                state.schedule_workspace_scan();
                ControlFlow::Continue(())
            })
            .request::<request::Shutdown, _>(|state, _| {
                state.cancel_all_analysis();
                state.invalidate_workspace_scan();
                async move { Ok(()) }
            })
            .notification::<notification::Exit>(|state, _| {
                state.cancel_all_analysis();
                state.invalidate_workspace_scan();
                ControlFlow::Continue(())
            })
            .notification::<notification::DidOpenTextDocument>(|state, params| {
                for job in state.service.begin_open(params) {
                    state.schedule_analysis(job);
                }
                ControlFlow::Continue(())
            })
            .notification::<notification::DidChangeTextDocument>(|state, params| {
                match state.service.begin_change(params) {
                    Ok(jobs) => {
                        for job in jobs {
                            state.schedule_analysis(job);
                        }
                        ControlFlow::Continue(())
                    }
                    Err(error) => ControlFlow::Break(Err(async_lsp::Error::Routing(error))),
                }
            })
            .notification::<notification::DidSaveTextDocument>(|state, params| {
                state.publish_current_diagnostics(params.text_document.uri)
            })
            .notification::<notification::DidChangeConfiguration>(|state, params| {
                if let Ok(jobs) = state.service.update_configuration(params.settings) {
                    for job in jobs {
                        state.schedule_analysis(job);
                    }
                }
                ControlFlow::Continue(())
            })
            .notification::<notification::DidChangeWatchedFiles>(|state, params| {
                let project_configuration_changed = params.changes.iter().any(|change| {
                    change.uri.path_segments().and_then(Iterator::last)
                        == Some(adocweave_config::FILE_NAME)
                });
                if project_configuration_changed {
                    state.schedule_workspace_scan();
                } else {
                    let changes = state.service.workspace_files_changed_with_journal(params);
                    let recovery_generation =
                        state.workspace_scans.record_workspace_changes(&changes);
                    if let Some(generation) = recovery_generation {
                        state.schedule_workspace_scan_recovery(generation);
                    }
                    for job in changes.jobs {
                        state.schedule_analysis(job);
                    }
                }
                ControlFlow::Continue(())
            })
            .notification::<notification::DidChangeWorkspaceFolders>(|state, params| {
                if state.service.workspace_folders_changed(params) {
                    state.schedule_workspace_scan();
                }
                ControlFlow::Continue(())
            })
            .notification::<notification::DidCloseTextDocument>(|state, params| {
                let uri = params.text_document.uri;
                state.cancel_analysis(uri.as_str());
                let (_, jobs) = state.service.close(&uri);
                for job in jobs {
                    state.schedule_analysis(job);
                }
                state.publish_current_diagnostics(uri)
            })
            .request::<request::DocumentSymbolRequest, _>(|state, params| {
                state.cpu_request(
                    params.text_document.uri,
                    move |service, uri, cancellation| {
                        service.document_symbols_cancellable(uri, cancellation)
                    },
                )
            })
            .request::<request::CodeActionRequest, _>(|state, params| {
                let range = params.range;
                let context = params.context;
                state.cpu_request(
                    params.text_document.uri,
                    move |service, uri, cancellation| {
                        service.code_actions_cancellable(uri, range, &context, cancellation)
                    },
                )
            })
            .request::<request::Formatting, _>(|state, params| {
                state.cpu_request(
                    params.text_document.uri,
                    move |service, uri, cancellation| {
                        service.formatting_cancellable(uri, cancellation)
                    },
                )
            })
            .request::<request::HoverRequest, _>(|state, params| {
                let request = params.text_document_position_params;
                let position = request.position;
                state.cpu_request(
                    request.text_document.uri,
                    move |service, uri, cancellation| {
                        service.hover_cancellable(uri, position, cancellation)
                    },
                )
            })
            .request::<request::Completion, _>(|state, params| {
                let request = params.text_document_position;
                let position = request.position;
                state.cpu_request(
                    request.text_document.uri,
                    move |service, uri, cancellation| {
                        service.completion_cancellable(uri, position, cancellation)
                    },
                )
            })
            .request::<request::GotoDefinition, _>(|state, params| {
                let request = params.text_document_position_params;
                let position = request.position;
                state.cpu_request(
                    request.text_document.uri,
                    move |service, uri, cancellation| {
                        service.definition_cancellable(uri, position, cancellation)
                    },
                )
            })
            .request::<request::References, _>(|state, params| {
                let request = params.text_document_position;
                let position = request.position;
                let include_declaration = params.context.include_declaration;
                state.cpu_request(
                    request.text_document.uri,
                    move |service, uri, cancellation| {
                        service.references_cancellable(
                            uri,
                            position,
                            include_declaration,
                            cancellation,
                        )
                    },
                )
            })
            .request::<request::DocumentLinkRequest, _>(|state, params| {
                state.cpu_request(
                    params.text_document.uri,
                    move |service, uri, cancellation| {
                        service.document_links_cancellable(uri, cancellation)
                    },
                )
            })
            .request::<request::SemanticTokensFullRequest, _>(|state, params| {
                state.cpu_request(
                    params.text_document.uri,
                    move |service, uri, cancellation| {
                        service.semantic_tokens_cancellable(uri, cancellation)
                    },
                )
            })
            .request::<request::PrepareRenameRequest, _>(|state, params| {
                let position = params.position;
                state.cpu_request(
                    params.text_document.uri,
                    move |service, uri, cancellation| {
                        service.prepare_rename_cancellable(uri, position, cancellation)
                    },
                )
            })
            .request::<request::Rename, _>(|state, params| {
                let request = params.text_document_position;
                let position = request.position;
                let new_name = params.new_name;
                state.cpu_request(
                    request.text_document.uri,
                    move |service, uri, cancellation| {
                        service.rename_cancellable(uri, position, &new_name, cancellation)
                    },
                )
            })
            .event::<AnalysisCompleted>(|state, completed| state.analysis_completed(completed))
            .event::<WorkspaceScanned>(|state, scanned| {
                let Some(transition) = state.workspace_scans.complete(&mut state.service, scanned)
                else {
                    return ControlFlow::Continue(());
                };
                for job in transition.jobs {
                    state.schedule_analysis(job);
                }
                if let Some(next) = transition.next {
                    state.spawn_workspace_scan(next);
                }
                match transition.recovery_timer {
                    WorkspaceRecoveryTimerUpdate::Keep => {}
                    WorkspaceRecoveryTimerUpdate::Cancel => {
                        state.cancel_workspace_scan_recovery();
                    }
                    WorkspaceRecoveryTimerUpdate::Arm(generation) => {
                        state.schedule_workspace_scan_recovery(generation);
                    }
                }
                ControlFlow::Continue(())
            })
            .event::<WorkspaceScanRecovery>(|state, recovery| {
                if state.workspace_scans.debouncing_generation() != Some(recovery.generation) {
                    return ControlFlow::Continue(());
                }
                if !state
                    .workspace_scan_recovery_timer
                    .complete(recovery.generation)
                {
                    return ControlFlow::Continue(());
                }
                if let Some(start) = state.workspace_scans.request_recovery(recovery.generation) {
                    state.spawn_workspace_scan(start);
                }
                ControlFlow::Continue(())
            });

        ServiceBuilder::new()
            .layer(TracingLayer::default())
            .layer(ProtocolLifecycleLayer)
            .layer(CatchUnwindLayer::default())
            .layer(ConcurrencyLayer::new(
                NonZeroUsize::new(MAX_CONCURRENT_REQUESTS).expect("non-zero request limit"),
            ))
            .layer(ClientProcessMonitorLayer::new(process_monitor))
            .service(router)
    }

    fn register_dynamic_capabilities(&mut self) {
        let Some(params) = self.service.watched_files_registration() else {
            return;
        };
        let client = self.client.clone();
        tokio::spawn(async move {
            let _ = client.request::<request::RegisterCapability>(params).await;
        });
    }

    /// Runs a read-only language request on the CPU pool with the shared
    /// cancellation and concurrency policy, resolving the document cancellation
    /// token before the request is scheduled.
    fn cpu_request<T, F>(
        &self,
        uri: Url,
        operation: F,
    ) -> impl std::future::Future<Output = Result<T, ResponseError>> + Send + use<T, F>
    where
        T: Send + 'static,
        F: FnOnce(&LanguageService, &Url, &QueryCancellation) -> QueryResult<T> + Send + 'static,
    {
        let cancellation = self.service.document_cancellation(&uri);
        let service = self.service.clone();
        let limit = self.cpu_limit.clone();
        async move {
            run_cpu_request(limit, cancellation, move |cancellation| {
                operation(&service, &uri, cancellation)
            })
            .await
        }
    }

    /// Reads the workspace roots on a worker and installs the result later.
    ///
    /// The walk takes time proportional to the workspace, so running it here
    /// would stop the event loop from answering anything until it finished.
    /// A replacement request cancels the active worker but waits for its
    /// completion event before starting the next worker.
    fn schedule_workspace_scan(&mut self) {
        self.cancel_workspace_scan_recovery();
        let Some(start) = self.workspace_scans.request_replacement() else {
            return;
        };
        self.spawn_workspace_scan(start);
    }

    fn spawn_workspace_scan(&self, start: WorkspaceScanStart) {
        let WorkspaceScanStart {
            sequence,
            cancellation,
        } = start;
        let service = self.service.clone();
        let client = self.client.clone();
        tokio::spawn(async move {
            let worker_cancellation = Arc::clone(&cancellation);
            let scan = tokio::task::spawn_blocking(move || {
                service.plan_workspace_scan(worker_cancellation.as_ref())
            })
            .await
            .map_err(|error| format!("workspace scan worker failed: {error}"));
            let _ = client.emit(WorkspaceScanned { sequence, scan });
        });
    }

    fn schedule_workspace_scan_recovery(&mut self, generation: u64) {
        self.cancel_workspace_scan_recovery();
        let client = self.client.clone();
        self.workspace_scan_recovery_timer.replace(
            generation,
            tokio::spawn(async move {
                tokio::time::sleep(std::time::Duration::from_millis(
                    WATCH_SCAN_RECOVERY_DEBOUNCE_MS,
                ))
                .await;
                let _ = client.emit(WorkspaceScanRecovery { generation });
            }),
        );
    }

    fn cancel_workspace_scan_recovery(&mut self) {
        self.workspace_scan_recovery_timer.cancel();
    }

    fn invalidate_workspace_scan(&mut self) {
        self.cancel_workspace_scan_recovery();
        self.workspace_scans.cancel();
    }

    fn schedule_analysis(&mut self, job: AnalysisJob) {
        self.schedule_analysis_with_delay(job, self.service.debounce_ms());
    }

    fn schedule_analysis_immediately(&mut self, job: AnalysisJob) {
        self.schedule_analysis_with_delay(job, 0);
    }

    fn schedule_analysis_with_delay(&mut self, job: AnalysisJob, debounce_ms: u64) {
        self.cancel_analysis(&job.uri);
        let limit = self.cpu_limit.clone();
        let client = self.client.clone();
        let uri = job.uri.clone();
        let generation = job.request.revision.generation;
        let handle = tokio::spawn(async move {
            if debounce_ms > 0 {
                tokio::time::sleep(std::time::Duration::from_millis(debounce_ms)).await;
            }
            let Ok(_permit) = limit.acquire_owned().await else {
                return;
            };
            if job.cancellation.is_cancelled() {
                return;
            }
            let worker_job = job.clone();
            let result = tokio::task::spawn_blocking(move || {
                let result = worker_job
                    .request
                    .analyze(worker_job.cancellation.as_ref())
                    .map_err(|error| error.to_string());
                let mut missing_resource = None;
                let workspace_result =
                    worker_job.workspace_problem.clone().map(Err).or_else(|| {
                        worker_job.workspace.as_ref().map(|input| {
                            input
                                .analyze(
                                    &worker_job.request.options,
                                    worker_job.cancellation.as_ref(),
                                )
                                .map_err(|error| {
                                    missing_resource = error.requested_resource().cloned();
                                    WorkspaceProblem {
                                        source_id: error
                                            .source_id
                                            .as_ref()
                                            .map(ToString::to_string),
                                        range: error.range.unwrap_or_else(zero_range),
                                        code: error.diagnostic_code().to_owned(),
                                        message: error.to_string(),
                                    }
                                })
                        })
                    });
                (result, workspace_result, missing_resource)
            })
            .await
            .unwrap_or_else(|error| (Err(format!("analysis worker failed: {error}")), None, None));
            let _ = client.emit(AnalysisCompleted {
                job,
                result: result.0,
                workspace_result: result.1,
                missing_resource: result.2,
            });
        });
        self.analysis_tasks
            .insert(uri, AnalysisTask { generation, handle });
    }

    fn analysis_completed(
        &mut self,
        completed: AnalysisCompleted,
    ) -> ControlFlow<async_lsp::Result<()>> {
        if self
            .analysis_tasks
            .get(&completed.job.uri)
            .is_some_and(|task| task.generation == completed.job.request.revision.generation)
        {
            self.analysis_tasks.remove(&completed.job.uri);
        }
        if let Some(retry) = self.service.refresh_stale_workspace(&completed.job) {
            self.schedule_analysis_immediately(retry);
            return ControlFlow::Continue(());
        }
        let mut resolution_problem = None;
        if let Some(target) = &completed.missing_resource {
            match self.service.resolve_missing_include(&completed.job, target) {
                Ok(Some(retry)) => {
                    self.schedule_analysis_immediately(retry);
                    return ControlFlow::Continue(());
                }
                Ok(None) => {}
                Err(message) => {
                    let original = completed
                        .workspace_result
                        .as_ref()
                        .and_then(|result| result.as_ref().err());
                    resolution_problem = Some(WorkspaceProblem {
                        source_id: original.and_then(|problem| problem.source_id.clone()),
                        range: original.map_or_else(zero_range, |problem| problem.range),
                        code: "workspace-input-error".to_owned(),
                        message,
                    });
                }
            }
        }
        let Ok(analysis) = completed.result else {
            return ControlFlow::Continue(());
        };
        if self.service.adopt(&completed.job, analysis) != Adoption::Adopted {
            return ControlFlow::Continue(());
        }
        let mut publish_uris = std::collections::BTreeSet::from([completed.job.uri.clone()]);
        if let Some(problem) = resolution_problem {
            let _ = self
                .service
                .adopt_workspace_problem(&completed.job, problem);
        } else if let Some(workspace) = completed.workspace_result {
            match workspace {
                Ok(workspace) => {
                    publish_uris.extend(
                        workspace
                            .source_ids()
                            .into_iter()
                            .map(|source_id| source_id.to_string()),
                    );
                    let _ = self.service.adopt_workspace(&completed.job, workspace);
                }
                Err(problem) => {
                    let _ = self
                        .service
                        .adopt_workspace_problem(&completed.job, problem);
                }
            }
        }
        for uri in publish_uris {
            let Ok(uri) = uri.parse() else {
                return ControlFlow::Break(Err(async_lsp::Error::Routing(format!(
                    "invalid projected source URI: {uri}"
                ))));
            };
            if let ControlFlow::Break(error) = self.publish_current_diagnostics(uri) {
                return ControlFlow::Break(error);
            }
        }
        ControlFlow::Continue(())
    }

    fn cancel_analysis(&mut self, uri: &str) {
        if let Some(task) = self.analysis_tasks.remove(uri) {
            task.handle.abort();
        }
    }

    fn cancel_all_analysis(&mut self) {
        self.service.cancel_all();
        for (_, task) in std::mem::take(&mut self.analysis_tasks) {
            task.handle.abort();
        }
    }

    fn publish_current_diagnostics(&mut self, uri: Url) -> ControlFlow<async_lsp::Result<()>> {
        let result = self
            .service
            .diagnostics(&uri)
            .map_err(async_lsp::Error::Routing)
            .and_then(|params: PublishDiagnosticsParams| {
                self.client
                    .notify::<notification::PublishDiagnostics>(params)?;
                Ok(())
            });
        match result {
            Ok(()) => ControlFlow::Continue(()),
            Err(error) => ControlFlow::Break(Err(error)),
        }
    }
}

fn zero_range() -> adocweave::text::TextRange {
    adocweave::text::TextRange::new(
        adocweave::text::TextSize::ZERO,
        adocweave::text::TextSize::ZERO,
    )
    .expect("zero range is ordered")
}

struct CancelWorkerOnDrop(Arc<CancellationToken>);

impl Drop for CancelWorkerOnDrop {
    fn drop(&mut self) {
        self.0.cancel();
    }
}

async fn run_cpu_request<T, F>(
    limit: Arc<Semaphore>,
    document_cancellation: Option<Arc<CancellationToken>>,
    operation: F,
) -> Result<T, ResponseError>
where
    T: Send + 'static,
    F: FnOnce(&QueryCancellation) -> QueryResult<T> + Send + 'static,
{
    run_cpu_request_with_completion_hook(limit, document_cancellation, operation, || {}).await
}

async fn run_cpu_request_with_completion_hook<T, F, H>(
    limit: Arc<Semaphore>,
    document_cancellation: Option<Arc<CancellationToken>>,
    operation: F,
    after_worker: H,
) -> Result<T, ResponseError>
where
    T: Send + 'static,
    F: FnOnce(&QueryCancellation) -> QueryResult<T> + Send + 'static,
    H: FnOnce(),
{
    let request_cancellation = Arc::new(CancellationToken::new());
    let cancel_on_drop = CancelWorkerOnDrop(request_cancellation.clone());
    let permit = limit
        .acquire_owned()
        .await
        .map_err(|error| internal_error(error.to_string()))?;
    let cancellation = Arc::new(QueryCancellation::new(
        request_cancellation,
        document_cancellation,
    ));
    cancellation.check_now().map_err(query_response_error)?;
    let worker_cancellation = cancellation.clone();
    let result = tokio::task::spawn_blocking(move || {
        let _permit = permit;
        (|| {
            worker_cancellation.check_now()?;
            let result = operation(&worker_cancellation);
            worker_cancellation.check_now()?;
            result
        })()
    })
    .await;
    after_worker();
    let response = finish_cpu_request(&cancellation, result);
    drop(cancel_on_drop);
    response
}

fn finish_cpu_request<T>(
    cancellation: &QueryCancellation,
    result: Result<QueryResult<T>, tokio::task::JoinError>,
) -> Result<T, ResponseError> {
    cancellation.check_now().map_err(query_response_error)?;
    let result =
        result.map_err(|error| internal_error(format!("request worker failed: {error}")))?;
    result.map_err(query_response_error)
}

fn query_response_error(error: QueryError) -> ResponseError {
    match error {
        QueryError::RequestCancelled => {
            ResponseError::new(ErrorCode::REQUEST_CANCELLED, "request was cancelled")
        }
        QueryError::ContentModified => content_modified(),
        QueryError::Internal(message) => internal_error(message),
    }
}

fn internal_error(error: impl ToString) -> ResponseError {
    ResponseError::new(ErrorCode::INTERNAL_ERROR, error.to_string())
}

fn content_modified() -> ResponseError {
    ResponseError::new(
        ErrorCode::CONTENT_MODIFIED,
        "document changed while the request was running",
    )
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::mpsc;
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    use serde_json::json;

    use super::*;

    struct NotifyOnDrop(mpsc::Sender<()>);

    impl Drop for NotifyOnDrop {
        fn drop(&mut self) {
            let _ = self.0.send(());
        }
    }

    fn scan_race_service(prefix: &str) -> (std::path::PathBuf, Url, LanguageService) {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let root = std::env::temp_dir().join(format!("{prefix}-{unique}"));
        fs::create_dir_all(&root).expect("workspace");
        let document_path = root.join("root.adoc");
        fs::write(&document_path, "= Before\n").expect("initial document");
        let root_uri = Url::from_directory_path(&root).expect("root URI");
        let document_uri = Url::from_file_path(&document_path).expect("document URI");
        let params = serde_json::from_value(json!({
            "processId": null,
            "rootUri": root_uri,
            "capabilities": {}
        }))
        .expect("initialize params");
        let mut service = LanguageService::default();
        service.initialize(&params);
        let scan = service.plan_workspace_scan(&adocweave::NeverCancel);
        let _ = service.apply_workspace_scan(scan);
        (root, document_uri, service)
    }

    #[test]
    fn replacement_scans_are_coalesced_without_overlapping_workers() {
        let mut coordinator = WorkspaceScanCoordinator::default();
        let old = coordinator.request_replacement().expect("initial scan");

        for _ in 0..100 {
            assert!(coordinator.request_replacement().is_none());
        }

        assert!(old.cancellation.is_cancelled());
        assert!(!coordinator.accepts_active_result());
        let completion = coordinator
            .complete_active(old.sequence)
            .expect("old completion");
        assert!(!completion.accept_result);
        let new = completion.next.expect("one replacement");
        assert!(!new.cancellation.is_cancelled());

        let completion = coordinator
            .complete_active(new.sequence)
            .expect("new completion");
        assert!(completion.accept_result);
        assert!(completion.next.is_none());
    }

    #[test]
    fn shutdown_cancels_the_active_scan_and_discards_pending_work() {
        let mut coordinator = WorkspaceScanCoordinator::default();
        let active = coordinator.request_replacement().expect("initial scan");
        assert!(coordinator.request_replacement().is_none());

        coordinator.cancel();

        assert!(active.cancellation.is_cancelled());
        let completion = coordinator
            .complete_active(active.sequence)
            .expect("completion");
        assert!(!completion.accept_result);
        assert!(completion.next.is_none());
    }

    #[test]
    fn stale_scan_completion_cannot_replace_the_active_scan() {
        let mut coordinator = WorkspaceScanCoordinator::default();
        let active = coordinator.request_replacement().expect("initial scan");

        assert!(coordinator.complete_active(active.sequence + 1).is_none());
        assert!(coordinator.accepts_active_result());
        assert!(
            coordinator
                .complete_active(active.sequence)
                .expect("active completion")
                .accept_result
        );
    }

    #[test]
    fn continuous_watched_changes_do_not_cancel_or_restart_the_active_scan() {
        let mut coordinator = WorkspaceScanCoordinator::default();
        let active = coordinator.request_replacement().expect("initial scan");
        let uri = Url::parse("file:///workspace/root.adoc").expect("URI");

        for _ in 0..100 {
            assert!(
                coordinator
                    .record_watched_changes(&[
                        FileEvent::new(uri.clone(), FileChangeType::CHANGED,)
                    ])
                    .is_none()
            );
        }

        assert!(!active.cancellation.is_cancelled());
        assert_eq!(coordinator.watched_changes.changes.len(), 1);
        let completion = coordinator
            .complete_active(active.sequence)
            .expect("scan completion");
        assert!(completion.accept_result);
        assert!(completion.next.is_none());
    }

    #[test]
    fn structural_replacement_discards_watch_state_and_stale_recovery() {
        let mut coordinator = WorkspaceScanCoordinator::default();
        let active = coordinator.request_replacement().expect("initial scan");
        let uri = Url::parse("file:///workspace/root.adoc").expect("URI");
        let _ = coordinator.record_watched_changes(&[FileEvent::new(uri, FileChangeType::CHANGED)]);
        let stale_recovery = coordinator.request_quiet_recovery();

        assert!(coordinator.request_replacement().is_none());
        assert!(active.cancellation.is_cancelled());
        assert!(coordinator.watched_changes.changes.is_empty());
        assert!(coordinator.recovery_generation().is_none());
        assert!(coordinator.request_recovery(stale_recovery).is_none());

        let completion = coordinator
            .complete_active(active.sequence)
            .expect("cancelled completion");
        assert!(!completion.accept_result);
        assert!(completion.next.is_some());
    }

    #[test]
    fn recovery_generation_represents_one_replaceable_timer() {
        let mut coordinator = WorkspaceScanCoordinator::default();
        let first = coordinator.request_quiet_recovery();
        let second = coordinator.request_quiet_recovery();
        let latest = coordinator.request_quiet_recovery();

        assert_ne!(first, second);
        assert!(matches!(
            coordinator.recovery,
            WorkspaceRecoveryState::Debouncing {
                generation,
                minimum_scan_sequence: 1,
            } if generation == latest
        ));
        assert_eq!(coordinator.recovery_generation(), Some(latest));
        assert!(coordinator.request_recovery(first).is_none());
        assert_eq!(coordinator.debouncing_generation(), Some(latest));

        let scan = coordinator
            .request_recovery(latest)
            .expect("latest timer starts one scan");
        assert!(coordinator.recovery_generation().is_none());
        assert!(coordinator.request_recovery(second).is_none());
        assert!(!scan.cancellation.is_cancelled());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn replacing_and_dropping_the_recovery_timer_aborts_its_task() {
        let (first_tx, first_rx) = mpsc::channel();
        let (second_tx, second_rx) = mpsc::channel();
        let (first_started_tx, first_started_rx) = tokio::sync::oneshot::channel();
        let (second_started_tx, second_started_rx) = tokio::sync::oneshot::channel();
        let mut timer = WorkspaceScanRecoveryTimer::default();
        timer.replace(
            1,
            tokio::spawn(async move {
                let _on_drop = NotifyOnDrop(first_tx);
                let _ = first_started_tx.send(());
                std::future::pending::<()>().await;
            }),
        );
        assert_eq!(timer.generation(), Some(1));
        first_started_rx.await.expect("first timer started");
        timer.replace(
            2,
            tokio::spawn(async move {
                let _on_drop = NotifyOnDrop(second_tx);
                let _ = second_started_tx.send(());
                std::future::pending::<()>().await;
            }),
        );
        assert_eq!(timer.generation(), Some(2));
        second_started_rx.await.expect("second timer started");
        assert!(!timer.complete(1));
        assert_eq!(timer.generation(), Some(2));

        first_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("replacing the timer aborts the previous task");
        drop(timer);
        second_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("dropping the timer aborts the retained task");

        let mut completed = WorkspaceScanRecoveryTimer::default();
        completed.replace(3, tokio::spawn(async {}));
        assert!(completed.complete(3));
        assert_eq!(completed.generation(), None);
    }

    #[test]
    fn watched_change_journal_coalesces_and_bounds_uris() {
        let first = Url::parse("file:///workspace/first.adoc").expect("first URI");
        let second = Url::parse("file:///workspace/second.adoc").expect("second URI");
        let mut journal = WatchedChangeJournal::default();

        assert!(journal.record_with_limits(
            &[
                FileEvent::new(first.clone(), FileChangeType::CREATED),
                FileEvent::new(second.clone(), FileChangeType::CHANGED),
                FileEvent::new(first.clone(), FileChangeType::DELETED),
            ],
            2,
            first.as_str().len() + second.as_str().len(),
        ));
        let replay = journal.take().expect("replay");
        assert_eq!(
            replay.changes,
            vec![
                FileEvent::new(first.clone(), FileChangeType::DELETED),
                FileEvent::new(second.clone(), FileChangeType::CHANGED),
            ]
        );

        assert!(!journal.record_with_limits(
            &[
                FileEvent::new(first, FileChangeType::CHANGED),
                FileEvent::new(second, FileChangeType::CHANGED),
            ],
            1,
            usize::MAX,
        ));
        assert!(journal.take().is_none());
        assert!(journal.changes.is_empty());
    }

    #[test]
    fn journal_overflow_waits_for_quiet_before_restarting_the_worker() {
        let mut coordinator = WorkspaceScanCoordinator::default();
        let active = coordinator.request_replacement().expect("initial scan");
        let uri = Url::parse("file:///workspace/root.adoc").expect("URI");

        assert!(!coordinator.watched_changes.record_with_limits(
            &[FileEvent::new(uri.clone(), FileChangeType::CHANGED)],
            0,
            usize::MAX,
        ));
        let recovery = coordinator
            .record_watched_changes(&[FileEvent::new(uri, FileChangeType::CHANGED)])
            .expect("recovery generation");

        assert!(!active.cancellation.is_cancelled());
        assert!(!coordinator.accepts_active_result());
        assert!(coordinator.request_recovery(recovery).is_none());
        assert!(!active.cancellation.is_cancelled());
        let completion = coordinator
            .complete_active(active.sequence)
            .expect("completion");
        assert!(!completion.accept_result);
        assert!(completion.next.is_some());
        assert!(
            completion
                .rejection
                .as_deref()
                .is_some_and(|message| message.contains("watch journal limit exceeded"))
        );
    }

    #[test]
    fn accepted_scan_replays_watched_changes_after_installing_its_snapshot() {
        let (root, document_uri, mut service) = scan_race_service("adocweave-scan-replay");
        let mut coordinator = WorkspaceScanCoordinator::default();
        let active = coordinator.request_replacement().expect("active scan");
        let stale_scan = service.plan_workspace_scan(&adocweave::NeverCancel);
        fs::write(document_uri.to_file_path().expect("path"), "= After\n")
            .expect("changed document");
        let changes = vec![FileEvent::new(
            document_uri.clone(),
            FileChangeType::CHANGED,
        )];
        let _ = coordinator.record_watched_changes(&changes);
        let _ =
            service.workspace_files_changed_with_journal(DidChangeWatchedFilesParams { changes });

        let transition = coordinator
            .complete(
                &mut service,
                WorkspaceScanned {
                    sequence: active.sequence,
                    scan: Ok(stale_scan),
                },
            )
            .expect("scan completion");

        assert!(transition.next.is_none());
        assert_eq!(
            service
                .workspace_resource(&document_uri)
                .expect("updated resource")
                .as_ref(),
            "= After\n",
        );
        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn successful_scan_cancels_an_older_recovery_reservation() {
        let (root, _, mut service) = scan_race_service("adocweave-scan-clears-recovery");
        let mut coordinator = WorkspaceScanCoordinator::default();
        let active = coordinator.request_replacement().expect("active scan");
        let stale_recovery = coordinator.request_quiet_recovery();
        let scan = service.plan_workspace_scan(&adocweave::NeverCancel);

        let transition = coordinator
            .complete(
                &mut service,
                WorkspaceScanned {
                    sequence: active.sequence,
                    scan: Ok(scan),
                },
            )
            .expect("scan completion");

        assert!(matches!(
            transition.recovery_timer,
            WorkspaceRecoveryTimerUpdate::Cancel
        ));
        assert!(coordinator.recovery_generation().is_none());
        assert!(coordinator.request_recovery(stale_recovery).is_none());
        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn recovery_timer_before_completion_preserves_the_replay_journal() {
        let (root, document_uri, mut service) =
            scan_race_service("adocweave-recovery-before-completion");
        let mut coordinator = WorkspaceScanCoordinator::default();
        let active = coordinator.request_replacement().expect("active scan");
        let stale_scan = service.plan_workspace_scan(&adocweave::NeverCancel);
        fs::write(document_uri.to_file_path().expect("path"), "= Current\n")
            .expect("changed document");
        let changes = service.workspace_files_changed_with_journal(DidChangeWatchedFilesParams {
            changes: vec![FileEvent::new(
                document_uri.clone(),
                FileChangeType::CHANGED,
            )],
        });
        assert!(coordinator.record_workspace_changes(&changes).is_none());
        let recovery = coordinator.request_quiet_recovery();

        assert!(matches!(
            coordinator.recovery,
            WorkspaceRecoveryState::Debouncing {
                generation,
                minimum_scan_sequence,
            } if generation == recovery && minimum_scan_sequence == active.sequence
        ));
        assert!(coordinator.request_recovery(recovery).is_none());
        assert!(coordinator.accepts_active_result());
        assert_eq!(coordinator.watched_changes.changes.len(), 1);
        assert!(!coordinator.pending_replacement);
        assert!(matches!(
            coordinator.recovery,
            WorkspaceRecoveryState::AwaitingActiveCompletion {
                generation,
                minimum_scan_sequence,
            } if generation == recovery && minimum_scan_sequence == active.sequence
        ));
        assert!(coordinator.debouncing_generation().is_none());
        assert!(coordinator.request_recovery(recovery).is_none());

        let transition = coordinator
            .complete(
                &mut service,
                WorkspaceScanned {
                    sequence: active.sequence,
                    scan: Ok(stale_scan),
                },
            )
            .expect("scan completion");
        assert!(transition.next.is_none());
        assert!(matches!(
            transition.recovery_timer,
            WorkspaceRecoveryTimerUpdate::Cancel
        ));
        assert!(matches!(coordinator.recovery, WorkspaceRecoveryState::Idle));
        assert_eq!(
            service
                .workspace_resource(&document_uri)
                .expect("replayed resource")
                .as_ref(),
            "= Current\n"
        );
        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn watched_change_rearms_recovery_while_active_completion_is_awaited() {
        let mut coordinator = WorkspaceScanCoordinator::default();
        let active = coordinator.request_replacement().expect("active scan");
        let first = coordinator.request_quiet_recovery();

        assert!(coordinator.request_recovery(first).is_none());
        assert!(matches!(
            coordinator.recovery,
            WorkspaceRecoveryState::AwaitingActiveCompletion {
                generation,
                minimum_scan_sequence,
            } if generation == first && minimum_scan_sequence == active.sequence
        ));

        let uri = Url::parse("file:///workspace/changed.adoc").expect("URI");
        let next = coordinator
            .record_watched_changes(&[FileEvent::new(uri, FileChangeType::CHANGED)])
            .expect("new timer generation");

        assert_ne!(next, first);
        assert!(matches!(
            coordinator.recovery,
            WorkspaceRecoveryState::Debouncing {
                generation,
                minimum_scan_sequence,
            } if generation == next && minimum_scan_sequence == active.sequence
        ));
        assert_eq!(coordinator.debouncing_generation(), Some(next));
        assert!(coordinator.request_recovery(first).is_none());
        assert_eq!(coordinator.debouncing_generation(), Some(next));
    }

    #[test]
    fn failed_successor_scan_preserves_incremental_state_after_recovery_timer() {
        let (root, document_uri, mut service) =
            scan_race_service("adocweave-failed-recovery-successor");
        let mut coordinator = WorkspaceScanCoordinator::default();
        let active = coordinator.request_replacement().expect("active scan");
        let stale_scan = service.plan_workspace_scan(&adocweave::NeverCancel);
        fs::write(document_uri.to_file_path().expect("path"), "= Current\n")
            .expect("changed document");
        let changes = service.workspace_files_changed_with_journal(DidChangeWatchedFilesParams {
            changes: vec![FileEvent::new(
                document_uri.clone(),
                FileChangeType::CHANGED,
            )],
        });
        let _ = coordinator.record_workspace_changes(&changes);
        let recovery = coordinator.request_unreplayable_recovery();
        assert!(coordinator.request_recovery(recovery).is_none());

        let first = coordinator
            .complete(
                &mut service,
                WorkspaceScanned {
                    sequence: active.sequence,
                    scan: Ok(stale_scan),
                },
            )
            .expect("old scan completion");
        let successor = first.next.expect("one recovery successor");
        assert_eq!(successor.sequence, active.sequence.saturating_add(1));
        assert_eq!(
            service
                .workspace_resource(&document_uri)
                .expect("incremental resource")
                .as_ref(),
            "= Current\n"
        );

        let failed = coordinator
            .complete(
                &mut service,
                WorkspaceScanned {
                    sequence: successor.sequence,
                    scan: Err("recovery worker failed".to_owned()),
                },
            )
            .expect("failed recovery completion");
        assert!(failed.next.is_none());
        assert_eq!(
            service
                .workspace_resource(&document_uri)
                .expect("retained incremental resource")
                .as_ref(),
            "= Current\n"
        );

        let retry_change =
            service.workspace_files_changed_with_journal(DidChangeWatchedFilesParams {
                changes: vec![FileEvent::new(
                    document_uri.clone(),
                    FileChangeType::CHANGED,
                )],
            });
        assert!(retry_change.recovery_required);
        let retry_timer = coordinator
            .record_workspace_changes(&retry_change)
            .expect("retry recovery reservation");
        let retry = coordinator
            .request_recovery(retry_timer)
            .expect("retry scan after quiet period");
        assert!(retry.sequence > successor.sequence);
        let retry_scan = service.plan_workspace_scan(&adocweave::NeverCancel);
        let recovered = coordinator
            .complete(
                &mut service,
                WorkspaceScanned {
                    sequence: retry.sequence,
                    scan: Ok(retry_scan),
                },
            )
            .expect("successful retry completion");
        assert!(matches!(
            recovered.recovery_timer,
            WorkspaceRecoveryTimerUpdate::Cancel
        ));
        assert!(coordinator.recovery_generation().is_none());
        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn configuration_replacement_supersedes_recovery_and_converges_once() {
        let (root, document_uri, mut service) =
            scan_race_service("adocweave-config-recovery-order");
        let mut coordinator = WorkspaceScanCoordinator::default();
        let active = coordinator.request_replacement().expect("active scan");
        let uri_change = FileEvent::new(document_uri, FileChangeType::CHANGED);
        let _ = coordinator.record_watched_changes(&[uri_change]);
        let stale_recovery = coordinator.request_quiet_recovery();

        assert!(coordinator.request_replacement().is_none());
        assert!(active.cancellation.is_cancelled());
        assert!(coordinator.request_recovery(stale_recovery).is_none());
        let replaced = coordinator
            .complete_active(active.sequence)
            .expect("cancelled completion");
        let replacement = replaced.next.expect("one structural replacement");
        assert!(!replacement.cancellation.is_cancelled());
        assert!(coordinator.watched_changes.changes.is_empty());

        let scan = service.plan_workspace_scan(&adocweave::NeverCancel);
        let completed = coordinator
            .complete(
                &mut service,
                WorkspaceScanned {
                    sequence: replacement.sequence,
                    scan: Ok(scan),
                },
            )
            .expect("replacement completion");
        assert!(completed.next.is_none());
        assert!(coordinator.recovery_generation().is_none());
        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn watched_change_after_completion_updates_the_installed_snapshot() {
        let (root, document_uri, mut service) = scan_race_service("adocweave-watch-after-scan");
        let mut coordinator = WorkspaceScanCoordinator::default();
        let active = coordinator.request_replacement().expect("active scan");
        let scan = service.plan_workspace_scan(&adocweave::NeverCancel);
        let transition = coordinator
            .complete(
                &mut service,
                WorkspaceScanned {
                    sequence: active.sequence,
                    scan: Ok(scan),
                },
            )
            .expect("scan completion");
        assert!(transition.next.is_none());

        fs::write(document_uri.to_file_path().expect("path"), "= After\n")
            .expect("changed document");
        let outcome = service.workspace_files_changed_with_journal(DidChangeWatchedFilesParams {
            changes: vec![FileEvent::new(
                document_uri.clone(),
                FileChangeType::CHANGED,
            )],
        });

        assert!(!outcome.recovery_required);
        assert_eq!(
            service
                .workspace_resource(&document_uri)
                .expect("updated resource")
                .as_ref(),
            "= After\n",
        );
        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn replay_propagates_a_new_recovery_requirement() {
        let (root, _, mut service) = scan_race_service("adocweave-scan-replay-recovery");
        let mut coordinator = WorkspaceScanCoordinator::default();
        let active = coordinator.request_replacement().expect("active scan");
        let scan = service.plan_workspace_scan(&adocweave::NeverCancel);
        let changes = (0..129)
            .map(|index| {
                let path = root.join(format!("missing-{index}.adoc"));
                FileEvent::new(
                    Url::from_file_path(path).expect("file URI"),
                    FileChangeType::CREATED,
                )
            })
            .collect::<Vec<_>>();
        assert!(coordinator.record_watched_changes(&changes).is_none());
        let first_pass =
            service.workspace_files_changed_with_journal(DidChangeWatchedFilesParams {
                changes: changes.clone(),
            });
        assert!(first_pass.recovery_required);

        let transition = coordinator
            .complete(
                &mut service,
                WorkspaceScanned {
                    sequence: active.sequence,
                    scan: Ok(scan),
                },
            )
            .expect("scan completion");

        let WorkspaceRecoveryTimerUpdate::Arm(recovery) = transition.recovery_timer else {
            panic!("replay must arm recovery");
        };
        assert_eq!(coordinator.recovery_generation(), Some(recovery));
        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn unreplayable_watch_batch_survives_completion_of_the_older_scan() {
        let (root, document_uri, mut service) = scan_race_service("adocweave-unreplayable-watch");
        let mut coordinator = WorkspaceScanCoordinator::default();
        let active = coordinator.request_replacement().expect("active scan");
        let stale_scan = service.plan_workspace_scan(&adocweave::NeverCancel);

        fs::write(document_uri.to_file_path().expect("path"), "= Live\n")
            .expect("changed document");
        let replayable =
            service.workspace_files_changed_with_journal(DidChangeWatchedFilesParams {
                changes: vec![FileEvent::new(
                    document_uri.clone(),
                    FileChangeType::CHANGED,
                )],
            });
        assert!(replayable.replay_complete);
        assert!(coordinator.record_workspace_changes(&replayable).is_none());

        let oversized = service.workspace_files_changed_with_journal(DidChangeWatchedFilesParams {
            changes: (0..=10_000)
                .map(|index| {
                    FileEvent::new(
                        Url::from_file_path(root.join(format!("f{index}.adoc"))).expect("file URI"),
                        FileChangeType::CREATED,
                    )
                })
                .collect(),
        });
        assert!(oversized.recovery_required);
        assert!(!oversized.replay_complete);
        let recovery = coordinator
            .record_workspace_changes(&oversized)
            .expect("recovery reservation");
        assert!(!coordinator.accepts_active_result());
        assert_eq!(
            coordinator.recovery_minimum_scan_sequence(),
            Some(active.sequence.saturating_add(1))
        );

        let transition = coordinator
            .complete(
                &mut service,
                WorkspaceScanned {
                    sequence: active.sequence,
                    scan: Ok(stale_scan),
                },
            )
            .expect("old scan completion");

        assert!(matches!(
            transition.recovery_timer,
            WorkspaceRecoveryTimerUpdate::Keep
        ));
        assert_eq!(
            coordinator.recovery_generation(),
            Some(recovery),
            "the older scan cannot discharge recovery that requires its successor"
        );
        assert_eq!(
            service
                .workspace_resource(&document_uri)
                .expect("retained live resource")
                .as_ref(),
            "= Live\n",
        );
        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn journal_overflow_keeps_incremental_state_and_finishes_with_a_bounded_error() {
        let (root, document_uri, mut service) = scan_race_service("adocweave-scan-overflow");
        let mut coordinator = WorkspaceScanCoordinator::default();
        let active = coordinator.request_replacement().expect("active scan");
        let stale_scan = service.plan_workspace_scan(&adocweave::NeverCancel);
        fs::write(document_uri.to_file_path().expect("path"), "= After\n")
            .expect("changed document");
        let changes = vec![FileEvent::new(
            document_uri.clone(),
            FileChangeType::CHANGED,
        )];
        assert!(
            !coordinator
                .watched_changes
                .record_with_limits(&changes, 0, usize::MAX)
        );
        let first_recovery = coordinator
            .record_watched_changes(&changes)
            .expect("first recovery generation");
        let _ =
            service.workspace_files_changed_with_journal(DidChangeWatchedFilesParams { changes });

        let transition = coordinator
            .complete(
                &mut service,
                WorkspaceScanned {
                    sequence: active.sequence,
                    scan: Ok(stale_scan),
                },
            )
            .expect("scan completion");

        assert!(
            transition.next.is_none(),
            "recovery waits for the quiet timer"
        );
        assert_eq!(
            service
                .workspace_resource(&document_uri)
                .expect("incremental resource")
                .as_ref(),
            "= After\n",
            "the rejected snapshot must not replace the watched update",
        );
        let diagnostics = service.diagnostics(&document_uri).expect("diagnostics");
        assert!(diagnostics.diagnostics.iter().any(|diagnostic| {
            diagnostic
                .message
                .contains("workspace watch journal limit exceeded")
        }));
        let final_recovery = coordinator
            .record_watched_changes(&[FileEvent::new(document_uri, FileChangeType::CHANGED)])
            .expect("updated recovery generation");
        assert!(coordinator.request_recovery(first_recovery).is_none());
        let recovery = coordinator
            .request_recovery(final_recovery)
            .expect("one recovery after notifications stop");
        assert!(
            !recovery.cancellation.is_cancelled(),
            "the bounded recovery worker starts after the quiet period"
        );
        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn accepted_worker_failure_is_reported_without_replacing_the_workspace() {
        let (root, document_uri, mut service) = scan_race_service("adocweave-scan-join-error");
        let previous = service
            .workspace_resource(&document_uri)
            .expect("workspace resource")
            .clone();
        let mut coordinator = WorkspaceScanCoordinator::default();
        let active = coordinator.request_replacement().expect("active scan");

        let transition = coordinator
            .complete(
                &mut service,
                WorkspaceScanned {
                    sequence: active.sequence,
                    scan: Err("workspace scan worker failed: panic".to_owned()),
                },
            )
            .expect("scan completion");

        assert!(transition.next.is_none());
        assert_eq!(
            service
                .workspace_resource(&document_uri)
                .expect("retained resource"),
            previous,
        );
        let diagnostics = service.diagnostics(&document_uri).expect("diagnostics");
        assert!(
            diagnostics
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.message.contains("workspace scan worker failed"))
        );
        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn rejected_worker_failure_starts_the_replacement_without_a_diagnostic() {
        let (root, document_uri, mut service) =
            scan_race_service("adocweave-rejected-scan-join-error");
        let mut coordinator = WorkspaceScanCoordinator::default();
        let old = coordinator.request_replacement().expect("active scan");
        assert!(coordinator.request_replacement().is_none());

        let transition = coordinator
            .complete(
                &mut service,
                WorkspaceScanned {
                    sequence: old.sequence,
                    scan: Err("workspace scan worker failed: cancelled panic".to_owned()),
                },
            )
            .expect("scan completion");

        assert!(transition.jobs.is_empty());
        assert!(transition.next.is_some());
        let diagnostics = service.diagnostics(&document_uri).expect("diagnostics");
        assert!(
            diagnostics
                .diagnostics
                .iter()
                .all(|diagnostic| !diagnostic.message.contains("workspace scan worker failed"))
        );
        fs::remove_dir_all(root).expect("cleanup");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn concurrency_cpu_requests_never_exceed_the_explicit_limit() {
        let limit = Arc::new(Semaphore::new(2));
        let active = Arc::new(AtomicUsize::new(0));
        let maximum = Arc::new(AtomicUsize::new(0));
        let requests = (0..8).map(|_| {
            let active = active.clone();
            let maximum = maximum.clone();
            run_cpu_request(limit.clone(), None, move |_| {
                let current = active.fetch_add(1, Ordering::SeqCst) + 1;
                maximum.fetch_max(current, Ordering::SeqCst);
                std::thread::sleep(Duration::from_millis(10));
                active.fetch_sub(1, Ordering::SeqCst);
                Ok(())
            })
        });

        let results = futures::future::join_all(requests).await;
        assert!(results.into_iter().all(|result| result.is_ok()));
        assert_eq!(maximum.load(Ordering::SeqCst), 2);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn cancellation_dropping_a_request_cooperatively_cancels_its_worker() {
        let (started_tx, started_rx) = std::sync::mpsc::channel();
        let (cancelled_tx, cancelled_rx) = std::sync::mpsc::channel();
        let task = tokio::spawn(run_cpu_request(
            Arc::new(Semaphore::new(1)),
            None,
            move |cancellation| {
                started_tx.send(()).expect("started receiver");
                while !cancellation.is_cancelled() {
                    std::thread::yield_now();
                }
                cancelled_tx.send(()).expect("cancelled receiver");
                Err::<(), _>(QueryError::RequestCancelled)
            },
        ));

        started_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("worker started");
        task.abort();
        cancelled_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("worker observed cancellation");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn cancellation_document_change_discards_a_completed_request() {
        let document_cancellation = Arc::new(CancellationToken::new());
        let worker_token = document_cancellation.clone();
        let (started_tx, started_rx) = std::sync::mpsc::channel();
        let (finish_tx, finish_rx) = std::sync::mpsc::channel();
        let task = tokio::spawn(run_cpu_request(
            Arc::new(Semaphore::new(1)),
            Some(worker_token),
            move |_| {
                started_tx.send(()).expect("started receiver");
                finish_rx
                    .recv_timeout(Duration::from_secs(1))
                    .expect("finish signal");
                Ok(())
            },
        ));

        started_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("worker started");
        document_cancellation.cancel();
        finish_tx.send(()).expect("finish receiver");
        let error = task
            .await
            .expect("request task")
            .expect_err("content modified");

        assert_eq!(error.code, ErrorCode::CONTENT_MODIFIED);
    }

    #[test]
    fn document_change_after_worker_completion_overrides_success_and_internal_error() {
        fn assert_content_modified<T: std::fmt::Debug>(result: QueryResult<T>) {
            let document_cancellation = Arc::new(CancellationToken::new());
            let cancellation = QueryCancellation::new(
                Arc::new(CancellationToken::new()),
                Some(document_cancellation.clone()),
            );
            document_cancellation.cancel();

            let error =
                finish_cpu_request(&cancellation, Ok(result)).expect_err("content modified");
            assert_eq!(error.code, ErrorCode::CONTENT_MODIFIED);
        }

        assert_content_modified(Ok("completed result"));
        assert_content_modified(Err::<(), _>(QueryError::Internal(
            "query failed".to_owned(),
        )));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn asynchronous_path_checks_document_change_after_worker_completion() {
        async fn assert_content_modified(result: QueryResult<()>) {
            let document_cancellation = Arc::new(CancellationToken::new());
            let cancel_after_worker = document_cancellation.clone();
            let error = run_cpu_request_with_completion_hook(
                Arc::new(Semaphore::new(1)),
                Some(document_cancellation),
                move |_| result,
                move || cancel_after_worker.cancel(),
            )
            .await
            .expect_err("content modified");
            assert_eq!(error.code, ErrorCode::CONTENT_MODIFIED);
        }

        assert_content_modified(Ok(())).await;
        assert_content_modified(Err(QueryError::Internal("worker error".to_owned()))).await;
    }

    #[test]
    fn query_errors_have_distinct_protocol_codes() {
        assert_eq!(
            query_response_error(QueryError::RequestCancelled).code,
            ErrorCode::REQUEST_CANCELLED
        );
        assert_eq!(
            query_response_error(QueryError::ContentModified).code,
            ErrorCode::CONTENT_MODIFIED
        );
        assert_eq!(
            query_response_error(QueryError::Internal("broken query".to_owned())).code,
            ErrorCode::INTERNAL_ERROR
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn cancelled_workers_release_both_permits_for_the_next_request() {
        let limit = Arc::new(Semaphore::new(2));
        let (started_tx, started_rx) = std::sync::mpsc::channel();
        let (cancelled_tx, cancelled_rx) = std::sync::mpsc::channel();
        let mut tasks = Vec::new();
        for _ in 0..2 {
            let started_tx = started_tx.clone();
            let cancelled_tx = cancelled_tx.clone();
            tasks.push(tokio::spawn(run_cpu_request(
                limit.clone(),
                None,
                move |cancellation| {
                    started_tx.send(()).expect("started receiver");
                    while !cancellation.is_cancelled() {
                        std::thread::yield_now();
                    }
                    cancelled_tx.send(()).expect("cancelled receiver");
                    Err::<(), _>(QueryError::RequestCancelled)
                },
            )));
        }

        for _ in 0..2 {
            started_rx
                .recv_timeout(Duration::from_secs(1))
                .expect("both workers started");
        }
        for task in &tasks {
            task.abort();
        }
        for _ in 0..2 {
            cancelled_rx
                .recv_timeout(Duration::from_secs(1))
                .expect("both workers observed cancellation");
        }

        let result = tokio::time::timeout(
            Duration::from_secs(1),
            run_cpu_request(limit, None, |_| Ok("next request")),
        )
        .await
        .expect("third request acquired a released permit")
        .expect("third request succeeded");
        assert_eq!(result, "next request");
    }
}
