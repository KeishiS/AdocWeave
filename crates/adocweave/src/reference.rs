//! Host boundary for generic cross-document and scheme reference resolution.
//!
//! Parsing never calls a resolver. Hosts translate parsed `Reference` values into
//! queries, await this interface, and pass validated results to consumers.

use std::future::Future;
use std::pin::Pin;

use crate::core::SourceId;
use crate::source::TextRange;

pub type ResolverFuture<'a, T> =
    Pin<Box<dyn Future<Output = Result<T, ResolverFailure>> + Send + 'a>>;

#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum ReferenceKey {
    Local {
        anchor: String,
    },
    Document {
        document: String,
        anchor: Option<String>,
    },
    Scheme {
        scheme: String,
        locator: String,
        anchor: Option<String>,
    },
}

impl ReferenceKey {
    pub fn from_destination(destination: &crate::inline::ReferenceDestination) -> Option<Self> {
        use crate::inline::ReferenceDestination;
        match destination {
            ReferenceDestination::Local { anchor, .. } => Some(Self::Local {
                anchor: anchor.clone(),
            }),
            ReferenceDestination::Document {
                document, anchor, ..
            } => Some(Self::Document {
                document: document.clone(),
                anchor: anchor.clone(),
            }),
            ReferenceDestination::Scheme {
                scheme,
                locator,
                anchor,
                ..
            } => Some(Self::Scheme {
                scheme: scheme.clone(),
                locator: locator.clone(),
                anchor: anchor.clone(),
            }),
            ReferenceDestination::Invalid => None,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReferenceQuery {
    pub source_id: Option<SourceId>,
    pub source_range: TextRange,
    pub target: ReferenceKey,
}

#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ResolutionCacheKey {
    pub source_id: Option<SourceId>,
    pub target: ReferenceKey,
    pub profile_version: u16,
    pub document_version: i64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ResolutionFailureKind {
    MissingTarget,
    MissingAnchor,
    AmbiguousTarget,
    OutsideRoot,
    ResolverFailure,
}

impl ResolutionFailureKind {
    pub const fn diagnostic_code(self) -> &'static str {
        match self {
            Self::MissingTarget => "missing-reference-target",
            Self::MissingAnchor => "missing-reference-anchor",
            Self::AmbiguousTarget => "ambiguous-reference-target",
            Self::OutsideRoot => "reference-outside-root",
            Self::ResolverFailure => "reference-resolver-failure",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResolverFailure {
    pub kind: ResolutionFailureKind,
    pub message: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResolvedReference {
    pub source_range: TextRange,
    pub outcome: ResolutionOutcome,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ResolutionOutcome {
    Resolved {
        href: String,
        display_text: Option<String>,
        notices: Vec<ResolutionNotice>,
    },
    Failed(ResolverFailure),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ResolutionNoticeKind {
    Fallback,
}

impl ResolutionNoticeKind {
    pub const fn diagnostic_code(self) -> &'static str {
        match self {
            Self::Fallback => "reference-resolution-fallback",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResolutionNotice {
    pub kind: ResolutionNoticeKind,
}

impl ResolvedReference {
    pub fn resolved(source_range: TextRange, href: impl Into<String>) -> Self {
        Self {
            source_range,
            outcome: ResolutionOutcome::Resolved {
                href: href.into(),
                display_text: None,
                notices: Vec::new(),
            },
        }
    }

    pub fn with_display_text(mut self, display_text: impl Into<String>) -> Self {
        if let ResolutionOutcome::Resolved {
            display_text: current,
            ..
        } = &mut self.outcome
        {
            *current = Some(display_text.into());
        }
        self
    }

    pub fn with_notices(mut self, notices: Vec<ResolutionNotice>) -> Self {
        if let ResolutionOutcome::Resolved {
            notices: current, ..
        } = &mut self.outcome
        {
            *current = notices;
        }
        self
    }

    pub fn failed(source_range: TextRange, failure: ResolverFailure) -> Self {
        Self {
            source_range,
            outcome: ResolutionOutcome::Failed(failure),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DocumentCandidate {
    pub source_id: SourceId,
    pub label: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReverseReference {
    pub source_id: SourceId,
    pub source_range: TextRange,
}

/// Asynchronous I/O owned by a CLI, editor, server, or WASM host.
pub trait ReferenceResolver: Send + Sync {
    fn document_candidates<'a>(
        &'a self,
        query: &'a str,
    ) -> ResolverFuture<'a, Vec<DocumentCandidate>>;

    fn resolve_document<'a>(
        &'a self,
        source: Option<&'a SourceId>,
        document: &'a str,
    ) -> ResolverFuture<'a, SourceId>;

    fn resolve_scheme<'a>(
        &'a self,
        scheme: &'a str,
        locator: &'a str,
    ) -> ResolverFuture<'a, SourceId>;

    fn resolve_anchor<'a>(
        &'a self,
        document: &'a SourceId,
        anchor: Option<&'a str>,
    ) -> ResolverFuture<'a, String>;

    fn reverse_references<'a>(
        &'a self,
        target: &'a ReferenceKey,
    ) -> ResolverFuture<'a, Vec<ReverseReference>>;
}

pub fn query_from_reference(
    source_id: Option<SourceId>,
    reference: &crate::inline::Reference,
) -> Option<ReferenceQuery> {
    let target = ReferenceKey::from_destination(&reference.destination)?;
    Some(ReferenceQuery {
        source_id,
        source_range: reference.range,
        target,
    })
}

#[cfg(test)]
mod tests {
    use super::ResolutionFailureKind;

    #[test]
    fn resolver_contract_exposes_stable_failure_codes() {
        assert_eq!(
            ResolutionFailureKind::MissingAnchor.diagnostic_code(),
            "missing-reference-anchor"
        );
        assert_eq!(
            ResolutionFailureKind::ResolverFailure.diagnostic_code(),
            "reference-resolver-failure"
        );
    }
}
