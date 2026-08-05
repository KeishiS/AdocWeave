//! Runtime-independent, bounded multi-document analysis.
//!
//! [`Workspace`] owns mutable disk and editor-overlay state. A
//! [`WorkspaceSnapshot`] is immutable and can safely move to a worker thread.
//! Callers accept completed analysis through [`Workspace::accept`] so results
//! from an older generation cannot replace current dependency information.
//! Filesystem discovery and reads belong to host adapters; this crate accepts
//! already validated resource identities and text without performing I/O.
#![warn(missing_docs)]

mod dependency_graph;

use std::collections::{BTreeMap, BTreeSet};
use std::error::Error;
use std::fmt;
use std::sync::{Arc, Mutex};

use adocweave::output::diagnostics::Severity;
use adocweave::preprocess::{
    AnalysisProjection, DirectiveKind, EffectivePreprocessStep, EffectiveProcessingOptions,
    EffectiveSuspendedPreprocess, HostResourceErrorKind, PreprocessError, PreprocessErrorKind,
    PreprocessInputs, PreprocessOptions, PreprocessedAnalysisError, ProjectionFailure,
    ProjectionLimits, ResourceDocument, ResourceLookup, ResourceLookupResult, ResourceRequest,
    ResourceResponse, ResourceSnapshot,
};
use adocweave::{AnalysisOptions, SourceId};
use dependency_graph::DependencyGraph;

#[cfg(test)]
thread_local! {
    static RESUMABLE_STAGE_RUNS: std::cell::Cell<[usize; 4]> = const {
        std::cell::Cell::new([0; 4])
    };
    static RESUMABLE_EVIDENCE_RECORDS: std::cell::Cell<usize> = const {
        std::cell::Cell::new(0)
    };
}

#[cfg(test)]
fn record_resumable_stage(index: usize) {
    RESUMABLE_STAGE_RUNS.with(|runs| {
        let mut current = runs.get();
        current[index] += 1;
        runs.set(current);
    });
}

/// Stable, host-defined identity for one workspace resource.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ResourceId(String);

impl ResourceId {
    /// Creates an identity after rejecting empty values and control characters.
    pub fn new(value: impl Into<String>) -> Result<Self, WorkspaceError> {
        let value = value.into();
        if value.is_empty() || value.chars().any(char::is_control) {
            return Err(WorkspaceError::new(
                WorkspaceErrorCode::InvalidResourceId,
                "resource IDs must be non-empty and contain no control characters",
            ));
        }
        Ok(Self(value))
    }

    /// Returns the identity as supplied by the host.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for ResourceId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// Monotonic revision assigned by the host within one resource layer.
#[derive(Clone, Copy, Debug, Default, Eq, Ord, PartialEq, PartialOrd)]
pub struct Revision(i64);

impl Revision {
    /// Creates a revision from a host-defined monotonic value.
    pub const fn new(value: i64) -> Self {
        Self(value)
    }

    /// Returns the underlying host revision.
    pub const fn get(self) -> i64 {
        self.0
    }
}

/// Monotonic generation of the effective workspace state.
#[derive(Clone, Copy, Debug, Default, Eq, Ord, PartialEq, PartialOrd)]
pub struct Generation(u64);

impl Generation {
    /// Creates a generation, for example when rebuilding adapter state.
    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    /// Returns the underlying generation.
    pub const fn get(self) -> u64 {
        self.0
    }

    const fn next(self) -> Self {
        Self(self.0.saturating_add(1))
    }
}

/// Storage layer supplying the effective resource text.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ResourceLayer {
    /// Text read from persistent host storage.
    Disk,
    /// Text supplied by an open editor or another transient host.
    Overlay,
}

/// Immutable effective resource stored in a workspace snapshot.
#[derive(Clone, Debug)]
pub struct Resource {
    id: ResourceId,
    revision: Revision,
    text: Arc<str>,
    layer: ResourceLayer,
}

impl Resource {
    /// Returns the stable resource identity.
    pub fn id(&self) -> &ResourceId {
        &self.id
    }

    /// Returns the revision of the effective layer.
    pub const fn revision(&self) -> Revision {
        self.revision
    }

    /// Returns shared immutable UTF-8 text.
    pub fn text(&self) -> &Arc<str> {
        &self.text
    }

    /// Returns the layer supplying the effective text.
    pub const fn layer(&self) -> ResourceLayer {
        self.layer
    }
}

/// Bounds applied to disk and overlay layers retained in workspace state.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RetainedResourceLimits {
    /// Maximum number of distinct resource identities.
    pub max_files: usize,
    /// Maximum combined bytes retained across disk and overlay layers.
    pub max_total_bytes: u64,
    /// Maximum bytes retained for one resource layer.
    pub max_resource_bytes: u64,
}

impl Default for RetainedResourceLimits {
    fn default() -> Self {
        Self {
            max_files: 10_000,
            max_total_bytes: 50 * 1024 * 1024,
            max_resource_bytes: 10 * 1024 * 1024,
        }
    }
}

/// Byte charges retained for the two independently owned resource layers.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct RetainedLayerCharge {
    /// Bytes retained for the filesystem-backed layer.
    disk_bytes: Option<u64>,
    /// Bytes retained for the open-document layer.
    overlay_bytes: Option<u64>,
}

impl RetainedLayerCharge {
    /// Constructs one layer charge.
    pub const fn new(disk_bytes: Option<u64>, overlay_bytes: Option<u64>) -> Self {
        Self {
            disk_bytes,
            overlay_bytes,
        }
    }

    /// Returns the disk-layer charge.
    pub const fn disk_bytes(self) -> Option<u64> {
        self.disk_bytes
    }

    /// Returns the overlay-layer charge.
    pub const fn overlay_bytes(self) -> Option<u64> {
        self.overlay_bytes
    }
}

/// Transactional accounting for disk and overlay layers in one project scope.
///
/// The returned replacement budget is committed by the caller only after the
/// corresponding workspace update succeeds.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct RetainedResourceBudget {
    resources: BTreeMap<ResourceId, RetainedLayerCharge>,
    resource_count: usize,
    total_bytes: u64,
}

impl RetainedResourceBudget {
    /// Returns the layer charges for one resource identity.
    pub fn charge(&self, id: &ResourceId) -> RetainedLayerCharge {
        self.resources.get(id).copied().unwrap_or_default()
    }

    /// Returns whether this scope retains no resource layers.
    pub fn is_empty(&self) -> bool {
        self.resource_count == 0
    }

    /// Replaces both layer charges without cloning the previously committed map.
    ///
    /// Limit validation completes before the entry and cached totals change.
    pub fn try_replace_layers(
        &mut self,
        id: ResourceId,
        charge: RetainedLayerCharge,
        limits: RetainedResourceLimits,
    ) -> Result<(), WorkspaceError> {
        let previous = self.resources.get(&id).copied().unwrap_or_default();
        let previous_present = previous.disk_bytes.is_some() || previous.overlay_bytes.is_some();
        let incoming_present = charge.disk_bytes.is_some() || charge.overlay_bytes.is_some();
        let incoming_bytes = charge
            .disk_bytes
            .into_iter()
            .chain(charge.overlay_bytes)
            .try_fold(0_u64, |total, bytes| {
                if bytes > limits.max_resource_bytes {
                    return None;
                }
                total.checked_add(bytes)
            })
            .ok_or_else(|| {
                WorkspaceError::new(
                    WorkspaceErrorCode::ResourceLimit,
                    "retained resource byte limit exceeded",
                )
            })?;
        let previous_bytes = previous
            .disk_bytes
            .into_iter()
            .chain(previous.overlay_bytes)
            .sum::<u64>();
        let resource_count = self
            .resource_count
            .checked_sub(usize::from(previous_present))
            .and_then(|count| count.checked_add(usize::from(incoming_present)))
            .ok_or_else(|| {
                WorkspaceError::new(
                    WorkspaceErrorCode::ResourceLimit,
                    "retained resource count limit exceeded",
                )
            })?;
        if resource_count > limits.max_files {
            return Err(WorkspaceError::new(
                WorkspaceErrorCode::ResourceLimit,
                "retained resource count limit exceeded",
            ));
        }
        let total_bytes = self
            .total_bytes
            .checked_sub(previous_bytes)
            .and_then(|total| total.checked_add(incoming_bytes))
            .filter(|total| *total <= limits.max_total_bytes)
            .ok_or_else(|| {
                WorkspaceError::new(
                    WorkspaceErrorCode::ResourceLimit,
                    "retained resource byte limit exceeded",
                )
            })?;
        if incoming_present {
            self.resources.insert(id, charge);
        } else {
            self.resources.remove(&id);
        }
        self.resource_count = resource_count;
        self.total_bytes = total_bytes;
        Ok(())
    }

    /// Returns a budget with both layers replaced atomically.
    pub fn with_layers(
        &self,
        id: ResourceId,
        charge: RetainedLayerCharge,
        limits: RetainedResourceLimits,
    ) -> Result<Self, WorkspaceError> {
        let mut replacement = self.clone();
        replacement.try_replace_layers(id, charge, limits)?;
        Ok(replacement)
    }

    /// Returns a budget with both layers released.
    pub fn without_resource(&self, id: &ResourceId) -> Self {
        let mut replacement = self.clone();
        let previous = replacement.resources.remove(id).unwrap_or_default();
        if previous.disk_bytes.is_some() || previous.overlay_bytes.is_some() {
            replacement.resource_count -= 1;
            replacement.total_bytes -= previous
                .disk_bytes
                .into_iter()
                .chain(previous.overlay_bytes)
                .sum::<u64>();
        }
        replacement
    }

    /// Returns a budget with one disk layer inserted, replaced, or removed.
    pub fn with_disk(
        &self,
        id: ResourceId,
        bytes: Option<u64>,
        limits: RetainedResourceLimits,
    ) -> Result<Self, WorkspaceError> {
        let mut replacement = self.clone();
        let mut charge = replacement.charge(&id);
        charge.disk_bytes = bytes;
        replacement.try_replace_layers(id, charge, limits)?;
        Ok(replacement)
    }

    /// Returns a budget with one overlay layer inserted, replaced, or removed.
    pub fn with_overlay(
        &self,
        id: ResourceId,
        bytes: Option<u64>,
        limits: RetainedResourceLimits,
    ) -> Result<Self, WorkspaceError> {
        let mut replacement = self.clone();
        let mut charge = replacement.charge(&id);
        charge.overlay_bytes = bytes;
        replacement.try_replace_layers(id, charge, limits)?;
        Ok(replacement)
    }
}

/// Bounds applied before resources or analysis roots enter workspace state.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct WorkspaceLimits {
    /// Resource count and byte limits for verified resources supplied by a host.
    pub resources: RetainedResourceLimits,
    /// Maximum number of registered analysis roots.
    pub max_roots: usize,
}

impl Default for WorkspaceLimits {
    fn default() -> Self {
        Self {
            resources: RetainedResourceLimits::default(),
            max_roots: 10_000,
        }
    }
}

/// Stable category for workspace state and analysis failures.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WorkspaceErrorCode {
    /// Analysis and preprocessing settings disagree before processing starts.
    InvalidOptions,
    /// Invalid host resource identity.
    InvalidResourceId,
    /// Required resource or registered root not present.
    MissingResource,
    /// Update or result older than current resource state.
    StaleRevision,
    /// Analysis result from an older workspace state.
    StaleGeneration,
    /// Configured resource or root limit exceeded.
    ResourceLimit,
    /// Cooperatively cancelled analysis.
    Cancelled,
    /// Include preprocessing failure.
    Preprocess,
    /// Core analysis failure.
    Analysis,
    /// Source-origin projection failure.
    Projection,
}

/// Workspace error with an optional source origin.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WorkspaceError {
    /// Stable high-level error category.
    pub code: WorkspaceErrorCode,
    /// Logical source identity when preprocessing supplied one.
    pub source_id: Option<ResourceId>,
    /// Source byte range when preprocessing supplied one.
    pub range: Option<adocweave::text::TextRange>,
    detail_code: Option<&'static str>,
    requested_resource: Option<ResourceId>,
    host_resource_kind: Option<WorkspaceHostResourceErrorKind>,
    message: String,
}

/// Stable category for a host resource failure during workspace analysis.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WorkspaceHostResourceErrorKind {
    /// The host could not load the requested resource.
    LoadFailed,
    /// A response belongs to another request or preprocessing run.
    ResponseMismatch,
}

impl WorkspaceError {
    fn new(code: WorkspaceErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            source_id: None,
            range: None,
            detail_code: None,
            requested_resource: None,
            host_resource_kind: None,
            message: message.into(),
        }
    }

    fn with_origin(
        mut self,
        source_id: Option<&SourceId>,
        range: adocweave::text::TextRange,
        detail_code: &'static str,
    ) -> Self {
        self.source_id = source_id.and_then(|value| ResourceId::new(value.as_str()).ok());
        self.range = Some(range);
        self.detail_code = Some(detail_code);
        self
    }

    /// Returns the most specific stable diagnostic code.
    pub const fn diagnostic_code(&self) -> &'static str {
        match self.detail_code {
            Some(code) => code,
            None => self.code.as_str(),
        }
    }

    /// Returns the missing logical resource requested by preprocessing.
    pub fn requested_resource(&self) -> Option<&ResourceId> {
        self.requested_resource.as_ref()
    }

    /// Returns the typed host resource cause when this error crossed that boundary.
    pub const fn host_resource_kind(&self) -> Option<WorkspaceHostResourceErrorKind> {
        self.host_resource_kind
    }

    fn with_requested_resource(mut self, target: Option<&str>) -> Self {
        self.requested_resource = target.and_then(|target| ResourceId::new(target).ok());
        self
    }

    fn with_host_resource(mut self, kind: HostResourceErrorKind, target: &str) -> Self {
        let (kind, detail_code) = match kind {
            HostResourceErrorKind::LoadFailed => (
                WorkspaceHostResourceErrorKind::LoadFailed,
                "host-resource-load-failed",
            ),
            HostResourceErrorKind::ResponseMismatch => (
                WorkspaceHostResourceErrorKind::ResponseMismatch,
                "host-resource-response-mismatch",
            ),
            _ => return self,
        };
        self.host_resource_kind = Some(kind);
        self.requested_resource = ResourceId::new(target).ok();
        self.detail_code = Some(detail_code);
        self
    }
}

impl fmt::Display for WorkspaceError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.message.is_empty() {
            write!(formatter, "workspace {}", self.code.as_str())
        } else {
            write!(
                formatter,
                "workspace {}: {}",
                self.code.as_str(),
                self.message
            )
        }
    }
}

impl Error for WorkspaceError {}

impl WorkspaceErrorCode {
    /// Returns the stable kebab-case code.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::InvalidOptions => "invalid-options",
            Self::InvalidResourceId => "invalid-resource-id",
            Self::MissingResource => "missing-resource",
            Self::StaleRevision => "stale-revision",
            Self::StaleGeneration => "stale-generation",
            Self::ResourceLimit => "resource-limit",
            Self::Cancelled => "cancelled",
            Self::Preprocess => "preprocess",
            Self::Analysis => "analysis",
            Self::Projection => "projection",
        }
    }
}

/// Runtime-independent cancellation accepted by workspace analysis.
pub trait Cancellation: adocweave::CancellationCheck {}

impl<T: adocweave::CancellationCheck + ?Sized> Cancellation for T {}

/// Cancellation implementation for synchronous calls that always complete.
#[derive(Clone, Copy, Debug, Default)]
pub struct NeverCancelled;

impl adocweave::CancellationCheck for NeverCancelled {
    fn is_cancelled(&self) -> bool {
        false
    }
}

/// Mutable bounded workspace state.
///
/// Mutations are atomic with respect to validation: a rejected update leaves
/// the prior effective snapshot unchanged.
#[derive(Clone, Debug)]
pub struct Workspace {
    generation: Generation,
    limits: WorkspaceLimits,
    roots: Arc<BTreeSet<ResourceId>>,
    disk: BTreeMap<ResourceId, Resource>,
    overlays: BTreeMap<ResourceId, Resource>,
    retained_resource_count: usize,
    retained_total_bytes: u64,
    effective: Arc<BTreeMap<ResourceId, Resource>>,
    dependencies: DependencyGraph<ResourceId>,
}

impl Default for Workspace {
    fn default() -> Self {
        Self::new(WorkspaceLimits::default())
    }
}

impl Workspace {
    /// Creates an empty workspace with explicit limits.
    pub fn new(limits: WorkspaceLimits) -> Self {
        Self::new_at_generation(limits, Generation::default())
    }

    /// Creates an empty workspace starting at a host-selected generation.
    pub fn new_at_generation(limits: WorkspaceLimits, generation: Generation) -> Self {
        Self {
            generation,
            limits,
            roots: Arc::default(),
            disk: BTreeMap::new(),
            overlays: BTreeMap::new(),
            retained_resource_count: 0,
            retained_total_bytes: 0,
            effective: Arc::default(),
            dependencies: DependencyGraph::default(),
        }
    }

    /// Returns the current effective-state generation.
    pub const fn generation(&self) -> Generation {
        self.generation
    }

    /// Returns the effective resource, preferring an overlay over disk text.
    pub fn get(&self, id: &ResourceId) -> Option<&Resource> {
        self.effective.get(id)
    }

    /// Returns explicitly registered analysis roots.
    pub fn roots(&self) -> &BTreeSet<ResourceId> {
        &self.roots
    }

    /// Registers an existing resource as an analysis root.
    pub fn register_root(&mut self, id: ResourceId) -> Result<(), WorkspaceError> {
        if !self.effective.contains_key(&id) {
            return Err(WorkspaceError::new(
                WorkspaceErrorCode::MissingResource,
                id.to_string(),
            ));
        }
        if !self.roots.contains(&id) && self.roots.len() >= self.limits.max_roots {
            return Err(WorkspaceError::new(
                WorkspaceErrorCode::ResourceLimit,
                "root limit exceeded",
            ));
        }
        if Arc::make_mut(&mut self.roots).insert(id) {
            self.generation = self.generation.next();
        }
        Ok(())
    }

    /// Removes an analysis root without removing its resource.
    pub fn unregister_root(&mut self, id: &ResourceId) {
        if Arc::make_mut(&mut self.roots).remove(id) {
            self.generation = self.generation.next();
        }
    }

    /// Inserts or replaces disk text and returns roots affected by the change.
    pub fn upsert_disk(
        &mut self,
        id: ResourceId,
        revision: Revision,
        text: impl Into<Arc<str>>,
    ) -> Result<BTreeSet<ResourceId>, WorkspaceError> {
        let resource = Resource {
            id: id.clone(),
            revision,
            text: text.into(),
            layer: ResourceLayer::Disk,
        };
        self.ensure_newer(self.disk.get(&id), &resource)?;
        let (retained_resource_count, retained_total_bytes) =
            self.ensure_capacity(Some((&id, &resource)), None)?;
        self.disk.insert(id.clone(), resource);
        self.retained_resource_count = retained_resource_count;
        self.retained_total_bytes = retained_total_bytes;
        if self.overlays.contains_key(&id) {
            return Ok(BTreeSet::new());
        }
        self.refresh_effective(id)
    }

    /// Inserts or replaces open overlay text and returns affected roots.
    pub fn upsert_overlay(
        &mut self,
        id: ResourceId,
        revision: Revision,
        text: impl Into<Arc<str>>,
    ) -> Result<BTreeSet<ResourceId>, WorkspaceError> {
        let resource = Resource {
            id: id.clone(),
            revision,
            text: text.into(),
            layer: ResourceLayer::Overlay,
        };
        self.ensure_newer(self.overlays.get(&id), &resource)?;
        let (retained_resource_count, retained_total_bytes) =
            self.ensure_capacity(None, Some((&id, &resource)))?;
        self.overlays.insert(id.clone(), resource);
        self.retained_resource_count = retained_resource_count;
        self.retained_total_bytes = retained_total_bytes;
        self.refresh_effective(id)
    }

    /// Closes an overlay, restoring disk text when present.
    pub fn close_overlay(
        &mut self,
        id: &ResourceId,
    ) -> Result<BTreeSet<ResourceId>, WorkspaceError> {
        let Some(overlay) = self.overlays.remove(id) else {
            return Ok(BTreeSet::new());
        };
        self.retained_total_bytes -= overlay.text.len() as u64;
        if !self.disk.contains_key(id) {
            self.retained_resource_count -= 1;
        }
        self.refresh_effective(id.clone())
    }

    /// Removes disk text and returns affected roots.
    ///
    /// An open overlay remains effective until it is closed.
    pub fn remove_disk(&mut self, id: &ResourceId) -> BTreeSet<ResourceId> {
        let Some(disk) = self.disk.remove(id) else {
            return BTreeSet::new();
        };
        self.retained_total_bytes -= disk.text.len() as u64;
        if !self.overlays.contains_key(id) {
            self.retained_resource_count -= 1;
        }
        if self.overlays.contains_key(id) {
            return BTreeSet::new();
        }
        self.remove_effective(id)
    }

    /// Returns registered roots transitively depending on a resource.
    pub fn affected_roots(&self, id: &ResourceId) -> BTreeSet<ResourceId> {
        self.dependencies
            .affected(id)
            .intersection(&self.roots)
            .cloned()
            .collect()
    }

    /// Captures an immutable copy-on-write analysis input.
    pub fn snapshot(&self) -> WorkspaceSnapshot {
        WorkspaceSnapshot {
            generation: self.generation,
            roots: Arc::clone(&self.roots),
            resources: Arc::clone(&self.effective),
        }
    }

    /// Captures only resources accepted by a fallible predicate.
    ///
    /// The predicate runs before each resource and registered root is cloned.
    /// If it returns an error, later resources and roots are not visited or
    /// copied into temporary snapshot state.
    pub fn try_snapshot_resources<E>(
        &self,
        mut retain: impl FnMut(&ResourceId, &Resource) -> Result<bool, E>,
    ) -> Result<WorkspaceSnapshot, E> {
        // A Language Server rebuilds this on every keystroke, and the usual
        // answer is that every resource stays. Copying the whole map to say so
        // made the cost of one keypress grow with the size of the workspace, so
        // the unfiltered answer shares the state it was built from instead.
        // `retain` may charge a budget, so it runs exactly once per resource.
        let mut kept = Vec::with_capacity(self.effective.len());
        let mut excluded = false;
        for (id, resource) in self.effective.iter() {
            let retained = retain(id, resource)?;
            excluded |= !retained;
            kept.push(retained);
        }
        if !excluded {
            return Ok(WorkspaceSnapshot {
                generation: self.generation,
                roots: Arc::clone(&self.roots),
                resources: Arc::clone(&self.effective),
            });
        }
        let mut roots = BTreeSet::new();
        let mut resources = BTreeMap::new();
        for ((id, resource), retained) in self.effective.iter().zip(kept) {
            if !retained {
                continue;
            }
            if self.roots.contains(id) {
                roots.insert(id.clone());
            }
            resources.insert(id.clone(), resource.clone());
        }
        Ok(WorkspaceSnapshot {
            generation: self.generation,
            roots: Arc::new(roots),
            resources: Arc::new(resources),
        })
    }

    /// Adopts dependency information from a result that is still current.
    pub fn accept(&mut self, analysis: &WorkspaceAnalysis) -> Result<(), WorkspaceError> {
        if analysis.generation != self.generation {
            return Err(WorkspaceError::new(
                WorkspaceErrorCode::StaleGeneration,
                "workspace changed while analysis was running",
            ));
        }
        let current = self.effective.get(&analysis.root).ok_or_else(|| {
            WorkspaceError::new(
                WorkspaceErrorCode::MissingResource,
                analysis.root.to_string(),
            )
        })?;
        if current.revision != analysis.root_revision {
            return Err(WorkspaceError::new(
                WorkspaceErrorCode::StaleRevision,
                analysis.root.to_string(),
            ));
        }
        for (owner, dependencies) in &analysis.dependencies {
            self.dependencies
                .replace(owner.clone(), dependencies.clone());
        }
        Ok(())
    }

    /// Validates observed evidence and stamps a draft against the current workspace.
    ///
    /// This is the workspace layer of a two-layer adoption contract. Unrelated
    /// workspace changes may advance the generation without invalidating the
    /// draft. Every resource actually observed by preprocessing is checked by
    /// shared-text identity before publication. Revisions and layers of
    /// non-root resources may change when they retain the same `Arc<str>`.
    ///
    /// A Language Server or another owner of one canonical result must first
    /// apply [`WorkspaceAnalysisDraft::matches_canonical_context`] to enforce
    /// the stricter starting-generation and configuration gate.
    pub fn finalize_draft(
        &self,
        draft: Box<WorkspaceAnalysisDraft>,
    ) -> Result<WorkspaceAnalysis, WorkspaceError> {
        let draft = *draft;
        if !self.roots.contains(&draft.root) {
            return Err(WorkspaceError::new(
                WorkspaceErrorCode::MissingResource,
                "analysis root is not registered",
            ));
        }
        let root = self.effective.get(&draft.root).ok_or_else(|| {
            WorkspaceError::new(WorkspaceErrorCode::MissingResource, draft.root.to_string())
        })?;
        if root.revision != draft.root_revision || !Arc::ptr_eq(&root.text, &draft.root_text) {
            return Err(WorkspaceError::new(
                WorkspaceErrorCode::StaleRevision,
                draft.root.to_string(),
            ));
        }
        for (id, text) in draft.base.iter().chain(&draft.found) {
            let Some(current) = self.effective.get(id) else {
                return Err(WorkspaceError::new(
                    WorkspaceErrorCode::MissingResource,
                    id.to_string(),
                ));
            };
            if !Arc::ptr_eq(&current.text, text) {
                return Err(WorkspaceError::new(
                    WorkspaceErrorCode::StaleRevision,
                    id.to_string(),
                ));
            }
        }
        if let Some(id) = draft
            .missing
            .iter()
            .find(|id| self.effective.contains_key(*id))
        {
            return Err(WorkspaceError::new(
                WorkspaceErrorCode::StaleGeneration,
                format!("resource appeared after authoritative absence: {id}"),
            ));
        }
        let resource_revisions = self
            .effective
            .iter()
            .map(|(id, resource)| (id.clone(), resource.revision))
            .collect();
        Ok(WorkspaceAnalysis {
            generation: self.generation,
            root: draft.root,
            root_revision: root.revision,
            dependencies: draft.dependencies,
            document: draft.document,
            analysis: draft.analysis,
            projection: draft.projection,
            resource_revisions,
            counts: draft.counts,
        })
    }

    fn ensure_newer(
        &self,
        current: Option<&Resource>,
        incoming: &Resource,
    ) -> Result<(), WorkspaceError> {
        if current.is_some_and(|current| incoming.revision <= current.revision) {
            return Err(WorkspaceError::new(
                WorkspaceErrorCode::StaleRevision,
                incoming.id.to_string(),
            ));
        }
        Ok(())
    }

    fn ensure_capacity(
        &self,
        disk_replacement: Option<(&ResourceId, &Resource)>,
        overlay_replacement: Option<(&ResourceId, &Resource)>,
    ) -> Result<(usize, u64), WorkspaceError> {
        let incoming_new = disk_replacement
            .or(overlay_replacement)
            .is_some_and(|(id, _)| !self.disk.contains_key(id) && !self.overlays.contains_key(id));
        let count = self
            .retained_resource_count
            .checked_add(usize::from(incoming_new))
            .filter(|count| *count <= self.limits.resources.max_files)
            .ok_or_else(|| {
                WorkspaceError::new(WorkspaceErrorCode::ResourceLimit, "file limit exceeded")
            })?;
        let replaced_bytes = disk_replacement
            .and_then(|(id, _)| self.disk.get(id))
            .into_iter()
            .chain(overlay_replacement.and_then(|(id, _)| self.overlays.get(id)))
            .try_fold(0_u64, |total, resource| {
                total.checked_add(resource.text.len() as u64)
            })
            .ok_or_else(|| {
                WorkspaceError::new(WorkspaceErrorCode::ResourceLimit, "byte limit exceeded")
            })?;
        let incoming = disk_replacement
            .into_iter()
            .chain(overlay_replacement)
            .try_fold(0_u64, |total, (_, resource)| {
                if resource.text.len() as u64 > self.limits.resources.max_resource_bytes {
                    return Err(WorkspaceError::new(
                        WorkspaceErrorCode::ResourceLimit,
                        "resource byte limit exceeded",
                    ));
                }
                total
                    .checked_add(resource.text.len() as u64)
                    .ok_or_else(|| {
                        WorkspaceError::new(
                            WorkspaceErrorCode::ResourceLimit,
                            "byte limit exceeded",
                        )
                    })
            })?;
        let total = self
            .retained_total_bytes
            .checked_sub(replaced_bytes)
            .and_then(|total| total.checked_add(incoming))
            .filter(|total| *total <= self.limits.resources.max_total_bytes)
            .ok_or_else(|| {
                WorkspaceError::new(
                    WorkspaceErrorCode::ResourceLimit,
                    "total byte limit exceeded",
                )
            })?;
        Ok((count, total))
    }

    fn refresh_effective(
        &mut self,
        id: ResourceId,
    ) -> Result<BTreeSet<ResourceId>, WorkspaceError> {
        let replacement = self
            .overlays
            .get(&id)
            .or_else(|| self.disk.get(&id))
            .cloned();
        if let Some(resource) = replacement {
            Arc::make_mut(&mut self.effective).insert(id.clone(), resource);
        } else {
            Arc::make_mut(&mut self.effective).remove(&id);
            Arc::make_mut(&mut self.roots).remove(&id);
            self.dependencies.remove(&id);
        }
        self.generation = self.generation.next();
        let mut affected = self.affected_roots(&id);
        if self.roots.contains(&id) {
            affected.insert(id);
        }
        Ok(affected)
    }

    fn remove_effective(&mut self, id: &ResourceId) -> BTreeSet<ResourceId> {
        let affected = self.affected_roots(id);
        Arc::make_mut(&mut self.effective).remove(id);
        Arc::make_mut(&mut self.roots).remove(id);
        self.dependencies.remove(id);
        self.generation = self.generation.next();
        affected
    }
}

/// One resource request that suspended workspace analysis.
#[derive(Clone, Debug)]
pub struct WorkspaceResourceRequest {
    inner: ResourceRequest,
}

impl WorkspaceResourceRequest {
    /// Returns the resolved workspace resource identity.
    pub fn target(&self) -> &str {
        self.inner.target()
    }

    /// Returns whether the include declared the resource optional.
    pub const fn is_optional(&self) -> bool {
        self.inner.is_optional()
    }

    /// Supplies immutable UTF-8 text acquired for this request.
    pub fn found(&self, text: impl Into<Arc<str>>) -> WorkspaceResourceResponse {
        let text = text.into();
        let id = ResourceId::new(self.target()).expect("preprocessor returned a valid target");
        WorkspaceResourceResponse {
            inner: self.inner.found(ResourceDocument {
                source_id: SourceId::new(id.to_string()),
                source: Arc::clone(&text),
            }),
            evidence: WorkspaceResourceEvidence::Found { id, text },
        }
    }

    /// Establishes that this requested resource does not exist.
    pub fn not_found(&self) -> WorkspaceResourceResponse {
        let id = ResourceId::new(self.target()).expect("preprocessor returned a valid target");
        WorkspaceResourceResponse {
            inner: self.inner.not_found(),
            evidence: WorkspaceResourceEvidence::Missing(id),
        }
    }

    /// Reports a terminal host loading failure.
    pub fn load_failed(&self, message: impl Into<String>) -> WorkspaceResourceResponse {
        WorkspaceResourceResponse {
            inner: self.inner.load_failed(message),
            evidence: WorkspaceResourceEvidence::Failed,
        }
    }
}

/// Authoritative response used to resume workspace analysis.
#[derive(Clone, Debug)]
pub struct WorkspaceResourceResponse {
    inner: ResourceResponse,
    evidence: WorkspaceResourceEvidence,
}

#[derive(Clone, Debug)]
enum WorkspaceResourceEvidence {
    Found { id: ResourceId, text: Arc<str> },
    Missing(ResourceId),
    Failed,
}

/// Result of starting or resuming workspace analysis.
#[non_exhaustive]
pub enum WorkspaceAnalysisStep {
    /// Preprocessing, core analysis, and origin projection completed once.
    Complete(Box<WorkspaceAnalysisDraft>),
    /// The host must answer one include request before processing can continue.
    NeedResource(Box<SuspendedWorkspaceAnalysis>),
    /// Processing failed without publishing a partial result.
    Failed(WorkspaceError),
    /// Cooperative cancellation discarded all unpublished state.
    Cancelled,
}

/// Opaque, single-use continuation for suspended workspace analysis.
pub struct SuspendedWorkspaceAnalysis {
    continuation: EffectiveSuspendedPreprocess,
    state: WorkspaceAnalysisRun,
    request: WorkspaceResourceRequest,
}

impl SuspendedWorkspaceAnalysis {
    /// Returns the one resource request that must be answered.
    pub const fn request(&self) -> &WorkspaceResourceRequest {
        &self.request
    }

    /// Consumes this continuation and resumes without rebuilding its snapshot.
    pub fn resume(
        self,
        response: WorkspaceResourceResponse,
        cancellation: &impl Cancellation,
    ) -> WorkspaceAnalysisStep {
        let Self {
            continuation,
            mut state,
            request: _,
        } = self;
        let WorkspaceResourceResponse { inner, evidence } = response;
        let step = continuation.resume(inner, &state.lookup, cancellation);
        if !matches!(
            &step,
            EffectivePreprocessStep::HostError(error)
                if error.kind() == HostResourceErrorKind::ResponseMismatch
        ) {
            state.record_evidence(evidence);
        }
        state.advance(step, cancellation)
    }
}

#[derive(Debug)]
struct StableWorkspaceLookup {
    resources: Arc<BTreeMap<ResourceId, Resource>>,
    root: ResourceId,
    observed: Mutex<BTreeSet<ResourceId>>,
}

impl ResourceLookup for StableWorkspaceLookup {
    fn lookup(&self, target: &str) -> ResourceLookupResult {
        let Ok(id) = ResourceId::new(target) else {
            return ResourceLookupResult::Failed("invalid workspace resource identity".to_owned());
        };
        if id == self.root {
            return ResourceLookupResult::Deferred;
        }
        let Some(resource) = self.resources.get(&id) else {
            return ResourceLookupResult::Deferred;
        };
        self.observed
            .lock()
            .expect("workspace lookup evidence lock")
            .insert(id);
        ResourceLookupResult::Ready(ResourceDocument {
            source_id: SourceId::new(resource.id.to_string()),
            source: Arc::clone(&resource.text),
        })
    }
}

struct WorkspaceAnalysisRun {
    base_generation: Generation,
    root: ResourceId,
    root_revision: Revision,
    root_text: Arc<str>,
    options: EffectiveProcessingOptions,
    canonical_options: EffectiveProcessingOptions,
    projection_limits: ProjectionLimits,
    lookup: StableWorkspaceLookup,
    found: BTreeMap<ResourceId, Arc<str>>,
    missing: BTreeSet<ResourceId>,
}

impl WorkspaceAnalysisRun {
    fn advance(
        self,
        step: EffectivePreprocessStep,
        cancellation: &impl Cancellation,
    ) -> WorkspaceAnalysisStep {
        match step {
            EffectivePreprocessStep::Complete(prepared) => {
                #[cfg(test)]
                record_resumable_stage(1);
                let preprocessed = match self.options.analyze_preprocessed(
                    prepared,
                    PreprocessInputs {
                        cancellation: Some(cancellation),
                    },
                ) {
                    Ok(preprocessed) => preprocessed,
                    Err(error) => return processing_error_step(error),
                };
                if adocweave::CancellationCheck::is_cancelled(cancellation) {
                    return WorkspaceAnalysisStep::Cancelled;
                }
                let dependencies = actual_dependencies(&preprocessed.document, &self.root);
                let include_journal = preprocessed
                    .document
                    .directives
                    .iter()
                    .filter(|directive| directive.kind == DirectiveKind::Include)
                    .map(|directive| {
                        let target = ResourceId::new(&directive.target)
                            .expect("preprocessor returned a valid target");
                        let resolution = if self.found.contains_key(&target) {
                            WorkspaceIncludeResolution::DeferredFound
                        } else if self.missing.contains(&target) {
                            WorkspaceIncludeResolution::AuthoritativeMissing
                        } else {
                            WorkspaceIncludeResolution::SnapshotReady
                        };
                        WorkspaceIncludeEvent {
                            target,
                            resolution,
                            optional: directive.optional,
                        }
                    })
                    .collect();
                #[cfg(test)]
                record_resumable_stage(2);
                let projection = match preprocessed
                    .project_origins_cancellable(self.projection_limits, cancellation)
                {
                    Ok(projection) => projection,
                    Err(ProjectionFailure::Cancelled) => return WorkspaceAnalysisStep::Cancelled,
                    Err(error) => {
                        return WorkspaceAnalysisStep::Failed(WorkspaceError::new(
                            WorkspaceErrorCode::Projection,
                            error.to_string(),
                        ));
                    }
                };
                #[cfg(test)]
                record_resumable_stage(3);
                if adocweave::CancellationCheck::is_cancelled(cancellation) {
                    return WorkspaceAnalysisStep::Cancelled;
                }
                let counts = DiagnosticCounts::from_projection(&projection);
                let base = self
                    .lookup
                    .observed
                    .into_inner()
                    .expect("workspace lookup evidence lock")
                    .into_iter()
                    .filter_map(|id| {
                        self.lookup
                            .resources
                            .get(&id)
                            .map(|resource| (id, Arc::clone(&resource.text)))
                    })
                    .collect();
                WorkspaceAnalysisStep::Complete(Box::new(WorkspaceAnalysisDraft {
                    base_generation: self.base_generation,
                    canonical_options: self.canonical_options,
                    root: self.root,
                    root_revision: self.root_revision,
                    root_text: self.root_text,
                    dependencies,
                    document: Arc::new(preprocessed.document),
                    analysis: Arc::new(preprocessed.analysis),
                    projection: Arc::new(projection),
                    counts,
                    base,
                    found: self.found,
                    missing: self.missing,
                    include_journal,
                }))
            }
            EffectivePreprocessStep::NeedResource(continuation) => {
                let request = WorkspaceResourceRequest {
                    inner: continuation.request().clone(),
                };
                if ResourceId::new(request.target()).is_err() {
                    return WorkspaceAnalysisStep::Failed(WorkspaceError::new(
                        WorkspaceErrorCode::InvalidResourceId,
                        request.target(),
                    ));
                }
                WorkspaceAnalysisStep::NeedResource(Box::new(SuspendedWorkspaceAnalysis {
                    continuation: *continuation,
                    state: self,
                    request,
                }))
            }
            EffectivePreprocessStep::Failed(error) => {
                WorkspaceAnalysisStep::Failed(preprocess_workspace_error(error))
            }
            EffectivePreprocessStep::HostError(error) => {
                WorkspaceAnalysisStep::Failed(host_resource_workspace_error(error))
            }
            EffectivePreprocessStep::Cancelled => WorkspaceAnalysisStep::Cancelled,
            _ => WorkspaceAnalysisStep::Failed(WorkspaceError::new(
                WorkspaceErrorCode::Preprocess,
                "unsupported preprocessing suspension state",
            )),
        }
    }

    fn record_evidence(&mut self, evidence: WorkspaceResourceEvidence) {
        #[cfg(test)]
        RESUMABLE_EVIDENCE_RECORDS.with(|records| records.set(records.get() + 1));
        match evidence {
            WorkspaceResourceEvidence::Found { id, text } => {
                self.missing.remove(&id);
                self.found.insert(id, text);
            }
            WorkspaceResourceEvidence::Missing(id) => {
                self.found.remove(&id);
                self.missing.insert(id);
            }
            WorkspaceResourceEvidence::Failed => {}
        }
    }
}

/// Unstamped analysis awaiting validation against live workspace state.
#[derive(Debug)]
pub struct WorkspaceAnalysisDraft {
    base_generation: Generation,
    canonical_options: EffectiveProcessingOptions,
    root: ResourceId,
    root_revision: Revision,
    root_text: Arc<str>,
    dependencies: BTreeMap<ResourceId, BTreeSet<ResourceId>>,
    /// Preprocessed document and source map.
    pub document: Arc<adocweave::preprocess::PreprocessedDocument>,
    /// Core analysis over the expanded source.
    pub analysis: Arc<adocweave::Analysis>,
    /// Diagnostics and queries projected to resource origins.
    pub projection: Arc<AnalysisProjection>,
    /// Projected diagnostic totals.
    pub counts: DiagnosticCounts,
    base: BTreeMap<ResourceId, Arc<str>>,
    found: BTreeMap<ResourceId, Arc<str>>,
    missing: BTreeSet<ResourceId>,
    include_journal: Vec<WorkspaceIncludeEvent>,
}

/// How an executed include was resolved during suspended workspace analysis.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WorkspaceIncludeResolution {
    /// The immutable starting snapshot already contained the resource.
    SnapshotReady,
    /// The host supplied the resource after preprocessing suspended.
    DeferredFound,
    /// The host authoritatively established that the resource was absent.
    AuthoritativeMissing,
}

/// One executed include in source execution order.
///
/// The journal preserves repeated includes as separate events.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WorkspaceIncludeEvent {
    target: ResourceId,
    resolution: WorkspaceIncludeResolution,
    optional: bool,
}

impl WorkspaceIncludeEvent {
    /// Returns the resolved resource identity.
    pub fn target(&self) -> &ResourceId {
        &self.target
    }

    /// Returns how this include was resolved.
    pub const fn resolution(&self) -> WorkspaceIncludeResolution {
        self.resolution
    }

    /// Returns whether the include declared the resource optional.
    pub const fn is_optional(&self) -> bool {
        self.optional
    }
}

impl WorkspaceAnalysisDraft {
    /// Returns the analyzed root identity.
    pub fn root(&self) -> &ResourceId {
        &self.root
    }

    /// Returns includes in execution order, preserving repetitions and outcomes.
    pub fn include_journal(&self) -> &[WorkspaceIncludeEvent] {
        &self.include_journal
    }

    /// Returns the immutable workspace generation used to start this draft.
    pub const fn base_generation(&self) -> Generation {
        self.base_generation
    }

    /// Applies the strict generation and configuration gate required by an integration layer.
    ///
    /// [`Workspace::finalize_draft`] intentionally validates only resources
    /// observed by the run, so unrelated workspace updates may still finalize.
    /// A Language Server or another canonical-result owner must call this
    /// method separately and discard a draft when it returns `false`.
    pub fn matches_canonical_context(
        &self,
        generation: Generation,
        options: &EffectiveProcessingOptions,
    ) -> bool {
        self.base_generation == generation && self.canonical_options.same_contract(options)
    }
}

/// Immutable workspace state safe to move to a worker thread.
#[derive(Clone, Debug)]
pub struct WorkspaceSnapshot {
    generation: Generation,
    roots: Arc<BTreeSet<ResourceId>>,
    resources: Arc<BTreeMap<ResourceId, Resource>>,
}

impl WorkspaceSnapshot {
    /// Returns the captured workspace generation.
    pub const fn generation(&self) -> Generation {
        self.generation
    }

    /// Returns a captured effective resource.
    pub fn get(&self, id: &ResourceId) -> Option<&Resource> {
        self.resources.get(id)
    }

    /// Iterates over captured resources in identity order.
    pub fn resources(&self) -> impl Iterator<Item = (&ResourceId, &Resource)> {
        self.resources.iter()
    }

    /// Produces a snapshot containing only resources accepted by `retain`.
    ///
    /// Registered roots excluded by the predicate are removed from the
    /// returned root set.
    pub fn filter_resources(&self, mut retain: impl FnMut(&ResourceId, &Resource) -> bool) -> Self {
        let mut excluded = false;
        let resources: BTreeMap<ResourceId, Resource> = self
            .resources
            .iter()
            .filter(|(id, resource)| {
                let retained = retain(id, resource);
                excluded |= !retained;
                retained
            })
            .map(|(id, resource)| (id.clone(), resource.clone()))
            .collect();
        if !excluded {
            return self.clone();
        }
        let roots = self
            .roots
            .iter()
            .filter(|root| resources.contains_key(*root))
            .cloned()
            .collect();
        Self {
            generation: self.generation,
            roots: Arc::new(roots),
            resources: Arc::new(resources),
        }
    }

    /// Starts analysis that suspends at the first resource absent from this snapshot.
    ///
    /// The returned continuation retains this immutable lookup and resumes from
    /// the exact include directive without rebuilding or rescanning the snapshot.
    /// The resulting draft records this snapshot generation and the exact
    /// effective-options instance so an integration layer can strictly reject
    /// non-canonical work before calling [`Workspace::finalize_draft`].
    pub fn analyze_resumable(
        &self,
        root: &ResourceId,
        options: &EffectiveProcessingOptions,
        projection_limits: ProjectionLimits,
        cancellation: &impl Cancellation,
    ) -> WorkspaceAnalysisStep {
        if adocweave::CancellationCheck::is_cancelled(cancellation) {
            return WorkspaceAnalysisStep::Cancelled;
        }
        if !self.roots.contains(root) {
            return WorkspaceAnalysisStep::Failed(WorkspaceError::new(
                WorkspaceErrorCode::MissingResource,
                "analysis root is not registered",
            ));
        }
        let Some(root_resource) = self.resources.get(root) else {
            return WorkspaceAnalysisStep::Failed(WorkspaceError::new(
                WorkspaceErrorCode::MissingResource,
                root.to_string(),
            ));
        };
        let canonical_options = options.clone();
        let options = options
            .clone()
            .with_source_id(Some(SourceId::new(root.to_string())));
        let state = WorkspaceAnalysisRun {
            base_generation: self.generation,
            root: root.clone(),
            root_revision: root_resource.revision,
            root_text: Arc::clone(&root_resource.text),
            projection_limits,
            lookup: StableWorkspaceLookup {
                resources: Arc::clone(&self.resources),
                root: root.clone(),
                observed: Mutex::new(BTreeSet::new()),
            },
            options,
            canonical_options,
            found: BTreeMap::new(),
            missing: BTreeSet::new(),
        };
        #[cfg(test)]
        record_resumable_stage(0);
        let step =
            state
                .options
                .preprocess_resumable(&root_resource.text, &state.lookup, cancellation);
        state.advance(step, cancellation)
    }

    /// Preprocesses, analyzes, and projects one registered root.
    ///
    /// Cancellation is checked before and between stages and inside the core
    /// parser. The returned result is not current until [`Workspace::accept`]
    /// succeeds against mutable workspace state.
    pub fn analyze(
        &self,
        root: &ResourceId,
        analysis_options: &AnalysisOptions,
        preprocess_options: &PreprocessOptions,
        projection_limits: ProjectionLimits,
        cancellation: &impl Cancellation,
    ) -> Result<WorkspaceAnalysis, WorkspaceError> {
        let options =
            EffectiveProcessingOptions::new(analysis_options.clone(), preprocess_options.clone())
                .map_err(|error| {
                WorkspaceError::new(WorkspaceErrorCode::InvalidOptions, error.to_string())
            })?;
        self.analyze_with_options(root, &options, projection_limits, cancellation)
    }

    /// Preprocesses, analyzes, and projects one root with validated settings.
    pub fn analyze_with_options(
        &self,
        root: &ResourceId,
        options: &EffectiveProcessingOptions,
        projection_limits: ProjectionLimits,
        cancellation: &impl Cancellation,
    ) -> Result<WorkspaceAnalysis, WorkspaceError> {
        check_cancelled(cancellation)?;
        if !self.roots.contains(root) {
            return Err(WorkspaceError::new(
                WorkspaceErrorCode::MissingResource,
                "analysis root is not registered",
            ));
        }
        let root_resource = self.resources.get(root).ok_or_else(|| {
            WorkspaceError::new(WorkspaceErrorCode::MissingResource, root.to_string())
        })?;
        let snapshot = self
            .resources
            .iter()
            .filter(|(id, _)| *id != root)
            .map(|(id, resource)| {
                (
                    id.to_string(),
                    ResourceDocument {
                        source_id: SourceId::new(id.to_string()),
                        source: Arc::clone(&resource.text),
                    },
                )
            })
            .collect::<ResourceSnapshot>();
        let options = options
            .clone()
            .with_source_id(Some(SourceId::new(root.to_string())));
        let preprocessed = options
            .preprocess_and_analyze(
                &root_resource.text,
                &snapshot,
                PreprocessInputs {
                    cancellation: Some(cancellation),
                },
            )
            .map_err(|error| match error {
                PreprocessedAnalysisError::Options(error) => {
                    WorkspaceError::new(WorkspaceErrorCode::InvalidOptions, error.to_string())
                }
                PreprocessedAnalysisError::Preprocess(error) => {
                    WorkspaceError::new(WorkspaceErrorCode::Preprocess, error.to_string())
                        .with_origin(error.source_id.as_ref(), error.range, error.kind.as_str())
                        .with_requested_resource(
                            (error.kind == PreprocessErrorKind::MissingResource)
                                .then_some(error.target.as_deref())
                                .flatten(),
                        )
                }
                PreprocessedAnalysisError::Parse(error) => {
                    WorkspaceError::new(WorkspaceErrorCode::Analysis, error.to_string())
                }
                PreprocessedAnalysisError::Cancelled => {
                    WorkspaceError::new(WorkspaceErrorCode::Cancelled, "processing was cancelled")
                }
            })?;
        check_cancelled(cancellation)?;
        let dependencies = actual_dependencies(&preprocessed.document, root);
        let projection = preprocessed
            .project_origins_cancellable(projection_limits, cancellation)
            .map_err(|error| {
                let code = if error == ProjectionFailure::Cancelled {
                    WorkspaceErrorCode::Cancelled
                } else {
                    WorkspaceErrorCode::Projection
                };
                WorkspaceError::new(code, error.to_string())
            })?;
        check_cancelled(cancellation)?;
        let counts = DiagnosticCounts::from_projection(&projection);
        let resource_revisions = self
            .resources
            .iter()
            .map(|(id, resource)| (id.clone(), resource.revision))
            .collect();
        Ok(WorkspaceAnalysis {
            generation: self.generation,
            root: root.clone(),
            root_revision: root_resource.revision,
            dependencies,
            document: Arc::new(preprocessed.document),
            analysis: Arc::new(preprocessed.analysis),
            projection: Arc::new(projection),
            resource_revisions,
            counts,
        })
    }
}

/// Severity totals after source-origin projection.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct DiagnosticCounts {
    /// Error occurrences.
    pub errors: usize,
    /// Warning occurrences.
    pub warnings: usize,
    /// Information occurrences.
    pub information: usize,
    /// Hint occurrences.
    pub hints: usize,
}

impl DiagnosticCounts {
    fn from_projection(projection: &AnalysisProjection) -> Self {
        let mut counts = Self::default();
        for item in &projection.diagnostics {
            let count = item.origins.len();
            match item.diagnostic.severity {
                Severity::Error => counts.errors += count,
                Severity::Warning => counts.warnings += count,
                Severity::Information => counts.information += count,
                Severity::Hint => counts.hints += count,
            }
        }
        counts
    }
}

/// Immutable result for one root and workspace generation.
#[derive(Debug)]
pub struct WorkspaceAnalysis {
    generation: Generation,
    root: ResourceId,
    root_revision: Revision,
    dependencies: BTreeMap<ResourceId, BTreeSet<ResourceId>>,
    /// Preprocessed document and source map.
    pub document: Arc<adocweave::preprocess::PreprocessedDocument>,
    /// Core analysis over the expanded source.
    pub analysis: Arc<adocweave::Analysis>,
    /// Diagnostics and queries projected to resource origins.
    pub projection: Arc<AnalysisProjection>,
    /// Revisions captured for all resources in the snapshot.
    pub resource_revisions: BTreeMap<ResourceId, Revision>,
    /// Projected diagnostic totals.
    pub counts: DiagnosticCounts,
}

impl WorkspaceAnalysis {
    /// Returns the captured workspace generation.
    pub const fn generation(&self) -> Generation {
        self.generation
    }

    /// Returns the analyzed root identity.
    pub fn root(&self) -> &ResourceId {
        &self.root
    }

    /// Returns every resource referenced by the analyzed dependency graph.
    pub fn dependencies(&self) -> BTreeSet<ResourceId> {
        self.dependencies
            .values()
            .flat_map(BTreeSet::iter)
            .cloned()
            .collect()
    }

    /// Returns source identities present in directives or diagnostic origins.
    pub fn source_ids(&self) -> BTreeSet<ResourceId> {
        let mut ids = BTreeSet::new();
        for directive in &self.projection.directives {
            for source_id in directive
                .source_id
                .iter()
                .chain(directive.resource_source_id.iter())
            {
                if let Ok(id) = ResourceId::new(source_id.as_str()) {
                    ids.insert(id);
                }
            }
        }
        for diagnostic in &self.projection.diagnostics {
            for source_id in diagnostic
                .origins
                .iter()
                .filter_map(|origin| origin.source_id.as_ref())
            {
                if let Ok(id) = ResourceId::new(source_id.as_str()) {
                    ids.insert(id);
                }
            }
        }
        ids
    }
}

fn check_cancelled(cancellation: &impl Cancellation) -> Result<(), WorkspaceError> {
    if adocweave::CancellationCheck::is_cancelled(cancellation) {
        Err(WorkspaceError::new(
            WorkspaceErrorCode::Cancelled,
            "analysis was cancelled",
        ))
    } else {
        Ok(())
    }
}

fn processing_error_step(error: PreprocessedAnalysisError) -> WorkspaceAnalysisStep {
    match error {
        PreprocessedAnalysisError::Cancelled => WorkspaceAnalysisStep::Cancelled,
        PreprocessedAnalysisError::Options(error) => WorkspaceAnalysisStep::Failed(
            WorkspaceError::new(WorkspaceErrorCode::InvalidOptions, error.to_string()),
        ),
        PreprocessedAnalysisError::Preprocess(error) => {
            WorkspaceAnalysisStep::Failed(preprocess_workspace_error(error))
        }
        PreprocessedAnalysisError::Parse(error) => WorkspaceAnalysisStep::Failed(
            WorkspaceError::new(WorkspaceErrorCode::Analysis, error.to_string()),
        ),
    }
}

fn preprocess_workspace_error(error: PreprocessError) -> WorkspaceError {
    WorkspaceError::new(WorkspaceErrorCode::Preprocess, error.to_string())
        .with_origin(error.source_id.as_ref(), error.range, error.kind.as_str())
        .with_requested_resource(
            (error.kind == PreprocessErrorKind::MissingResource)
                .then_some(error.target.as_deref())
                .flatten(),
        )
}

fn host_resource_workspace_error(
    error: adocweave::preprocess::HostResourceError,
) -> WorkspaceError {
    WorkspaceError::new(WorkspaceErrorCode::Preprocess, error.to_string())
        .with_host_resource(error.kind(), error.target())
}

fn actual_dependencies(
    document: &adocweave::preprocess::PreprocessedDocument,
    root: &ResourceId,
) -> BTreeMap<ResourceId, BTreeSet<ResourceId>> {
    let mut dependencies =
        BTreeMap::<ResourceId, BTreeSet<ResourceId>>::from([(root.clone(), BTreeSet::new())]);
    for directive in &document.directives {
        if directive.kind != DirectiveKind::Include {
            continue;
        }
        let (Some(owner), Some(target)) = (
            directive.source_id.as_ref(),
            directive.resource_source_id.as_ref(),
        ) else {
            continue;
        };
        let (Ok(owner), Ok(target)) = (
            ResourceId::new(owner.as_str()),
            ResourceId::new(target.as_str()),
        ) else {
            continue;
        };
        dependencies
            .entry(owner)
            .or_default()
            .insert(target.clone());
        dependencies.entry(target).or_default();
    }
    dependencies
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;

    fn id(value: &str) -> ResourceId {
        ResourceId::new(value).expect("resource ID")
    }

    fn options() -> PreprocessOptions {
        let mut allowed_schemes = BTreeSet::new();
        allowed_schemes.insert("file".to_owned());
        PreprocessOptions {
            base_uri: Some("file:///book/".to_owned()),
            safe_mode: adocweave::preprocess::SafeMode::Server,
            allowed_schemes,
            ..PreprocessOptions::default()
        }
    }

    #[test]
    fn overlays_are_bounded_and_close_restores_disk() {
        let mut workspace = Workspace::default();
        let root = id("file:///book/root.adoc");
        workspace
            .upsert_disk(root.clone(), Revision::new(1), "disk\n")
            .expect("disk");
        workspace.register_root(root.clone()).expect("root");
        workspace
            .upsert_overlay(root.clone(), Revision::new(5), "overlay\n")
            .expect("overlay");
        assert_eq!(workspace.get(&root).unwrap().text().as_ref(), "overlay\n");
        assert_eq!(
            workspace.get(&root).unwrap().layer(),
            ResourceLayer::Overlay
        );
        workspace.close_overlay(&root).expect("close");
        assert_eq!(workspace.get(&root).unwrap().text().as_ref(), "disk\n");
    }

    #[test]
    fn oversized_overlay_is_rejected_without_replacing_current_text() {
        let limits = WorkspaceLimits {
            resources: RetainedResourceLimits {
                max_files: 2,
                max_total_bytes: 8,
                max_resource_bytes: 8,
            },
            max_roots: 2,
        };
        let mut workspace = Workspace::new(limits);
        let root = id("file:///book/root.adoc");
        workspace
            .upsert_disk(root.clone(), Revision::new(1), "disk")
            .expect("disk");
        assert_eq!(
            workspace
                .upsert_overlay(root.clone(), Revision::new(2), "too large")
                .expect_err("limit")
                .code,
            WorkspaceErrorCode::ResourceLimit
        );
        assert_eq!(workspace.get(&root).unwrap().text().as_ref(), "disk");
    }

    #[test]
    fn retained_layer_budget_rejects_transactionally_and_releases_each_layer() {
        let limits = RetainedResourceLimits {
            max_files: 1,
            max_total_bytes: 5,
            max_resource_bytes: 4,
        };
        let root = id("file:///book/root.adoc");
        let disk = RetainedResourceBudget::default()
            .with_disk(root.clone(), Some(3), limits)
            .expect("disk charge");

        assert_eq!(
            disk.with_overlay(root.clone(), Some(3), limits)
                .expect_err("combined layer limit")
                .code,
            WorkspaceErrorCode::ResourceLimit
        );
        let overlay = disk
            .with_disk(root.clone(), None, limits)
            .expect("disk release")
            .with_overlay(root.clone(), Some(4), limits)
            .expect("overlay charge after release");
        overlay
            .with_overlay(root, None, limits)
            .expect("overlay release");
    }

    #[test]
    fn retained_budget_cached_totals_cover_layers_replacement_and_failure() {
        let limits = RetainedResourceLimits {
            max_files: 2,
            max_total_bytes: 7,
            max_resource_bytes: 5,
        };
        let first = id("file:///first.adoc");
        let second = id("file:///second.adoc");
        let mut budget = RetainedResourceBudget::default();
        budget
            .try_replace_layers(
                first.clone(),
                RetainedLayerCharge::new(Some(2), Some(3)),
                limits,
            )
            .expect("two layers under one identity");
        assert_eq!(budget.resource_count, 1);
        assert_eq!(budget.total_bytes, 5);
        budget
            .try_replace_layers(
                first.clone(),
                RetainedLayerCharge::new(Some(1), None),
                limits,
            )
            .expect("replace and remove overlay");
        budget
            .try_replace_layers(
                second.clone(),
                RetainedLayerCharge::new(None, Some(5)),
                limits,
            )
            .expect("second identity");
        assert_eq!(budget.resource_count, 2);
        assert_eq!(budget.total_bytes, 6);

        let before = budget.clone();
        budget
            .try_replace_layers(second, RetainedLayerCharge::new(Some(5), Some(5)), limits)
            .expect_err("total limit");
        assert_eq!(budget.resources, before.resources);
        assert_eq!(budget.resource_count, before.resource_count);
        assert_eq!(budget.total_bytes, before.total_bytes);

        budget
            .try_replace_layers(first, RetainedLayerCharge::default(), limits)
            .expect("remove both layers");
        assert_eq!(budget.resource_count, 1);
        assert_eq!(budget.total_bytes, 5);
    }

    #[test]
    fn mutable_budget_and_workspace_accept_the_ten_thousand_resource_boundary() {
        let limits = RetainedResourceLimits {
            max_files: 10_000,
            max_total_bytes: 10_000,
            max_resource_bytes: 1,
        };
        let mut budget = RetainedResourceBudget::default();
        let mut workspace = Workspace::new(WorkspaceLimits {
            resources: limits,
            max_roots: 10_000,
        });
        for index in 0..10_000 {
            let id = id(&format!("file:///{index}.adoc"));
            budget
                .try_replace_layers(id.clone(), RetainedLayerCharge::new(Some(1), None), limits)
                .expect("budget boundary");
            workspace
                .upsert_disk(id, Revision::new(1), "x")
                .expect("workspace boundary");
        }
        assert_eq!(budget.resource_count, 10_000);
        assert_eq!(budget.total_bytes, 10_000);
        assert_eq!(workspace.retained_resource_count, 10_000);
        assert_eq!(workspace.retained_total_bytes, 10_000);

        let rejected = id("file:///rejected.adoc");
        budget
            .try_replace_layers(
                rejected.clone(),
                RetainedLayerCharge::new(Some(1), None),
                limits,
            )
            .expect_err("budget count boundary");
        workspace
            .upsert_disk(rejected, Revision::new(1), "x")
            .expect_err("workspace count boundary");
        assert_eq!(budget.resource_count, 10_000);
        assert_eq!(workspace.retained_resource_count, 10_000);
    }

    #[test]
    fn workspace_cached_totals_follow_same_id_layers_replacement_and_removal() {
        let resource = id("file:///resource.adoc");
        let mut workspace = Workspace::new(WorkspaceLimits {
            resources: RetainedResourceLimits {
                max_files: 1,
                max_total_bytes: 6,
                max_resource_bytes: 4,
            },
            max_roots: 1,
        });
        workspace
            .upsert_disk(resource.clone(), Revision::new(1), "aa")
            .expect("disk");
        workspace
            .upsert_overlay(resource.clone(), Revision::new(2), "bbb")
            .expect("same identity overlay");
        assert_eq!(workspace.retained_resource_count, 1);
        assert_eq!(workspace.retained_total_bytes, 5);

        let before = workspace.clone();
        workspace
            .upsert_disk(resource.clone(), Revision::new(3), "xxxx")
            .expect_err("combined layer total");
        assert_eq!(
            workspace.retained_resource_count,
            before.retained_resource_count
        );
        assert_eq!(workspace.retained_total_bytes, before.retained_total_bytes);
        assert_eq!(
            workspace.disk.get(&resource).map(Resource::text),
            before.disk.get(&resource).map(Resource::text),
        );

        workspace.close_overlay(&resource).expect("close overlay");
        assert_eq!(workspace.retained_resource_count, 1);
        assert_eq!(workspace.retained_total_bytes, 2);
        workspace.remove_disk(&resource);
        assert_eq!(workspace.retained_resource_count, 0);
        assert_eq!(workspace.retained_total_bytes, 0);
    }

    #[test]
    fn cancelled_analysis_stops_before_work() {
        let mut workspace = Workspace::default();
        let root = id("file:///book/root.adoc");
        workspace
            .upsert_disk(root.clone(), Revision::new(1), "root\n")
            .expect("disk");
        workspace.register_root(root.clone()).expect("root");
        let cancellation = adocweave::CancellationToken::new();
        cancellation.cancel();
        assert_eq!(
            workspace
                .snapshot()
                .analyze(
                    &root,
                    &AnalysisOptions::default(),
                    &options(),
                    ProjectionLimits::default(),
                    &cancellation,
                )
                .expect_err("cancelled")
                .code,
            WorkspaceErrorCode::Cancelled
        );
    }

    #[test]
    fn actual_attribute_expanded_dependencies_select_only_roots() {
        let mut workspace = Workspace::default();
        let root = id("file:///book/root.adoc");
        let other = id("file:///book/other.adoc");
        let part = id("file:///book/part.adoc");
        workspace
            .upsert_disk(
                root.clone(),
                Revision::new(1),
                ":part: part\ninclude::{part}.adoc[]\n",
            )
            .expect("root");
        workspace
            .upsert_disk(other.clone(), Revision::new(1), "other\n")
            .expect("other");
        workspace
            .upsert_disk(part.clone(), Revision::new(1), "part\n")
            .expect("part");
        workspace.register_root(root.clone()).expect("root");
        workspace.register_root(other).expect("other");

        let result = workspace
            .snapshot()
            .analyze(
                &root,
                &AnalysisOptions::default(),
                &options(),
                ProjectionLimits::default(),
                &NeverCancelled,
            )
            .expect("analysis");
        workspace.accept(&result).expect("accept");
        assert_eq!(
            workspace.affected_roots(&part),
            BTreeSet::from([root.clone()])
        );
    }

    #[test]
    fn effective_options_share_external_attributes_across_workspace_stages() {
        let mut workspace = Workspace::default();
        let root = id("file:///book/root.adoc");
        let part = id("file:///book/part.adoc");
        workspace
            .upsert_disk(
                root.clone(),
                Revision::new(1),
                "ifdef::selected[]\ninclude::{selected}.adoc[]\nendif::[]\n",
            )
            .expect("root");
        workspace
            .upsert_disk(
                part.clone(),
                Revision::new(1),
                ":selected: other\nincluded {selected}\n",
            )
            .expect("part");
        workspace.register_root(root.clone()).expect("root");
        let attributes = BTreeMap::from([("selected".to_owned(), Some("part".to_owned()))]);
        let mut analysis = AnalysisOptions::default();
        analysis.attributes.clone_from(&attributes);
        let mut preprocess = options();
        preprocess.attributes = attributes;
        let effective = EffectiveProcessingOptions::new(analysis, preprocess)
            .expect("matching processing options");

        let result = workspace
            .snapshot()
            .analyze_with_options(
                &root,
                &effective,
                ProjectionLimits::default(),
                &NeverCancelled,
            )
            .expect("workspace analysis");

        assert_eq!(
            result
                .analysis
                .attribute_environment()
                .final_values()
                .get("selected")
                .map(String::as_str),
            Some("part")
        );
        assert_eq!(
            result.dependencies.get(&root),
            Some(&BTreeSet::from([part]))
        );
    }

    #[test]
    fn workspace_compatibility_entry_rejects_mismatch_before_root_lookup() {
        for mismatch in 0..3 {
            let analysis = AnalysisOptions::default();
            let mut preprocess = options();
            match mismatch {
                0 => {
                    preprocess
                        .attributes
                        .insert("different".to_owned(), Some("value".to_owned()));
                }
                1 => preprocess.max_attribute_expansion_depth += 1,
                2 => preprocess.max_attribute_expansion_bytes += 1,
                _ => unreachable!(),
            }

            let error = Workspace::default()
                .snapshot()
                .analyze(
                    &id("missing"),
                    &analysis,
                    &preprocess,
                    ProjectionLimits::default(),
                    &NeverCancelled,
                )
                .expect_err("options must be checked first");

            assert_eq!(error.code, WorkspaceErrorCode::InvalidOptions);
            assert_eq!(error.diagnostic_code(), "invalid-options");
        }
    }

    #[test]
    fn workspace_uses_the_effective_attribute_expansion_boundaries() {
        let mut workspace = Workspace::default();
        let root = id("file:///book/root.adoc");
        let part = id("file:///book/12345.adoc");
        workspace
            .upsert_disk(
                root.clone(),
                Revision::new(1),
                ":base: 12345\n:expanded: {base}\ninclude::{expanded}.adoc[]\n",
            )
            .expect("root");
        workspace
            .upsert_disk(part, Revision::new(1), "included\n")
            .expect("part");
        workspace.register_root(root.clone()).expect("root");

        for (depth, bytes, expected) in [
            (1, 5, Ok(())),
            (0, 5, Err("missing-resource")),
            (1, 4, Err("missing-resource")),
        ] {
            let mut analysis = AnalysisOptions::default();
            analysis.syntax.limits.max_attribute_expansion_depth = depth;
            analysis.syntax.limits.max_attribute_expansion_bytes = bytes;
            let mut preprocess = options();
            preprocess.max_attribute_expansion_depth = depth;
            preprocess.max_attribute_expansion_bytes = bytes;
            let result = workspace.snapshot().analyze(
                &root,
                &analysis,
                &preprocess,
                ProjectionLimits::default(),
                &NeverCancelled,
            );
            match expected {
                Ok(()) => {
                    result.expect("accepted boundary");
                }
                Err(code) => {
                    assert_eq!(
                        result.expect_err("rejected boundary").diagnostic_code(),
                        code
                    );
                }
            }
        }
    }

    #[test]
    fn missing_include_error_preserves_the_requested_resource_identity() {
        let mut workspace = Workspace::default();
        let root = id("file:///book/root.adoc");
        let missing = id("file:///book/generated/part.adoc");
        workspace
            .upsert_disk(
                root.clone(),
                Revision::new(1),
                "include::generated/part.adoc[]\n",
            )
            .expect("root");
        workspace.register_root(root.clone()).expect("root");

        let error = workspace
            .snapshot()
            .analyze(
                &root,
                &AnalysisOptions::default(),
                &options(),
                ProjectionLimits::default(),
                &NeverCancelled,
            )
            .expect_err("missing include");

        assert_eq!(error.diagnostic_code(), "missing-resource");
        assert_eq!(error.requested_resource(), Some(&missing));
    }

    #[test]
    fn stale_analysis_is_rejected_after_concurrent_update() {
        let mut workspace = Workspace::default();
        let root = id("file:///book/root.adoc");
        workspace
            .upsert_disk(root.clone(), Revision::new(1), "root\n")
            .expect("disk");
        workspace.register_root(root.clone()).expect("root");
        let result = workspace
            .snapshot()
            .analyze(
                &root,
                &AnalysisOptions::default(),
                &options(),
                ProjectionLimits::default(),
                &NeverCancelled,
            )
            .expect("analysis");
        workspace
            .upsert_overlay(root.clone(), Revision::new(2), "changed\n")
            .expect("update");
        assert_eq!(
            workspace.accept(&result).expect_err("stale").code,
            WorkspaceErrorCode::StaleGeneration
        );
    }

    #[test]
    fn cancellation_during_preprocessing_returns_no_partial_analysis() {
        struct CancelDuringPreprocessing(AtomicUsize);

        impl adocweave::CancellationCheck for CancelDuringPreprocessing {
            fn is_cancelled(&self) -> bool {
                self.0.fetch_add(1, Ordering::Relaxed) >= 3
            }
        }

        let mut workspace = Workspace::default();
        let root = id("file:///book/root.adoc");
        workspace
            .upsert_disk(root.clone(), Revision::new(1), "paragraph\n".repeat(10_000))
            .expect("disk");
        workspace.register_root(root.clone()).expect("root");
        let error = workspace
            .snapshot()
            .analyze(
                &root,
                &AnalysisOptions::default(),
                &options(),
                ProjectionLimits::default(),
                &CancelDuringPreprocessing(AtomicUsize::new(0)),
            )
            .expect_err("cancelled");
        assert_eq!(error.code, WorkspaceErrorCode::Cancelled);
    }

    #[test]
    fn snapshots_share_resource_text_and_are_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<WorkspaceSnapshot>();
        assert_send_sync::<WorkspaceAnalysis>();
        assert_send_sync::<WorkspaceAnalysisDraft>();
        assert_send_sync::<WorkspaceAnalysisStep>();
        assert_send_sync::<SuspendedWorkspaceAnalysis>();
        assert_send_sync::<WorkspaceResourceRequest>();
        assert_send_sync::<WorkspaceResourceResponse>();
        assert_send_sync::<WorkspaceIncludeEvent>();
        assert_send_sync::<WorkspaceIncludeResolution>();
        assert_send_sync::<WorkspaceHostResourceErrorKind>();

        let mut workspace = Workspace::default();
        let root = id("file:///book/root.adoc");
        workspace
            .upsert_disk(root.clone(), Revision::new(1), "root\n")
            .expect("disk");
        let before = Arc::clone(workspace.get(&root).unwrap().text());
        let snapshot = workspace.snapshot();
        let after = Arc::clone(&snapshot.resources.get(&root).unwrap().text);
        assert!(Arc::ptr_eq(&before, &after));
    }

    #[test]
    fn fallible_snapshot_stops_before_later_roots_and_resources_are_cloned() {
        let mut workspace = Workspace::default();
        let last_text: Arc<str> = Arc::from("last");
        for revision in 0..128 {
            let name = format!("resource-{revision:03}");
            let id = ResourceId::new(name.clone()).expect("resource ID");
            let text: Arc<str> = if revision == 127 {
                Arc::clone(&last_text)
            } else {
                Arc::from(name)
            };
            workspace
                .upsert_disk(id.clone(), Revision::new(revision), text)
                .expect("resource");
            workspace.register_root(id).expect("root");
        }
        let mut visited = Vec::new();
        let last_text_references = Arc::strong_count(&last_text);

        let result = workspace.try_snapshot_resources(|id, _| {
            visited.push(id.to_string());
            if visited.len() == 2 {
                return Err("limit");
            }
            Ok(true)
        });

        assert!(matches!(result, Err("limit")));
        assert_eq!(visited, ["resource-000", "resource-001"]);
        assert_eq!(Arc::strong_count(&last_text), last_text_references);
    }

    #[test]
    fn direct_fallible_snapshot_registers_only_accepted_roots() {
        let mut workspace = Workspace::default();
        let accepted = id("accepted");
        let rejected = id("rejected");
        workspace
            .upsert_disk(accepted.clone(), Revision::new(1), "accepted")
            .expect("accepted resource");
        workspace
            .upsert_disk(rejected.clone(), Revision::new(1), "rejected")
            .expect("rejected resource");
        workspace
            .register_root(accepted.clone())
            .expect("accepted root");
        workspace
            .register_root(rejected.clone())
            .expect("rejected root");

        let snapshot = workspace
            .try_snapshot_resources::<std::convert::Infallible>(|id, _| Ok(id == &accepted))
            .expect("snapshot");

        assert_eq!(snapshot.resources().count(), 1);
        assert!(snapshot.get(&accepted).is_some());
        assert!(snapshot.get(&rejected).is_none());
        assert_eq!(*snapshot.roots, BTreeSet::from([accepted]));
    }

    /// A snapshot that keeps everything shares the state it was built from.
    ///
    /// A Language Server rebuilds this on every keystroke. Copying the map to
    /// say "all of it" made one keypress cost time proportional to the whole
    /// workspace, so the identity of the shared allocation is the property
    /// under test, not just the contents.
    #[test]
    fn an_unfiltered_snapshot_shares_workspace_state_instead_of_copying_it() {
        let mut workspace = Workspace::default();
        for index in 0..8 {
            let resource = id(&format!("resource-{index}"));
            workspace
                .upsert_disk(resource.clone(), Revision::new(1), "text")
                .expect("resource");
            workspace.register_root(resource).expect("root");
        }

        let shared = workspace
            .try_snapshot_resources::<std::convert::Infallible>(|_, _| Ok(true))
            .expect("snapshot");
        assert!(Arc::ptr_eq(&shared.resources, &workspace.effective));
        assert!(Arc::ptr_eq(&shared.roots, &workspace.roots));
        assert_eq!(shared.resources().count(), 8);

        let filtered = workspace
            .try_snapshot_resources::<std::convert::Infallible>(|id, _| {
                Ok(id != &self::id("resource-0"))
            })
            .expect("snapshot");
        assert!(!Arc::ptr_eq(&filtered.resources, &workspace.effective));
        assert_eq!(filtered.resources().count(), 7);

        // Filtering an already-shared snapshot keeps the same guarantee.
        assert!(Arc::ptr_eq(
            &shared.filter_resources(|_, _| true).resources,
            &workspace.effective,
        ));
        assert_eq!(
            shared
                .filter_resources(|id, _| id != &self::id("resource-0"))
                .resources()
                .count(),
            7,
        );
    }

    fn effective_options() -> EffectiveProcessingOptions {
        EffectiveProcessingOptions::new(AnalysisOptions::default(), options())
            .expect("effective options")
    }

    #[test]
    fn resumable_workspace_analysis_uses_each_stage_once_and_finalizes_evidence() {
        RESUMABLE_STAGE_RUNS.with(|runs| runs.set([0; 4]));
        let mut workspace = Workspace::default();
        let root = id("file:///book/root.adoc");
        let base = id("file:///book/base.adoc");
        let loaded = id("file:///book/loaded.adoc");
        let missing = id("file:///book/missing.adoc");
        workspace
            .upsert_disk(
                root.clone(),
                Revision::new(1),
                "include::base.adoc[]\ninclude::loaded.adoc[]\ninclude::base.adoc[]\ninclude::loaded.adoc[]\ninclude::missing.adoc[optional]\n",
            )
            .expect("root");
        workspace
            .upsert_disk(base.clone(), Revision::new(2), "base\n")
            .expect("base");
        workspace.register_root(root.clone()).expect("root");
        let snapshot = workspace.snapshot();
        let canonical_options = effective_options();

        let WorkspaceAnalysisStep::NeedResource(loaded_continuation) = snapshot.analyze_resumable(
            &root,
            &canonical_options,
            ProjectionLimits::default(),
            &NeverCancelled,
        ) else {
            panic!("missing loaded resource must suspend analysis");
        };
        assert_eq!(loaded_continuation.request().target(), loaded.as_str());
        let loaded_text = Arc::<str>::from("loaded\n");
        let loaded_response = loaded_continuation
            .request()
            .found(Arc::clone(&loaded_text));
        let WorkspaceAnalysisStep::NeedResource(missing_continuation) =
            loaded_continuation.resume(loaded_response, &NeverCancelled)
        else {
            panic!("optional missing resource must suspend analysis");
        };
        assert_eq!(missing_continuation.request().target(), missing.as_str());
        let missing_response = missing_continuation.request().not_found();
        let WorkspaceAnalysisStep::Complete(draft) =
            missing_continuation.resume(missing_response, &NeverCancelled)
        else {
            panic!("authoritative absence must complete analysis");
        };

        assert_eq!(
            draft.include_journal(),
            &[
                WorkspaceIncludeEvent {
                    target: base,
                    resolution: WorkspaceIncludeResolution::SnapshotReady,
                    optional: false,
                },
                WorkspaceIncludeEvent {
                    target: loaded.clone(),
                    resolution: WorkspaceIncludeResolution::DeferredFound,
                    optional: false,
                },
                WorkspaceIncludeEvent {
                    target: id("file:///book/base.adoc"),
                    resolution: WorkspaceIncludeResolution::SnapshotReady,
                    optional: false,
                },
                WorkspaceIncludeEvent {
                    target: loaded.clone(),
                    resolution: WorkspaceIncludeResolution::DeferredFound,
                    optional: false,
                },
                WorkspaceIncludeEvent {
                    target: missing,
                    resolution: WorkspaceIncludeResolution::AuthoritativeMissing,
                    optional: true,
                },
            ]
        );
        RESUMABLE_STAGE_RUNS.with(|runs| assert_eq!(runs.get(), [1, 1, 1, 1]));
        assert!(draft.matches_canonical_context(snapshot.generation(), &canonical_options));
        assert!(!draft.matches_canonical_context(snapshot.generation(), &effective_options()));
        workspace
            .upsert_disk(loaded.clone(), Revision::new(3), Arc::clone(&loaded_text))
            .expect("stage loaded resource");
        assert!(!draft.matches_canonical_context(workspace.generation(), &canonical_options));
        let analysis = workspace.finalize_draft(draft).expect("current evidence");
        assert_eq!(analysis.generation(), workspace.generation());
        assert_eq!(analysis.resource_revisions[&loaded], Revision::new(3));
        workspace.accept(&analysis).expect("accepted analysis");
    }

    #[test]
    fn draft_rejects_found_text_without_shared_identity() {
        let mut workspace = Workspace::default();
        let root = id("file:///book/root.adoc");
        let loaded = id("file:///book/loaded.adoc");
        workspace
            .upsert_disk(root.clone(), Revision::new(1), "include::loaded.adoc[]\n")
            .expect("root");
        workspace.register_root(root.clone()).expect("root");
        let WorkspaceAnalysisStep::NeedResource(continuation) =
            workspace.snapshot().analyze_resumable(
                &root,
                &effective_options(),
                ProjectionLimits::default(),
                &NeverCancelled,
            )
        else {
            panic!("analysis must suspend");
        };
        let acquired = Arc::<str>::from("same bytes\n");
        let response = continuation.request().found(Arc::clone(&acquired));
        let WorkspaceAnalysisStep::Complete(draft) = continuation.resume(response, &NeverCancelled)
        else {
            panic!("analysis must complete");
        };
        workspace
            .upsert_disk(loaded, Revision::new(2), Arc::<str>::from("same bytes\n"))
            .expect("different allocation");

        assert_eq!(
            workspace
                .finalize_draft(draft)
                .expect_err("found evidence identity must match")
                .code,
            WorkspaceErrorCode::StaleRevision
        );
    }

    #[test]
    fn draft_rejects_changed_base_root_and_missing_evidence() {
        let mut workspace = Workspace::default();
        let root = id("file:///book/root.adoc");
        let base = id("file:///book/base.adoc");
        let missing = id("file:///book/missing.adoc");
        workspace
            .upsert_disk(
                root.clone(),
                Revision::new(1),
                "include::base.adoc[]\ninclude::missing.adoc[optional]\n",
            )
            .expect("root");
        workspace
            .upsert_disk(base.clone(), Revision::new(1), "base\n")
            .expect("base");
        workspace.register_root(root.clone()).expect("root");
        let snapshot = workspace.snapshot();
        let WorkspaceAnalysisStep::NeedResource(continuation) = snapshot.analyze_resumable(
            &root,
            &effective_options(),
            ProjectionLimits::default(),
            &NeverCancelled,
        ) else {
            panic!("analysis must suspend");
        };
        let response = continuation.request().not_found();
        let WorkspaceAnalysisStep::Complete(missing_draft) =
            continuation.resume(response, &NeverCancelled)
        else {
            panic!("analysis must complete");
        };
        workspace
            .upsert_disk(missing, Revision::new(1), "now present\n")
            .expect("appeared resource");
        assert_eq!(
            workspace
                .finalize_draft(missing_draft)
                .expect_err("missing evidence must remain absent")
                .code,
            WorkspaceErrorCode::StaleGeneration
        );

        let WorkspaceAnalysisStep::NeedResource(continuation) = snapshot.analyze_resumable(
            &root,
            &effective_options(),
            ProjectionLimits::default(),
            &NeverCancelled,
        ) else {
            panic!("analysis must suspend again");
        };
        let response = continuation.request().not_found();
        let WorkspaceAnalysisStep::Complete(base_draft) =
            continuation.resume(response, &NeverCancelled)
        else {
            panic!("analysis must complete again");
        };
        workspace
            .upsert_disk(base, Revision::new(2), "changed base\n")
            .expect("changed base");
        assert_eq!(
            workspace
                .finalize_draft(base_draft)
                .expect_err("base evidence must remain identical")
                .code,
            WorkspaceErrorCode::StaleRevision
        );

        let WorkspaceAnalysisStep::NeedResource(continuation) = snapshot.analyze_resumable(
            &root,
            &effective_options(),
            ProjectionLimits::default(),
            &NeverCancelled,
        ) else {
            panic!("analysis must suspend for root validation");
        };
        let response = continuation.request().not_found();
        let WorkspaceAnalysisStep::Complete(root_draft) =
            continuation.resume(response, &NeverCancelled)
        else {
            panic!("analysis must complete for root validation");
        };
        workspace
            .upsert_disk(root, Revision::new(2), "changed root\n")
            .expect("changed root");
        assert_eq!(
            workspace
                .finalize_draft(root_draft)
                .expect_err("root evidence must remain current")
                .code,
            WorkspaceErrorCode::StaleRevision
        );
    }

    struct CancelAtResumableStage(usize);

    impl adocweave::CancellationCheck for CancelAtResumableStage {
        fn is_cancelled(&self) -> bool {
            RESUMABLE_STAGE_RUNS.with(|runs| runs.get()[self.0] > 0)
        }
    }

    #[test]
    fn resumable_analysis_discards_analysis_projection_and_pre_draft_cancellation() {
        let mut workspace = Workspace::default();
        let root = id("file:///book/root.adoc");
        workspace
            .upsert_disk(root.clone(), Revision::new(1), "paragraph\n")
            .expect("root");
        workspace.register_root(root.clone()).expect("root");
        let snapshot = workspace.snapshot();

        for stage in 1..=3 {
            RESUMABLE_STAGE_RUNS.with(|runs| runs.set([0; 4]));
            assert!(matches!(
                snapshot.analyze_resumable(
                    &root,
                    &effective_options(),
                    ProjectionLimits::default(),
                    &CancelAtResumableStage(stage),
                ),
                WorkspaceAnalysisStep::Cancelled
            ));
            RESUMABLE_STAGE_RUNS.with(|runs| {
                let actual = runs.get();
                assert_eq!(actual[0], 1);
                assert_eq!(actual[1], 1);
                assert_eq!(actual[2], usize::from(stage >= 2));
                assert_eq!(actual[3], usize::from(stage >= 3));
            });
        }
    }

    #[test]
    fn workspace_preserves_host_failure_kinds_and_rejects_foreign_responses() {
        let mut workspace = Workspace::default();
        let first_root = id("file:///book/first.adoc");
        let second_root = id("file:///book/second.adoc");
        workspace
            .upsert_disk(
                first_root.clone(),
                Revision::new(1),
                "include::first-part.adoc[]\n",
            )
            .expect("first root");
        workspace
            .upsert_disk(
                second_root.clone(),
                Revision::new(1),
                "include::second-part.adoc[]\n",
            )
            .expect("second root");
        workspace
            .register_root(first_root.clone())
            .expect("first root");
        workspace
            .register_root(second_root.clone())
            .expect("second root");
        let snapshot = workspace.snapshot();
        let options = effective_options();
        let WorkspaceAnalysisStep::NeedResource(first) = snapshot.analyze_resumable(
            &first_root,
            &options,
            ProjectionLimits::default(),
            &NeverCancelled,
        ) else {
            panic!("first run must suspend");
        };
        let WorkspaceAnalysisStep::NeedResource(second) = snapshot.analyze_resumable(
            &second_root,
            &options,
            ProjectionLimits::default(),
            &NeverCancelled,
        ) else {
            panic!("second run must suspend");
        };
        let foreign_response = first.request().found("foreign\n");
        RESUMABLE_EVIDENCE_RECORDS.with(|records| records.set(0));
        let WorkspaceAnalysisStep::Failed(error) = second.resume(foreign_response, &NeverCancelled)
        else {
            panic!("foreign response must fail");
        };
        assert_eq!(error.code, WorkspaceErrorCode::Preprocess);
        assert_eq!(
            error.host_resource_kind(),
            Some(WorkspaceHostResourceErrorKind::ResponseMismatch)
        );
        assert_eq!(error.diagnostic_code(), "host-resource-response-mismatch");
        RESUMABLE_EVIDENCE_RECORDS.with(|records| assert_eq!(records.get(), 0));

        let WorkspaceAnalysisStep::NeedResource(same_run_first) = snapshot.analyze_resumable(
            &first_root,
            &options,
            ProjectionLimits::default(),
            &NeverCancelled,
        ) else {
            panic!("same target first run must suspend");
        };
        let WorkspaceAnalysisStep::NeedResource(same_run_second) = snapshot.analyze_resumable(
            &first_root,
            &options,
            ProjectionLimits::default(),
            &NeverCancelled,
        ) else {
            panic!("same target second run must suspend");
        };
        let stale_response = same_run_first.request().found("stale\n");
        RESUMABLE_EVIDENCE_RECORDS.with(|records| records.set(0));
        let WorkspaceAnalysisStep::Failed(error) =
            same_run_second.resume(stale_response, &NeverCancelled)
        else {
            panic!("response from another run must fail");
        };
        assert_eq!(
            error.host_resource_kind(),
            Some(WorkspaceHostResourceErrorKind::ResponseMismatch)
        );
        RESUMABLE_EVIDENCE_RECORDS.with(|records| assert_eq!(records.get(), 0));

        let WorkspaceAnalysisStep::NeedResource(load) = snapshot.analyze_resumable(
            &first_root,
            &options,
            ProjectionLimits::default(),
            &NeverCancelled,
        ) else {
            panic!("load failure run must suspend");
        };
        let target = id(load.request().target());
        let response = load.request().load_failed("permission denied");
        let WorkspaceAnalysisStep::Failed(error) = load.resume(response, &NeverCancelled) else {
            panic!("load failure must remain typed");
        };
        assert_eq!(error.code, WorkspaceErrorCode::Preprocess);
        assert_eq!(
            error.host_resource_kind(),
            Some(WorkspaceHostResourceErrorKind::LoadFailed)
        );
        assert_eq!(error.requested_resource(), Some(&target));
        assert_eq!(error.diagnostic_code(), "host-resource-load-failed");
    }

    #[test]
    fn non_root_revision_and_layer_changes_accept_the_same_shared_text() {
        let mut workspace = Workspace::default();
        let root = id("file:///book/root.adoc");
        let base = id("file:///book/base.adoc");
        let loaded = id("file:///book/loaded.adoc");
        let unrelated = id("file:///book/unrelated.adoc");
        let root_text = Arc::<str>::from("include::base.adoc[]\ninclude::loaded.adoc[]\n");
        let base_text = Arc::<str>::from("base\n");
        workspace
            .upsert_disk(root.clone(), Revision::new(1), Arc::clone(&root_text))
            .expect("root");
        workspace
            .upsert_disk(base.clone(), Revision::new(1), Arc::clone(&base_text))
            .expect("base");
        workspace.register_root(root.clone()).expect("root");
        let snapshot = workspace.snapshot();
        let canonical_options = effective_options();
        let WorkspaceAnalysisStep::NeedResource(continuation) = snapshot.analyze_resumable(
            &root,
            &canonical_options,
            ProjectionLimits::default(),
            &NeverCancelled,
        ) else {
            panic!("loaded resource must suspend");
        };
        let loaded_text = Arc::<str>::from("loaded\n");
        let response = continuation.request().found(Arc::clone(&loaded_text));
        let WorkspaceAnalysisStep::Complete(draft) = continuation.resume(response, &NeverCancelled)
        else {
            panic!("analysis must complete");
        };
        let base_generation = draft.base_generation();

        workspace
            .upsert_disk(base.clone(), Revision::new(2), Arc::clone(&base_text))
            .expect("base revision");
        workspace
            .upsert_disk(loaded.clone(), Revision::new(1), Arc::clone(&loaded_text))
            .expect("found disk");
        workspace
            .upsert_overlay(loaded.clone(), Revision::new(2), Arc::clone(&loaded_text))
            .expect("found overlay");
        workspace
            .upsert_disk(unrelated, Revision::new(1), "unrelated\n")
            .expect("unrelated generation");

        assert_ne!(base_generation, workspace.generation());
        assert!(!draft.matches_canonical_context(workspace.generation(), &canonical_options));
        let analysis = workspace
            .finalize_draft(draft)
            .expect("same observed allocations remain valid");
        assert_eq!(analysis.resource_revisions[&base], Revision::new(2));
        assert_eq!(analysis.resource_revisions[&loaded], Revision::new(2));
        assert_eq!(
            workspace.get(&loaded).unwrap().layer(),
            ResourceLayer::Overlay
        );

        let WorkspaceAnalysisStep::Complete(root_draft) = workspace.snapshot().analyze_resumable(
            &root,
            &canonical_options,
            ProjectionLimits::default(),
            &NeverCancelled,
        ) else {
            panic!("all resources are ready");
        };
        workspace
            .upsert_disk(root, Revision::new(2), Arc::clone(&root_text))
            .expect("root revision changes with shared text");
        assert_eq!(
            workspace
                .finalize_draft(root_draft)
                .expect_err("root revision remains an explicit gate")
                .code,
            WorkspaceErrorCode::StaleRevision
        );
    }
}
