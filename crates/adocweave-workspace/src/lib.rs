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
use std::sync::Arc;

use adocweave::output::diagnostics::Severity;
use adocweave::preprocess::{
    AnalysisProjection, DirectiveKind, EffectiveProcessingOptions, PreprocessOptions,
    PreprocessedAnalysisError, ProjectionFailure, ProjectionLimits, ResourceDocument,
    ResourceSnapshot, preprocess_and_analyze_cancellable_with_options,
};
use adocweave::{AnalysisOptions, SourceId};
use dependency_graph::DependencyGraph;

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
#[derive(Clone, Debug, Default)]
pub struct RetainedResourceBudget {
    resources: BTreeMap<ResourceId, RetainedLayerCharge>,
}

impl RetainedResourceBudget {
    /// Returns the layer charges for one resource identity.
    pub fn charge(&self, id: &ResourceId) -> RetainedLayerCharge {
        self.resources.get(id).copied().unwrap_or_default()
    }

    /// Returns whether this scope retains no resource layers.
    pub fn is_empty(&self) -> bool {
        self.resources.is_empty()
    }

    /// Returns a budget with both layers replaced atomically.
    pub fn with_layers(
        &self,
        id: ResourceId,
        charge: RetainedLayerCharge,
        limits: RetainedResourceLimits,
    ) -> Result<Self, WorkspaceError> {
        let mut replacement = self.clone();
        replacement.resources.insert(id.clone(), charge);
        replacement.validate_replacement(id, limits)?;
        Ok(replacement)
    }

    /// Returns a budget with both layers released.
    pub fn without_resource(&self, id: &ResourceId) -> Self {
        let mut replacement = self.clone();
        replacement.resources.remove(id);
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
        replacement
            .resources
            .entry(id.clone())
            .or_default()
            .disk_bytes = bytes;
        replacement.validate_replacement(id, limits)?;
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
        replacement
            .resources
            .entry(id.clone())
            .or_default()
            .overlay_bytes = bytes;
        replacement.validate_replacement(id, limits)?;
        Ok(replacement)
    }

    fn validate_replacement(
        &mut self,
        id: ResourceId,
        limits: RetainedResourceLimits,
    ) -> Result<(), WorkspaceError> {
        if self
            .resources
            .get(&id)
            .is_some_and(|charge| charge.disk_bytes.is_none() && charge.overlay_bytes.is_none())
        {
            self.resources.remove(&id);
        }
        if self.resources.len() > limits.max_files {
            return Err(WorkspaceError::new(
                WorkspaceErrorCode::ResourceLimit,
                "retained resource count limit exceeded",
            ));
        }
        let bytes = self.resources.values().try_fold(0_u64, |total, charge| {
            charge
                .disk_bytes
                .into_iter()
                .chain(charge.overlay_bytes)
                .try_fold(total, |total, bytes| {
                    if bytes > limits.max_resource_bytes {
                        return None;
                    }
                    total.checked_add(bytes)
                })
        });
        if bytes.is_none_or(|bytes| bytes > limits.max_total_bytes) {
            return Err(WorkspaceError::new(
                WorkspaceErrorCode::ResourceLimit,
                "retained resource byte limit exceeded",
            ));
        }
        Ok(())
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
    message: String,
}

impl WorkspaceError {
    fn new(code: WorkspaceErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            source_id: None,
            range: None,
            detail_code: None,
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
    roots: BTreeSet<ResourceId>,
    disk: BTreeMap<ResourceId, Resource>,
    overlays: BTreeMap<ResourceId, Resource>,
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
            roots: BTreeSet::new(),
            disk: BTreeMap::new(),
            overlays: BTreeMap::new(),
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
        if self.roots.insert(id) {
            self.generation = self.generation.next();
        }
        Ok(())
    }

    /// Removes an analysis root without removing its resource.
    pub fn unregister_root(&mut self, id: &ResourceId) {
        if self.roots.remove(id) {
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
        self.ensure_capacity(Some((&id, &resource)), None)?;
        self.disk.insert(id.clone(), resource);
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
        self.ensure_capacity(None, Some((&id, &resource)))?;
        self.overlays.insert(id.clone(), resource);
        self.refresh_effective(id)
    }

    /// Closes an overlay, restoring disk text when present.
    pub fn close_overlay(
        &mut self,
        id: &ResourceId,
    ) -> Result<BTreeSet<ResourceId>, WorkspaceError> {
        if self.overlays.remove(id).is_none() {
            return Ok(BTreeSet::new());
        }
        self.refresh_effective(id.clone())
    }

    /// Removes disk text and returns affected roots.
    ///
    /// An open overlay remains effective until it is closed.
    pub fn remove_disk(&mut self, id: &ResourceId) -> BTreeSet<ResourceId> {
        if self.disk.remove(id).is_none() || self.overlays.contains_key(id) {
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
            roots: self.roots.clone(),
            resources: Arc::clone(&self.effective),
        }
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
    ) -> Result<(), WorkspaceError> {
        let count = self
            .disk
            .keys()
            .chain(self.overlays.keys())
            .cloned()
            .collect::<BTreeSet<_>>()
            .len();
        let incoming_new = disk_replacement
            .or(overlay_replacement)
            .is_some_and(|(id, _)| !self.disk.contains_key(id) && !self.overlays.contains_key(id));
        if count + usize::from(incoming_new) > self.limits.resources.max_files {
            return Err(WorkspaceError::new(
                WorkspaceErrorCode::ResourceLimit,
                "file limit exceeded",
            ));
        }
        let replaced_disk = disk_replacement.map(|(id, _)| id);
        let replaced_overlay = overlay_replacement.map(|(id, _)| id);
        let retained = self
            .disk
            .iter()
            .filter(|(id, _)| Some(*id) != replaced_disk)
            .chain(
                self.overlays
                    .iter()
                    .filter(|(id, _)| Some(*id) != replaced_overlay),
            )
            .try_fold(0_u64, |total, (_, resource)| {
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
                Ok(total + resource.text.len() as u64)
            })?;
        if retained.saturating_add(incoming) > self.limits.resources.max_total_bytes {
            return Err(WorkspaceError::new(
                WorkspaceErrorCode::ResourceLimit,
                "total byte limit exceeded",
            ));
        }
        Ok(())
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
            self.roots.remove(&id);
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
        self.roots.remove(id);
        self.dependencies.remove(id);
        self.generation = self.generation.next();
        affected
    }
}

/// Immutable workspace state safe to move to a worker thread.
#[derive(Clone, Debug)]
pub struct WorkspaceSnapshot {
    generation: Generation,
    roots: BTreeSet<ResourceId>,
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
        let resources: BTreeMap<ResourceId, Resource> = self
            .resources
            .iter()
            .filter(|(id, resource)| retain(id, resource))
            .map(|(id, resource)| (id.clone(), resource.clone()))
            .collect();
        let roots = self
            .roots
            .iter()
            .filter(|root| resources.contains_key(*root))
            .cloned()
            .collect();
        Self {
            generation: self.generation,
            roots,
            resources: Arc::new(resources),
        }
    }

    /// Produces a filtered snapshot while allowing the predicate to reject
    /// before an accepted resource is cloned into the replacement map.
    pub fn try_filter_resources<E>(
        &self,
        mut retain: impl FnMut(&ResourceId, &Resource) -> Result<bool, E>,
    ) -> Result<Self, E> {
        let mut resources = BTreeMap::new();
        for (id, resource) in self.resources.iter() {
            if retain(id, resource)? {
                resources.insert(id.clone(), resource.clone());
            }
        }
        let roots = self
            .roots
            .iter()
            .filter(|root| resources.contains_key(*root))
            .cloned()
            .collect();
        Ok(Self {
            generation: self.generation,
            roots,
            resources: Arc::new(resources),
        })
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
        let preprocessed = preprocess_and_analyze_cancellable_with_options(
            &root_resource.text,
            &snapshot,
            &options,
            cancellation,
        )
        .map_err(|error| match error {
            PreprocessedAnalysisError::Options(error) => {
                WorkspaceError::new(WorkspaceErrorCode::InvalidOptions, error.to_string())
            }
            PreprocessedAnalysisError::Preprocess(error) => WorkspaceError::new(
                WorkspaceErrorCode::Preprocess,
                error.to_string(),
            )
            .with_origin(error.source_id.as_ref(), error.range, error.kind.as_str()),
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
    fn fallible_snapshot_filter_stops_before_later_resources_are_cloned() {
        let mut workspace = Workspace::default();
        for (revision, name) in ["a", "b", "c"].into_iter().enumerate() {
            workspace
                .upsert_disk(
                    ResourceId::new(name).expect("resource ID"),
                    Revision::new(revision as i64),
                    name,
                )
                .expect("resource");
        }
        let snapshot = workspace.snapshot();
        let mut visited = Vec::new();

        let result = snapshot.try_filter_resources(|id, _| {
            visited.push(id.to_string());
            if visited.len() == 2 {
                return Err("limit");
            }
            Ok(true)
        });

        assert!(matches!(result, Err("limit")));
        assert_eq!(visited, ["a", "b"]);
    }
}
