//! Typed cooperative cancellation for read-only language requests.

use std::fmt;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use adocweave::{CancellationCheck, CancellationToken};

const CHECKPOINT_INTERVAL: usize = 256;

#[derive(Debug)]
pub(crate) struct QueryCancellation {
    request: Arc<CancellationToken>,
    document: Option<Arc<CancellationToken>>,
    work: AtomicUsize,
}

impl QueryCancellation {
    pub(crate) fn new(
        request: Arc<CancellationToken>,
        document: Option<Arc<CancellationToken>>,
    ) -> Self {
        Self {
            request,
            document,
            work: AtomicUsize::new(0),
        }
    }

    /// Checks cancellation at a fixed interval. Call once for every item in a
    /// potentially long loop so the same input always has the same checkpoints.
    pub(crate) fn checkpoint(&self) -> QueryResult<()> {
        if self
            .work
            .fetch_add(1, Ordering::Relaxed)
            .is_multiple_of(CHECKPOINT_INTERVAL)
        {
            self.check_now()
        } else {
            Ok(())
        }
    }

    /// Checks immediately at boundaries around work owned by another component.
    pub(crate) fn check_now(&self) -> QueryResult<()> {
        if self.request.is_cancelled() {
            Err(QueryError::RequestCancelled)
        } else if self
            .document
            .as_ref()
            .is_some_and(|token| token.is_cancelled())
        {
            Err(QueryError::ContentModified)
        } else {
            Ok(())
        }
    }
}

impl CancellationCheck for QueryCancellation {
    fn is_cancelled(&self) -> bool {
        self.request.is_cancelled()
            || self
                .document
                .as_ref()
                .is_some_and(|token| token.is_cancelled())
    }
}

#[derive(Debug, Eq, PartialEq)]
pub(crate) enum QueryError {
    RequestCancelled,
    ContentModified,
    Internal(String),
}

impl fmt::Display for QueryError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::RequestCancelled => formatter.write_str("request was cancelled"),
            Self::ContentModified => {
                formatter.write_str("document changed while the request was running")
            }
            Self::Internal(message) => formatter.write_str(message),
        }
    }
}

impl From<String> for QueryError {
    fn from(message: String) -> Self {
        Self::Internal(message)
    }
}

pub(crate) type QueryResult<T> = Result<T, QueryError>;

#[cfg(test)]
pub(crate) fn test_cancellation() -> QueryCancellation {
    QueryCancellation::new(Arc::new(CancellationToken::new()), None)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_cancellation_has_priority_over_document_change() {
        let request = Arc::new(CancellationToken::new());
        let document = Arc::new(CancellationToken::new());
        request.cancel();
        document.cancel();

        let cancellation = QueryCancellation::new(request, Some(document));

        assert_eq!(cancellation.check_now(), Err(QueryError::RequestCancelled));
    }
}
