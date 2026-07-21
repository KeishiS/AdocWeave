//! Host boundary for media resources referenced by standard macros.

use std::future::Future;
use std::pin::Pin;

use crate::core::SourceId;
use crate::inline::{MacroAttribute, MacroForm, StandardMacro, StandardMacroKind};
use crate::source::TextRange;

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum ResourceKind {
    Image,
    Icon,
    Audio,
    Video,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResourceReference {
    pub kind: ResourceKind,
    pub form: MacroForm,
    pub range: TextRange,
    pub target_range: TextRange,
    pub target: String,
    pub attributes: Vec<MacroAttribute>,
}

impl ResourceReference {
    pub fn from_macro(node: &StandardMacro) -> Option<Self> {
        let kind = match node.kind {
            StandardMacroKind::Image => ResourceKind::Image,
            StandardMacroKind::Icon => ResourceKind::Icon,
            StandardMacroKind::Audio => ResourceKind::Audio,
            StandardMacroKind::Video => ResourceKind::Video,
            _ => return None,
        };
        Some(Self {
            kind,
            form: node.form,
            range: node.range,
            target_range: node.target_range,
            target: node.target.clone(),
            attributes: node.attributes.clone(),
        })
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResourceQuery {
    pub source_id: Option<SourceId>,
    pub reference: ResourceReference,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResourceValue {
    pub href: String,
    pub media_type: Option<String>,
    pub byte_length: Option<u64>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ResourceFailureKind {
    Missing,
    OutsideRoot,
    SchemeDenied,
    PermissionDenied,
    ResolverFailure,
}

impl ResourceFailureKind {
    pub const fn diagnostic_code(self) -> &'static str {
        match self {
            Self::Missing => "missing-resource",
            Self::OutsideRoot => "resource-outside-root",
            Self::SchemeDenied => "resource-scheme-denied",
            Self::PermissionDenied => "resource-permission-denied",
            Self::ResolverFailure => "resource-resolver-failure",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResourceFailure {
    pub kind: ResourceFailureKind,
    pub message: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResolvedResource {
    pub source_range: TextRange,
    pub outcome: ResourceOutcome,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ResourceOutcome {
    Resolved(ResourceValue),
    Failed(ResourceFailure),
}

impl ResolvedResource {
    pub fn resolved(
        source_range: TextRange,
        href: impl Into<String>,
        media_type: Option<String>,
        byte_length: Option<u64>,
    ) -> Self {
        Self {
            source_range,
            outcome: ResourceOutcome::Resolved(ResourceValue {
                href: href.into(),
                media_type,
                byte_length,
            }),
        }
    }

    pub fn failed(source_range: TextRange, failure: ResourceFailure) -> Self {
        Self {
            source_range,
            outcome: ResourceOutcome::Failed(failure),
        }
    }
}

pub type ResourceFuture<'a> =
    Pin<Box<dyn Future<Output = Result<ResourceValue, ResourceFailure>> + Send + 'a>>;

/// Resource I/O is exclusively owned by the host and is never called while parsing.
pub trait ResourceResolver: Send + Sync {
    fn resolve<'a>(&'a self, query: &'a ResourceQuery) -> ResourceFuture<'a>;
}
