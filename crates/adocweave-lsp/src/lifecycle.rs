//! Project-owned LSP lifecycle gate.
//!
//! `async-lsp 0.2.4` forwards notifications before initialization and after
//! shutdown, and it does not distinguish a clean `exit` after `shutdown` from
//! an abnormal `exit`. LSP 3.18 requires both distinctions, so this adapter
//! owns the protocol state machine while leaving typed routing to `async-lsp`.

use std::future::{Ready, ready};
use std::ops::ControlFlow;
use std::task::{Context, Poll};

use async_lsp::lsp_types::notification::Notification as _;
use async_lsp::lsp_types::request::Request as _;
use async_lsp::lsp_types::{notification, request};
use async_lsp::{
    AnyEvent, AnyNotification, AnyRequest, Error, ErrorCode, LspService, ResponseError, Result,
};
use futures::future::Either;
use tower::Layer;
use tower::Service;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
enum State {
    #[default]
    Uninitialized,
    Initializing,
    Ready,
    ShuttingDown,
}

#[derive(Debug)]
pub(crate) struct ProtocolLifecycle<S> {
    inner: S,
    state: State,
}

impl<S> ProtocolLifecycle<S> {
    fn new(inner: S) -> Self {
        Self {
            inner,
            state: State::Uninitialized,
        }
    }
}

impl<S> Service<AnyRequest> for ProtocolLifecycle<S>
where
    S: LspService,
    S::Error: From<ResponseError>,
{
    type Response = S::Response;
    type Error = S::Error;
    type Future = Either<S::Future, Ready<Result<S::Response, S::Error>>>;

    fn poll_ready(&mut self, context: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(context)
    }

    fn call(&mut self, request: AnyRequest) -> Self::Future {
        match (self.state, request.method.as_str()) {
            (State::Uninitialized, request::Initialize::METHOD) => {
                self.state = State::Initializing;
                Either::Left(self.inner.call(request))
            }
            (State::Uninitialized | State::Initializing, _) => {
                Either::Right(ready(Err(ResponseError::new(
                    ErrorCode::SERVER_NOT_INITIALIZED,
                    "server is not initialized",
                )
                .into())))
            }
            (_, request::Initialize::METHOD) => Either::Right(ready(Err(ResponseError::new(
                ErrorCode::INVALID_REQUEST,
                "server is already initialized",
            )
            .into()))),
            (State::Ready, request::Shutdown::METHOD) => {
                self.state = State::ShuttingDown;
                Either::Left(self.inner.call(request))
            }
            (State::Ready, _) => Either::Left(self.inner.call(request)),
            (State::ShuttingDown, _) => Either::Right(ready(Err(ResponseError::new(
                ErrorCode::INVALID_REQUEST,
                "server is shutting down",
            )
            .into()))),
        }
    }
}

impl<S> LspService for ProtocolLifecycle<S>
where
    S: LspService,
    S::Error: From<ResponseError>,
{
    fn notify(&mut self, notification: AnyNotification) -> ControlFlow<Result<()>> {
        match notification.method.as_str() {
            notification::Exit::METHOD => {
                let clean = self.state == State::ShuttingDown;
                let _ = self.inner.notify(notification);
                if clean {
                    ControlFlow::Break(Ok(()))
                } else {
                    ControlFlow::Break(Err(Error::Protocol(
                        "exit received before shutdown".to_owned(),
                    )))
                }
            }
            notification::Initialized::METHOD if self.state == State::Initializing => {
                self.state = State::Ready;
                self.inner.notify(notification)?;
                ControlFlow::Continue(())
            }
            notification::Initialized::METHOD => ControlFlow::Continue(()),
            _ if self.state == State::Ready => self.inner.notify(notification),
            _ => ControlFlow::Continue(()),
        }
    }

    fn emit(&mut self, event: AnyEvent) -> ControlFlow<Result<()>> {
        self.inner.emit(event)
    }
}

pub(crate) struct ProtocolLifecycleLayer;

impl<S> Layer<S> for ProtocolLifecycleLayer {
    type Service = ProtocolLifecycle<S>;

    fn layer(&self, inner: S) -> Self::Service {
        ProtocolLifecycle::new(inner)
    }
}
