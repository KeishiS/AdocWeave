//! Typed LSP adapter, isolated from the deterministic parsing core.

mod backend;
mod service;
mod state;

pub use service::{LanguageService, PositionEncoding};
pub use state::{DocumentState, DocumentStore};

pub const SERVER_NAME: &str = "adocweave-lsp";
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

pub async fn run<R, W>(input: R, output: W) -> async_lsp::Result<()>
where
    R: futures::AsyncRead + Unpin,
    W: futures::AsyncWrite + Unpin,
{
    let (main_loop, _) = async_lsp::MainLoop::new_server(backend::Backend::router);
    main_loop.run_buffered(input, output).await
}

pub async fn run_stdio() -> async_lsp::Result<()> {
    #[cfg(unix)]
    let (stdin, stdout) = (
        async_lsp::stdio::PipeStdin::lock_tokio().map_err(async_lsp::Error::Io)?,
        async_lsp::stdio::PipeStdout::lock_tokio().map_err(async_lsp::Error::Io)?,
    );
    #[cfg(not(unix))]
    let (stdin, stdout) = {
        use tokio_util::compat::{TokioAsyncReadCompatExt, TokioAsyncWriteCompatExt};
        (
            tokio::io::stdin().compat(),
            tokio::io::stdout().compat_write(),
        )
    };

    run(stdin, stdout).await
}

#[cfg(test)]
mod tests;
