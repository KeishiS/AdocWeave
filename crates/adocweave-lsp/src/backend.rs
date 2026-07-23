//! Typed `async-lsp` adapter with generation-checked background analysis.

use std::collections::BTreeMap;
use std::num::NonZeroUsize;
use std::ops::ControlFlow;
use std::sync::Arc;

use adocweave::Engine;
use adocweave::preprocess::{PreprocessedAnalysis, ProjectionLimits, preprocess};
use adocweave::{CancellationCheck, CancellationToken};
use async_lsp::concurrency::ConcurrencyLayer;
use async_lsp::lsp_types::{PublishDiagnosticsParams, Url, notification, request};
use async_lsp::panic::CatchUnwindLayer;
use async_lsp::router::Router;
use async_lsp::server::LifecycleLayer;
use async_lsp::tracing::TracingLayer;
use async_lsp::{ClientSocket, ErrorCode, ResponseError};
use serde_json::Value;
use tokio::sync::Semaphore;
use tower::ServiceBuilder;

use crate::service::LanguageService;
use crate::state::{Adoption, AnalysisJob, WorkspaceAnalysis, WorkspaceProblem};
use crate::{HostReferenceIndex, NoHostReferenceIndex};

const MAX_CONCURRENT_REQUESTS: usize = 16;
const MAX_CONCURRENT_ANALYSES: usize = 2;

pub(crate) struct Backend {
    client: ClientSocket,
    service: LanguageService,
    cpu_limit: Arc<Semaphore>,
    analysis_tasks: BTreeMap<String, AnalysisTask>,
}

struct AnalysisTask {
    generation: u64,
    handle: tokio::task::JoinHandle<()>,
}

struct AnalysisCompleted {
    job: AnalysisJob,
    result: Result<adocweave::AnalysisResult, String>,
    workspace_result: Option<Result<WorkspaceAnalysis, WorkspaceProblem>>,
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
        let mut router = Router::new(Self {
            client,
            service: LanguageService::with_host_index(host_index),
            cpu_limit: Arc::new(Semaphore::new(MAX_CONCURRENT_ANALYSES)),
            analysis_tasks: BTreeMap::new(),
        });

        router
            .request::<request::Initialize, _>(|state, params| {
                let response = state.service.initialize(&params);
                async move { Ok(response) }
            })
            .notification::<notification::Initialized>(|_, _| ControlFlow::Continue(()))
            .request::<request::Shutdown, _>(|state, _| {
                state.cancel_all_analysis();
                async move { Ok(()) }
            })
            .notification::<notification::Exit>(|state, _| {
                state.cancel_all_analysis();
                ControlFlow::Break(Ok(()))
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
                let _ = state.service.update_configuration(params.settings);
                ControlFlow::Continue(())
            })
            .notification::<notification::DidChangeWatchedFiles>(|state, params| {
                for job in state.service.workspace_files_changed(params) {
                    state.schedule_analysis(job);
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
                let cancellation = state
                    .service
                    .document_cancellation(&params.text_document.uri);
                let service = state.service.clone();
                let limit = state.cpu_limit.clone();
                async move {
                    run_cpu_request(limit, cancellation, move |_| {
                        service.document_symbols(&params.text_document.uri)
                    })
                    .await
                }
            })
            .request::<request::CodeActionRequest, _>(|state, params| {
                let cancellation = state
                    .service
                    .document_cancellation(&params.text_document.uri);
                let service = state.service.clone();
                let limit = state.cpu_limit.clone();
                async move {
                    run_cpu_request(limit, cancellation, move |_| {
                        service.code_actions(&params.text_document.uri)
                    })
                    .await
                }
            })
            .request::<request::Formatting, _>(|state, params| {
                let cancellation = state
                    .service
                    .document_cancellation(&params.text_document.uri);
                let service = state.service.clone();
                let limit = state.cpu_limit.clone();
                async move {
                    run_cpu_request(limit, cancellation, move |_| {
                        service.formatting(&params.text_document.uri)
                    })
                    .await
                }
            })
            .request::<request::HoverRequest, _>(|state, params| {
                let request = params.text_document_position_params;
                let cancellation = state
                    .service
                    .document_cancellation(&request.text_document.uri);
                let service = state.service.clone();
                let limit = state.cpu_limit.clone();
                async move {
                    run_cpu_request(limit, cancellation, move |_| {
                        service.hover(&request.text_document.uri, request.position)
                    })
                    .await
                }
            })
            .request::<request::Completion, _>(|state, params| {
                let request = params.text_document_position;
                let cancellation = state
                    .service
                    .document_cancellation(&request.text_document.uri);
                let service = state.service.clone();
                let limit = state.cpu_limit.clone();
                async move {
                    run_cpu_request(limit, cancellation, move |_| {
                        service.completion(&request.text_document.uri, request.position)
                    })
                    .await
                }
            })
            .request::<request::GotoDefinition, _>(|state, params| {
                let request = params.text_document_position_params;
                let cancellation = state
                    .service
                    .document_cancellation(&request.text_document.uri);
                let service = state.service.clone();
                let limit = state.cpu_limit.clone();
                async move {
                    run_cpu_request(limit, cancellation, move |_| {
                        service.definition(&request.text_document.uri, request.position)
                    })
                    .await
                }
            })
            .request::<request::References, _>(|state, params| {
                let request = params.text_document_position;
                let include_declaration = params.context.include_declaration;
                let cancellation = state
                    .service
                    .document_cancellation(&request.text_document.uri);
                let service = state.service.clone();
                let limit = state.cpu_limit.clone();
                async move {
                    run_cpu_request(limit, cancellation, move |_| {
                        service.references(
                            &request.text_document.uri,
                            request.position,
                            include_declaration,
                        )
                    })
                    .await
                }
            })
            .request::<request::DocumentLinkRequest, _>(|state, params| {
                let cancellation = state
                    .service
                    .document_cancellation(&params.text_document.uri);
                let service = state.service.clone();
                let limit = state.cpu_limit.clone();
                async move {
                    run_cpu_request(limit, cancellation, move |_| {
                        service.document_links(&params.text_document.uri)
                    })
                    .await
                }
            })
            .request::<request::SemanticTokensFullRequest, _>(|state, params| {
                let cancellation = state
                    .service
                    .document_cancellation(&params.text_document.uri);
                let service = state.service.clone();
                let limit = state.cpu_limit.clone();
                async move {
                    run_cpu_request(limit, cancellation, move |_| {
                        service.semantic_tokens(&params.text_document.uri)
                    })
                    .await
                }
            })
            .request::<request::Rename, _>(|state, params| {
                let request = params.text_document_position;
                let new_name = params.new_name;
                let cancellation = state
                    .service
                    .document_cancellation(&request.text_document.uri);
                let service = state.service.clone();
                let limit = state.cpu_limit.clone();
                async move {
                    run_cpu_request(limit, cancellation, move |_| {
                        service.rename(&request.text_document.uri, request.position, &new_name)
                    })
                    .await
                }
            })
            .event::<AnalysisCompleted>(|state, completed| state.analysis_completed(completed));

        ServiceBuilder::new()
            .layer(TracingLayer::default())
            .layer(LifecycleLayer::default())
            .layer(CatchUnwindLayer::default())
            .layer(ConcurrencyLayer::new(
                NonZeroUsize::new(MAX_CONCURRENT_REQUESTS).expect("non-zero request limit"),
            ))
            .service(router)
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
                let workspace_result = worker_job.workspace.as_ref().map(|input| {
                    let document = preprocess(&input.root.text, &input.snapshot, &input.options)
                        .map_err(|error| WorkspaceProblem {
                            source_id: error
                                .source_id
                                .as_ref()
                                .map(|source_id| source_id.as_str().to_owned()),
                            range: error.range,
                            code: error.kind.as_str().to_owned(),
                            message: error.to_string(),
                        })?;
                    if worker_job.cancellation.is_cancelled() {
                        return Err(workspace_problem(
                            &worker_job,
                            "cancelled",
                            "workspace analysis was cancelled",
                        ));
                    }
                    let analysis = Engine::new(worker_job.request.options.clone())
                        .analyze_cancellable(&document.source, worker_job.cancellation.as_ref())
                        .map_err(|error| {
                            workspace_problem(
                                &worker_job,
                                error.code().as_str(),
                                &error.to_string(),
                            )
                        })?;
                    let preprocessed = PreprocessedAnalysis { document, analysis };
                    let projection = preprocessed
                        .project_origins(ProjectionLimits::default())
                        .map_err(|error| {
                            workspace_problem(&worker_job, "projection-limit", &error.to_string())
                        })?;
                    Ok(WorkspaceAnalysis {
                        document: Arc::new(preprocessed.document),
                        analysis: Arc::new(preprocessed.analysis),
                        projection: Arc::new(projection),
                        resource_versions: input.resource_versions.clone(),
                    })
                });
                (result, workspace_result)
            })
            .await
            .unwrap_or_else(|error| (Err(format!("analysis worker failed: {error}")), None));
            let _ = client.emit(AnalysisCompleted {
                job,
                result: result.0,
                workspace_result: result.1,
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
        let Ok(analysis) = completed.result else {
            return ControlFlow::Continue(());
        };
        if self.service.adopt(&completed.job, analysis) != Adoption::Adopted {
            return ControlFlow::Continue(());
        }
        let mut publish_uris = std::collections::BTreeSet::from([completed.job.uri.clone()]);
        if let Some(workspace) = completed.workspace_result {
            match workspace {
                Ok(workspace) => {
                    publish_uris.extend(workspace.source_uris());
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

fn workspace_problem(job: &AnalysisJob, code: &str, message: &str) -> WorkspaceProblem {
    WorkspaceProblem {
        source_id: Some(job.uri.clone()),
        range: adocweave::text::TextRange::new(
            adocweave::text::TextSize::ZERO,
            adocweave::text::TextSize::ZERO,
        )
        .expect("zero range is ordered"),
        code: code.to_owned(),
        message: message.to_owned(),
    }
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
