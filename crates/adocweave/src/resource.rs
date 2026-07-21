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
pub struct ResolvedResource {
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

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResourceFailure {
    pub kind: ResourceFailureKind,
    pub message: String,
}

pub type ResourceFuture<'a> =
    Pin<Box<dyn Future<Output = Result<ResolvedResource, ResourceFailure>> + Send + 'a>>;

/// Resource I/O is exclusively owned by the host and is never called while parsing.
pub trait ResourceResolver: Send + Sync {
    fn resolve<'a>(&'a self, query: &'a ResourceQuery) -> ResourceFuture<'a>;
}
