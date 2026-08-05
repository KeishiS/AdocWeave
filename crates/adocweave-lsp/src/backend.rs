//! Typed `async-lsp` adapter with generation-checked background analysis.

use std::collections::BTreeMap;
use std::num::NonZeroUsize;
use std::ops::ControlFlow;
use std::sync::Arc;

use adocweave::{CancellationCheck, CancellationToken};
use adocweave_workspace::WorkspaceAnalysis;
use async_lsp::client_monitor::ClientProcessMonitorLayer;
use async_lsp::concurrency::ConcurrencyLayer;
use async_lsp::lsp_types::{PublishDiagnosticsParams, Url, notification, request};
use async_lsp::panic::CatchUnwindLayer;
use async_lsp::router::Router;
use async_lsp::tracing::TracingLayer;
use async_lsp::{ClientSocket, ErrorCode, ResponseError};
use serde_json::Value;
use tokio::sync::Semaphore;
use tower::ServiceBuilder;

use crate::lifecycle::ProtocolLifecycleLayer;
use crate::service::LanguageService;
use crate::state::{Adoption, AnalysisJob, WorkspaceProblem};
use crate::{HostReferenceIndex, NoHostReferenceIndex};

const MAX_CONCURRENT_REQUESTS: usize = 16;
const MAX_CONCURRENT_ANALYSES: usize = 2;

pub(crate) struct Backend {
    client: ClientSocket,
    service: LanguageService,
    cpu_limit: Arc<Semaphore>,
    analysis_tasks: BTreeMap<String, AnalysisTask>,
    workspace_scan: WorkspaceScanControl,
}

#[derive(Default)]
struct WorkspaceScanControl {
    sequence: u64,
    cancellation: Option<Arc<CancellationToken>>,
}

impl WorkspaceScanControl {
    fn begin(&mut self) -> (u64, Arc<CancellationToken>) {
        self.invalidate();
        let cancellation = Arc::new(CancellationToken::new());
        self.cancellation = Some(Arc::clone(&cancellation));
        (self.sequence, cancellation)
    }

    fn invalidate(&mut self) {
        if let Some(cancellation) = self.cancellation.take() {
            cancellation.cancel();
        }
        self.sequence = self.sequence.saturating_add(1);
    }

    const fn is_active(&self) -> bool {
        self.cancellation.is_some()
    }

    fn accept(&mut self, sequence: u64) -> bool {
        if sequence != self.sequence {
            return false;
        }
        self.cancellation = None;
        true
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
/// `sequence` is the value the scan was scheduled with. A later scan makes an
/// earlier one obsolete, and the earlier result is discarded rather than
/// installed over the newer state.
struct WorkspaceScanned {
    sequence: u64,
    scan: crate::service::WorkspaceScan,
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
            workspace_scan: WorkspaceScanControl::default(),
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
                    let scan_was_active = state.workspace_scan.is_active();
                    if scan_was_active {
                        state.invalidate_workspace_scan();
                    }
                    for job in state.service.workspace_files_changed(params) {
                        state.schedule_analysis(job);
                    }
                    if scan_was_active {
                        state.schedule_workspace_scan();
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
                state.cpu_request(params.text_document.uri, move |service, uri| {
                    service.document_symbols(uri)
                })
            })
            .request::<request::CodeActionRequest, _>(|state, params| {
                let range = params.range;
                let context = params.context;
                state.cpu_request(params.text_document.uri, move |service, uri| {
                    service.code_actions(uri, range, &context)
                })
            })
            .request::<request::Formatting, _>(|state, params| {
                state.cpu_request(params.text_document.uri, move |service, uri| {
                    service.formatting(uri)
                })
            })
            .request::<request::HoverRequest, _>(|state, params| {
                let request = params.text_document_position_params;
                let position = request.position;
                state.cpu_request(request.text_document.uri, move |service, uri| {
                    service.hover(uri, position)
                })
            })
            .request::<request::Completion, _>(|state, params| {
                let request = params.text_document_position;
                let position = request.position;
                state.cpu_request(request.text_document.uri, move |service, uri| {
                    service.completion(uri, position)
                })
            })
            .request::<request::GotoDefinition, _>(|state, params| {
                let request = params.text_document_position_params;
                let position = request.position;
                state.cpu_request(request.text_document.uri, move |service, uri| {
                    service.definition(uri, position)
                })
            })
            .request::<request::References, _>(|state, params| {
                let request = params.text_document_position;
                let position = request.position;
                let include_declaration = params.context.include_declaration;
                state.cpu_request(request.text_document.uri, move |service, uri| {
                    service.references(uri, position, include_declaration)
                })
            })
            .request::<request::DocumentLinkRequest, _>(|state, params| {
                state.cpu_request(params.text_document.uri, move |service, uri| {
                    service.document_links(uri)
                })
            })
            .request::<request::SemanticTokensFullRequest, _>(|state, params| {
                state.cpu_request(params.text_document.uri, move |service, uri| {
                    service.semantic_tokens(uri)
                })
            })
            .request::<request::PrepareRenameRequest, _>(|state, params| {
                let position = params.position;
                state.cpu_request(params.text_document.uri, move |service, uri| {
                    service.prepare_rename(uri, position)
                })
            })
            .request::<request::Rename, _>(|state, params| {
                let request = params.text_document_position;
                let position = request.position;
                let new_name = params.new_name;
                state.cpu_request(request.text_document.uri, move |service, uri| {
                    service.rename(uri, position, &new_name)
                })
            })
            .event::<AnalysisCompleted>(|state, completed| state.analysis_completed(completed))
            .event::<WorkspaceScanned>(|state, scanned| {
                if !state.workspace_scan.accept(scanned.sequence) {
                    return ControlFlow::Continue(());
                }
                for job in state.service.apply_workspace_scan(scanned.scan) {
                    state.schedule_analysis(job);
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
        F: FnOnce(&LanguageService, &Url) -> Result<T, String> + Send + 'static,
    {
        let cancellation = self.service.document_cancellation(&uri);
        let service = self.service.clone();
        let limit = self.cpu_limit.clone();
        async move { run_cpu_request(limit, cancellation, move |_| operation(&service, &uri)).await }
    }

    /// Reads the workspace roots on a worker and installs the result later.
    ///
    /// The walk takes time proportional to the workspace, so running it here
    /// would stop the event loop from answering anything until it finished.
    /// Only the newest scan is installed; an older one that finishes later is
    /// discarded rather than written over newer state.
    fn schedule_workspace_scan(&mut self) {
        let (sequence, cancellation) = self.workspace_scan.begin();
        let service = self.service.clone();
        let client = self.client.clone();
        tokio::spawn(async move {
            let worker_cancellation = Arc::clone(&cancellation);
            let Ok(scan) = tokio::task::spawn_blocking(move || {
                service.plan_workspace_scan(worker_cancellation.as_ref())
            })
            .await
            else {
                return;
            };
            if cancellation.is_cancelled() {
                return;
            }
            let _ = client.emit(WorkspaceScanned { sequence, scan });
        });
    }

    fn invalidate_workspace_scan(&mut self) {
        self.workspace_scan.invalidate();
    }

    fn schedule_analysis(&mut self, job: AnalysisJob) {
        self.cancel_analysis(&job.uri);
        let limit = self.cpu_limit.clone();
        let client = self.client.clone();
        let debounce_ms = self.service.debounce_ms();
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
        let mut resolution_problem = None;
        if let Some(target) = &completed.missing_resource {
            match self.service.resolve_missing_include(&completed.job, target) {
                Ok(Some(retry)) => {
                    self.schedule_analysis(retry);
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
    F: FnOnce(Arc<CancellationToken>) -> Result<T, String> + Send + 'static,
{
    let cancellation = Arc::new(CancellationToken::new());
    let cancel_on_drop = CancelWorkerOnDrop(cancellation.clone());
    let permit = limit
        .acquire_owned()
        .await
        .map_err(|error| internal_error(error.to_string()))?;
    if cancellation.is_cancelled() {
        return Err(ResponseError::new(
            ErrorCode::REQUEST_CANCELLED,
            "request was cancelled",
        ));
    }
    if document_cancellation
        .as_ref()
        .is_some_and(|token| token.is_cancelled())
    {
        return Err(content_modified());
    }
    let result = tokio::task::spawn_blocking(move || {
        let _permit = permit;
        if cancellation.is_cancelled()
            || document_cancellation
                .as_ref()
                .is_some_and(|token| token.is_cancelled())
        {
            return Err("request was cancelled".to_owned());
        }
        let result = operation(cancellation.clone());
        if cancellation.is_cancelled() {
            return Err("request was cancelled".to_owned());
        }
        Ok((
            result,
            document_cancellation
                .as_ref()
                .is_some_and(|token| token.is_cancelled()),
        ))
    })
    .await
    .map_err(|error| internal_error(format!("request worker failed: {error}")))?;
    drop(cancel_on_drop);
    let (result, document_changed) = result.map_err(internal_error)?;
    if document_changed {
        return Err(content_modified());
    }
    result.map_err(internal_error)
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
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    use super::*;

    #[test]
    fn newer_workspace_scan_cancels_and_rejects_the_previous_result() {
        let mut control = WorkspaceScanControl::default();
        let (old_sequence, old_cancellation) = control.begin();
        let (new_sequence, new_cancellation) = control.begin();

        assert!(old_cancellation.is_cancelled());
        assert!(!new_cancellation.is_cancelled());
        assert!(!control.accept(old_sequence));
        assert!(control.accept(new_sequence));
        assert!(!control.is_active());
    }

    #[test]
    fn invalidating_a_workspace_scan_cancels_it_without_accepting_a_result() {
        let mut control = WorkspaceScanControl::default();
        let (sequence, cancellation) = control.begin();

        control.invalidate();

        assert!(cancellation.is_cancelled());
        assert!(!control.accept(sequence));
        assert!(!control.is_active());
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
                Err::<(), _>("request was cancelled".to_owned())
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
}
