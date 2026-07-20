//! Host boundary for cross-document and note reference resolution.
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
    Document {
        document: String,
        anchor: Option<String>,
    },
    Note {
        uuid: String,
        anchor: Option<String>,
    },
    Scheme {
        scheme: String,
        locator: String,
        anchor: Option<String>,
    },
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
    Resolved { href: String },
    Failed(ResolverFailure),
}

impl ResolvedReference {
    pub fn resolved(source_range: TextRange, href: impl Into<String>) -> Self {
        Self {
            source_range,
            outcome: ResolutionOutcome::Resolved { href: href.into() },
        }
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

    fn resolve_note<'a>(&'a self, uuid: &'a str) -> ResolverFuture<'a, SourceId>;

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

pub fn is_canonical_uuid(value: &str) -> bool {
    value.len() == 36
        && value.bytes().enumerate().all(|(index, byte)| {
            if matches!(index, 8 | 13 | 18 | 23) {
                byte == b'-'
            } else {
                byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase()
            }
        })
}

pub fn query_from_reference(
    source_id: Option<SourceId>,
    reference: &crate::inline::Reference,
) -> Option<ReferenceQuery> {
    use crate::inline::ReferenceDestination;
    let target = match &reference.destination {
        ReferenceDestination::Document {
            document, anchor, ..
        } => ReferenceKey::Document {
            document: document.clone(),
            anchor: anchor.clone(),
        },
        ReferenceDestination::Scheme {
            scheme,
            locator,
            anchor,
            ..
        } if scheme == "note" => ReferenceKey::Note {
            uuid: locator.clone(),
            anchor: anchor.clone(),
        },
        ReferenceDestination::Scheme {
            scheme,
            locator,
            anchor,
            ..
        } => ReferenceKey::Scheme {
            scheme: scheme.clone(),
            locator: locator.clone(),
            anchor: anchor.clone(),
        },
        ReferenceDestination::Local { .. } | ReferenceDestination::Invalid => return None,
    };
    Some(ReferenceQuery {
        source_id,
        source_range: reference.range,
        target,
    })
}

#[cfg(test)]
mod tests {
    use super::{ResolutionFailureKind, is_canonical_uuid};

    #[test]
    fn note_reference_accepts_only_canonical_lowercase_uuid() {
        assert!(is_canonical_uuid("123e4567-e89b-12d3-a456-426614174000"));
        assert!(!is_canonical_uuid("123"));
        assert!(!is_canonical_uuid("123E4567-E89B-12D3-A456-426614174000"));
    }

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
