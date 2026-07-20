//! Typed `async-lsp` adapter with generation-checked background analysis.

use std::num::NonZeroUsize;
use std::ops::ControlFlow;
use std::sync::Arc;

use adocweave::{CancellationCheck, Engine, ParseOptions, SourceId};
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

use crate::LanguageService;
use crate::state::{Adoption, AnalysisJob};

const MAX_CONCURRENT_REQUESTS: usize = 16;
const MAX_CONCURRENT_ANALYSES: usize = 2;

pub(crate) struct Backend {
    client: ClientSocket,
    service: LanguageService,
    analysis_limit: Arc<Semaphore>,
}

struct AnalysisCompleted {
    job: AnalysisJob,
    result: Result<adocweave::Analysis, String>,
}

impl Backend {
    pub(crate) fn router(
        client: ClientSocket,
    ) -> impl async_lsp::LspService<Response = Value, Error = ResponseError> {
        let mut router = Router::new(Self {
            client,
            service: LanguageService::default(),
            analysis_limit: Arc::new(Semaphore::new(MAX_CONCURRENT_ANALYSES)),
        });

        router
            .request::<request::Initialize, _>(|state, params| {
                let response = state.service.initialize(&params);
                async move { Ok(response) }
            })
            .notification::<notification::Initialized>(|_, _| ControlFlow::Continue(()))
            .request::<request::Shutdown, _>(|_, _| async move { Ok(()) })
            .notification::<notification::Exit>(|_, _| ControlFlow::Break(Ok(())))
            .notification::<notification::DidOpenTextDocument>(|state, params| {
                let job = state.service.begin_open(params);
                state.schedule_analysis(job);
                ControlFlow::Continue(())
            })
            .notification::<notification::DidChangeTextDocument>(|state, params| {
                match state.service.begin_change_full(params) {
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
            .notification::<notification::DidCloseTextDocument>(|state, params| {
                let uri = params.text_document.uri;
                state.service.close(&uri);
                state.publish_current_diagnostics(uri)
            })
            .request::<request::DocumentSymbolRequest, _>(|state, params| {
                let response = state
                    .service
                    .document_symbols(&params.text_document.uri)
                    .map_err(internal_error);
                async move { response }
            })
            .request::<request::CodeActionRequest, _>(|state, params| {
                let response = state
                    .service
                    .code_actions(&params.text_document.uri)
                    .map_err(internal_error);
                async move { response }
            })
            .request::<request::Formatting, _>(|state, params| {
                let response = state
                    .service
                    .formatting(&params.text_document.uri)
                    .map_err(internal_error);
                async move { response }
            })
            .request::<request::HoverRequest, _>(|state, params| {
                let request = params.text_document_position_params;
                let response = state
                    .service
                    .hover(&request.text_document.uri, request.position)
                    .map_err(internal_error);
                async move { response }
            })
            .request::<request::Completion, _>(|state, params| {
                let request = params.text_document_position;
                let response = state
                    .service
                    .completion(&request.text_document.uri, request.position)
                    .map_err(internal_error);
                async move { response }
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
        let limit = self.analysis_limit.clone();
        let client = self.client.clone();
        tokio::spawn(async move {
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

fn internal_error(error: impl ToString) -> ResponseError {
    ResponseError::new(ErrorCode::INTERNAL_ERROR, error.to_string())
}
