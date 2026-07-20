//! Typed `async-lsp` adapter with generation-checked background analysis.

use std::num::NonZeroUsize;
use std::ops::ControlFlow;
use std::sync::Arc;

use adocweave::{CancellationCheck, CancellationToken, Engine, ParseOptions, SourceId};
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

use crate::state::{Adoption, AnalysisJob};
use crate::{HostReferenceIndex, LanguageService, NoHostReferenceIndex};

const MAX_CONCURRENT_REQUESTS: usize = 16;
const MAX_CONCURRENT_ANALYSES: usize = 2;

pub(crate) struct Backend {
    client: ClientSocket,
    service: LanguageService,
    cpu_limit: Arc<Semaphore>,
}

struct AnalysisCompleted {
    job: AnalysisJob,
    result: Result<adocweave::Analysis, String>,
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
        });

        router
            .request::<request::Initialize, _>(|state, params| {
                let response = state.service.initialize(&params);
                async move { Ok(response) }
            })
            .notification::<notification::Initialized>(|_, _| ControlFlow::Continue(()))
            .request::<request::Shutdown, _>(|state, _| {
                state.service.cancel_all();
                async move { Ok(()) }
            })
            .notification::<notification::Exit>(|state, _| {
                state.service.cancel_all();
                ControlFlow::Break(Ok(()))
            })
            .notification::<notification::DidOpenTextDocument>(|state, params| {
                let job = state.service.begin_open(params);
                state.schedule_analysis(job);
                ControlFlow::Continue(())
            })
            .notification::<notification::DidChangeTextDocument>(|state, params| {
                match state.service.begin_change(params) {
                    Ok(Some(job)) => {
                        state.schedule_analysis(job);
                        ControlFlow::Continue(())
                    }
                    Ok(None) => ControlFlow::Continue(()),
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
            .notification::<notification::DidCloseTextDocument>(|state, params| {
                let uri = params.text_document.uri;
                state.service.close(&uri);
                state.publish_current_diagnostics(uri)
            })
            .request::<request::DocumentSymbolRequest, _>(|state, params| {
                let service = state.service.clone();
                let limit = state.cpu_limit.clone();
                async move {
                    run_cpu_request(limit, move |_| {
                        service.document_symbols(&params.text_document.uri)
                    })
                    .await
                }
            })
            .request::<request::CodeActionRequest, _>(|state, params| {
                let service = state.service.clone();
                let limit = state.cpu_limit.clone();
                async move {
                    run_cpu_request(limit, move |_| {
                        service.code_actions(&params.text_document.uri)
                    })
                    .await
                }
            })
            .request::<request::Formatting, _>(|state, params| {
                let service = state.service.clone();
                let limit = state.cpu_limit.clone();
                async move {
                    run_cpu_request(limit, move |_| {
                        service.formatting(&params.text_document.uri)
                    })
                    .await
                }
            })
            .request::<request::HoverRequest, _>(|state, params| {
                let request = params.text_document_position_params;
                let service = state.service.clone();
                let limit = state.cpu_limit.clone();
                async move {
                    run_cpu_request(limit, move |_| {
                        service.hover(&request.text_document.uri, request.position)
                    })
                    .await
                }
            })
            .request::<request::Completion, _>(|state, params| {
                let request = params.text_document_position;
                let service = state.service.clone();
                let limit = state.cpu_limit.clone();
                async move {
                    run_cpu_request(limit, move |_| {
                        service.completion(&request.text_document.uri, request.position)
                    })
                    .await
                }
            })
            .request::<request::GotoDefinition, _>(|state, params| {
                let request = params.text_document_position_params;
                let service = state.service.clone();
                let limit = state.cpu_limit.clone();
                async move {
                    run_cpu_request(limit, move |_| {
                        service.definition(&request.text_document.uri, request.position)
                    })
                    .await
                }
            })
            .request::<request::References, _>(|state, params| {
                let request = params.text_document_position;
                let include_declaration = params.context.include_declaration;
                let service = state.service.clone();
                let limit = state.cpu_limit.clone();
                async move {
                    run_cpu_request(limit, move |_| {
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
                let service = state.service.clone();
                let limit = state.cpu_limit.clone();
                async move {
                    run_cpu_request(limit, move |_| {
                        service.document_links(&params.text_document.uri)
                    })
                    .await
                }
            })
            .request::<request::SemanticTokensFullRequest, _>(|state, params| {
                let service = state.service.clone();
                let limit = state.cpu_limit.clone();
                async move {
                    run_cpu_request(limit, move |_| {
                        service.semantic_tokens(&params.text_document.uri)
                    })
                    .await
                }
            })
            .request::<request::Rename, _>(|state, params| {
                let request = params.text_document_position;
                let new_name = params.new_name;
                let service = state.service.clone();
                let limit = state.cpu_limit.clone();
                async move {
                    run_cpu_request(limit, move |_| {
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

    fn schedule_analysis(&self, job: AnalysisJob) {
        let limit = self.cpu_limit.clone();
        let client = self.client.clone();
        let debounce_ms = self.service.debounce_ms();
        tokio::spawn(async move {
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
                Engine::new(ParseOptions {
                    source_id: Some(SourceId::new(worker_job.uri.clone())),
                    ..ParseOptions::default()
                })
                .analyze_cancellable(&worker_job.source, worker_job.cancellation.as_ref())
                .map_err(|error| error.to_string())
            })
            .await
            .unwrap_or_else(|error| Err(format!("analysis worker failed: {error}")));
            let _ = client.emit(AnalysisCompleted { job, result });
        });
    }

    fn analysis_completed(
        &mut self,
        completed: AnalysisCompleted,
    ) -> ControlFlow<async_lsp::Result<()>> {
        let Ok(analysis) = completed.result else {
            return ControlFlow::Continue(());
        };
        if self.service.adopt(&completed.job, analysis) != Adoption::Adopted {
            return ControlFlow::Continue(());
        }
        match completed.job.uri.parse() {
            Ok(uri) => self.publish_current_diagnostics(uri),
            Err(error) => ControlFlow::Break(Err(async_lsp::Error::Routing(error.to_string()))),
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

struct CancelWorkerOnDrop(Arc<CancellationToken>);

impl Drop for CancelWorkerOnDrop {
    fn drop(&mut self) {
        self.0.cancel();
    }
}

async fn run_cpu_request<T, F>(limit: Arc<Semaphore>, operation: F) -> Result<T, ResponseError>
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
    let result = tokio::task::spawn_blocking(move || {
        let _permit = permit;
        if cancellation.is_cancelled() {
            return Err("request was cancelled".to_owned());
        }
        let result = operation(cancellation.clone());
        if cancellation.is_cancelled() {
            return Err("request was cancelled".to_owned());
        }
        result
    })
    .await
    .map_err(|error| internal_error(format!("request worker failed: {error}")))?
    .map_err(internal_error);
    drop(cancel_on_drop);
    result
}

fn internal_error(error: impl ToString) -> ResponseError {
    ResponseError::new(ErrorCode::INTERNAL_ERROR, error.to_string())
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
            run_cpu_request(limit.clone(), move |_| {
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
}
