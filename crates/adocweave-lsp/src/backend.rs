//! Typed `async-lsp` adapter around the existing language-service behavior.

use std::num::NonZeroUsize;
use std::ops::ControlFlow;

use async_lsp::concurrency::ConcurrencyLayer;
use async_lsp::lsp_types::{
    CodeActionOrCommand, CompletionResponse, DocumentSymbolResponse, Hover, InitializeResult,
    PublishDiagnosticsParams, TextEdit, notification, request,
};
use async_lsp::panic::CatchUnwindLayer;
use async_lsp::router::Router;
use async_lsp::server::LifecycleLayer;
use async_lsp::tracing::TracingLayer;
use async_lsp::{ClientSocket, ErrorCode, ResponseError};
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use tower::ServiceBuilder;

use crate::Server;

const MAX_CONCURRENT_REQUESTS: usize = 16;

pub(crate) struct Backend {
    client: ClientSocket,
    service: Server,
    next_request_id: i64,
}

impl Backend {
    pub(crate) fn router(
        client: ClientSocket,
    ) -> impl async_lsp::LspService<Response = Value, Error = ResponseError> {
        let mut router = Router::new(Self {
            client,
            service: Server::default(),
            next_request_id: 1,
        });

        router
            .request::<request::Initialize, _>(|state, params| {
                let response = state.request("initialize", params);
                async move { response }
            })
            .notification::<notification::Initialized>(|state, params| {
                state.notification("initialized", params)
            })
            .request::<request::Shutdown, _>(|state, params| {
                let response = state.request("shutdown", params);
                async move { response }
            })
            .notification::<notification::Exit>(|state, params| state.notification("exit", params))
            .notification::<notification::DidOpenTextDocument>(|state, params| {
                state.notification("textDocument/didOpen", params)
            })
            .notification::<notification::DidChangeTextDocument>(|state, params| {
                state.notification("textDocument/didChange", params)
            })
            .notification::<notification::DidSaveTextDocument>(|state, params| {
                state.notification("textDocument/didSave", params)
            })
            .notification::<notification::DidCloseTextDocument>(|state, params| {
                state.notification("textDocument/didClose", params)
            })
            .request::<request::DocumentSymbolRequest, _>(|state, params| {
                let response = state.request("textDocument/documentSymbol", params);
                async move { response }
            })
            .request::<request::CodeActionRequest, _>(|state, params| {
                let response = state.request("textDocument/codeAction", params);
                async move { response }
            })
            .request::<request::Formatting, _>(|state, params| {
                let response = state.request("textDocument/formatting", params);
                async move { response }
            })
            .request::<request::HoverRequest, _>(|state, params| {
                let response = state.request("textDocument/hover", params);
                async move { response }
            })
            .request::<request::Completion, _>(|state, params| {
                let response = state.request("textDocument/completion", params);
                async move { response }
            });

        ServiceBuilder::new()
            .layer(TracingLayer::default())
            .layer(LifecycleLayer::default())
            .layer(CatchUnwindLayer::default())
            .layer(ConcurrencyLayer::new(
                NonZeroUsize::new(MAX_CONCURRENT_REQUESTS).expect("non-zero request limit"),
            ))
            .service(router)
    }

    fn request<P, R>(&mut self, method: &str, params: P) -> Result<R, ResponseError>
    where
        P: Serialize,
        R: DeserializeOwned,
    {
        let id = self.next_request_id;
        self.next_request_id = self.next_request_id.saturating_add(1);
        let response = self
            .service
            .handle(&json!({
                "jsonrpc": "2.0",
                "id": id,
                "method": method,
                "params": params
            }))
            .map_err(internal_error)?
            .ok_or_else(|| internal_error("request produced no response"))?;
        if let Some(error) = response.get("error") {
            return Err(ResponseError::new_with_data(
                ErrorCode::INTERNAL_ERROR,
                error
                    .get("message")
                    .and_then(Value::as_str)
                    .unwrap_or("request failed"),
                error.clone(),
            ));
        }
        serde_json::from_value(response["result"].clone()).map_err(internal_error)
    }

    fn notification<P>(&mut self, method: &str, params: P) -> ControlFlow<async_lsp::Result<()>>
    where
        P: Serialize,
    {
        let result = self
            .service
            .handle(&json!({
                "jsonrpc": "2.0",
                "method": method,
                "params": params
            }))
            .map_err(async_lsp::Error::Routing)
            .and_then(|_| self.flush_notifications());

        if method == "exit" {
            ControlFlow::Break(result)
        } else {
            match result {
                Ok(()) => ControlFlow::Continue(()),
                Err(error) => ControlFlow::Break(Err(error)),
            }
        }
    }

    fn flush_notifications(&mut self) -> async_lsp::Result<()> {
        for message in self.service.drain_outgoing() {
            if message.get("method").and_then(Value::as_str)
                != Some("textDocument/publishDiagnostics")
            {
                continue;
            }
            let params: PublishDiagnosticsParams =
                serde_json::from_value(message["params"].clone())
                    .map_err(|error| async_lsp::Error::Routing(error.to_string()))?;
            self.client
                .notify::<notification::PublishDiagnostics>(params)?;
        }
        Ok(())
    }
}

fn internal_error(error: impl ToString) -> ResponseError {
    ResponseError::new(ErrorCode::INTERNAL_ERROR, error.to_string())
}

// These annotations make accidental changes to the typed request results fail here instead of
// surfacing only through an editor integration test.
const _: fn() = || {
    fn result_types(
        _: InitializeResult,
        _: Option<DocumentSymbolResponse>,
        _: Option<Vec<CodeActionOrCommand>>,
        _: Vec<TextEdit>,
        _: Option<Hover>,
        _: Option<CompletionResponse>,
    ) {
    }
    let _ = result_types;
};
