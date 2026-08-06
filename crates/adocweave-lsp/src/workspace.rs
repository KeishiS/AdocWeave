//! LSP URI and filesystem adapter for the runtime-independent workspace.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use adocweave::CancellationCheck;
#[cfg(test)]
use adocweave::NeverCancel;
use adocweave::preprocess::{
    EffectiveProcessingOptions, PreprocessOptions, ProjectionLimits, SafeMode,
};
use adocweave_host::{
    FilesystemDraftError, FilesystemJobCoordinator, FilesystemJobLimits, FilesystemReadLimits,
    FilesystemReadOutcome, FilesystemResourceBinding, LocalFilesystemDraft, LocalFilesystemPolicy,
    LocalFilesystemSession, LogicalSourceId,
};
use adocweave_workspace::{
    Generation, ResourceId, RetainedLayerCharge, RetainedResourceBudget, RetainedResourceLimits,
    Revision, Workspace, WorkspaceAnalysis, WorkspaceAnalysisDraft, WorkspaceAnalysisStep,
    WorkspaceError, WorkspaceLimits, WorkspaceSnapshot,
};
use async_lsp::lsp_types::Url;

const MAX_WATCHED_INCLUDE_RESOURCES: usize = 10_000;

pub(crate) const fn workspace_scan_job_limits() -> FilesystemJobLimits {
    let reads = FilesystemReadLimits::DEFAULT;
    FilesystemJobLimits {
        max_read_operations: reads.max_files as u64,
        max_read_bytes: reads.max_total_bytes,
        max_read_probe_bytes: 1,
        max_directory_operations: LocalFilesystemSession::MAX_SCAN_ENTRIES as u64
            + LocalFilesystemPolicy::MAX_ROOTS as u64,
        max_directory_entries: LocalFilesystemSession::MAX_SCAN_ENTRIES as u64,
        max_directory_probe_entries: 1,
        max_candidate_changes: reads.max_files as u64,
        max_sessions: reads.max_files + 2,
    }
}

/// Bounds the include reads of one document analysis.
///
/// Analysing one document only ever opens include targets by exact path, so the
/// job needs no directory allowance at all. The read allowance matches the
/// workspace scan because a document may legitimately include every file the
/// scan would have found.
pub(crate) const fn document_analysis_job_limits() -> FilesystemJobLimits {
    let reads = FilesystemReadLimits::DEFAULT;
    FilesystemJobLimits {
        max_read_operations: reads.max_files as u64,
        max_read_bytes: reads.max_total_bytes,
        max_read_probe_bytes: 1,
        max_directory_operations: 0,
        max_directory_entries: 0,
        max_directory_probe_entries: 0,
        max_candidate_changes: reads.max_files as u64,
        max_sessions: reads.max_files + 2,
    }
}

/// Bounds the reads of one watched-file update.
///
/// A watcher notification concerns exactly one file, so this allows one read and
/// no directory work at all.
pub(crate) const fn watched_file_job_limits() -> FilesystemJobLimits {
    let reads = FilesystemReadLimits::DEFAULT;
    FilesystemJobLimits {
        max_read_operations: 1,
        max_read_bytes: reads.max_resource_bytes,
        max_read_probe_bytes: 1,
        max_directory_operations: 0,
        max_directory_entries: 0,
        max_directory_probe_entries: 0,
        max_candidate_changes: 2,
        max_sessions: 1,
    }
}

const fn workspace_config_read_limits() -> FilesystemReadLimits {
    FilesystemReadLimits {
        max_files: FilesystemReadLimits::DEFAULT.max_files,
        max_total_bytes: FilesystemReadLimits::DEFAULT.max_total_bytes,
        max_resource_bytes: adocweave_config::MAX_PROJECT_FILE_BYTES,
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct AnalysisRootRoles {
    /// Membership in the workspace-discovered root set. The initial scan and
    /// later watcher discovery both maintain this role.
    scan_root: bool,
    open_overlay: bool,
}

impl AnalysisRootRoles {
    const fn is_root(self) -> bool {
        self.scan_root || self.open_overlay
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum WatchedFileKind {
    Upsert,
    Delete,
}

#[derive(Debug, Default)]
pub(crate) struct WatchedFileUpdate {
    pub(crate) affected: BTreeSet<String>,
    pub(crate) journal_relevant: bool,
}

#[derive(Debug)]
pub(crate) struct WatchedFileError {
    pub(crate) message: String,
    pub(crate) journal_relevant: bool,
}

#[derive(Clone, Debug)]
pub struct WorkspaceInput {
    pub generation: Generation,
    pub root: ResourceId,
    pub snapshot: WorkspaceSnapshot,
    pub options: PreprocessOptions,
    pub project_config: adocweave_config::ResolvedProjectConfig,
    pub config_sha256: Option<[u8; 32]>,
}

impl WorkspaceInput {
    #[cfg(test)]
    pub fn root_text(&self) -> Option<&Arc<str>> {
        self.snapshot
            .get(&self.root)
            .map(adocweave_workspace::Resource::text)
    }
}

use adocweave_config::ProjectScopeId;

#[derive(Debug)]
enum ScopeConfigError {
    Config(adocweave_config::ConfigError),
    Transient(String),
    Other(String),
}

impl ScopeConfigError {
    fn preserves_previous(&self) -> bool {
        matches!(
            self,
            Self::Config(error) if error.code == adocweave_config::ConfigErrorCode::ReadFailed
        ) || matches!(self, Self::Transient(_))
    }
}

impl std::fmt::Display for ScopeConfigError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Config(error) => error.fmt(formatter),
            Self::Transient(error) | Self::Other(error) => formatter.write_str(error),
        }
    }
}

/// A completed read of the workspace roots, not yet installed.
///
/// Produced by [`WorkspaceResources::load_roots_detached`] and consumed by
/// [`WorkspaceResources::apply_loaded_roots`].
#[derive(Clone, Debug)]
pub struct LoadedRoots {
    replacement: WorkspaceResources,
    error: Option<String>,
}

#[derive(Clone, Debug, Default)]
pub struct WorkspaceResources {
    inner: Workspace,
    analysis_root_roles: BTreeMap<ResourceId, AnalysisRootRoles>,
    roots: Vec<PathBuf>,
    directory_roots: Vec<PathBuf>,
    single_file_roots: BTreeSet<PathBuf>,
    scan_settings: BTreeMap<PathBuf, adocweave_config::WorkspaceScanSettings>,
    filesystem_policy: Option<LocalFilesystemPolicy>,
    filesystems: BTreeMap<ProjectScopeId, Arc<Mutex<LocalFilesystemSession>>>,
    project_plans: BTreeMap<ProjectScopeId, adocweave_config::ResolvedResourceLimitPlan>,
    resource_projects: BTreeMap<ResourceId, ProjectScopeId>,
    /// Include targets which must remain observable by the file watcher.
    ///
    /// Unlike `loaded_include_resources`, this includes admitted targets which are
    /// currently missing or could not be read. Keeping the interest separate
    /// from the disk layer lets a later create or repair notification recover
    /// the dependent open document.
    include_interests: BTreeSet<ResourceId>,
    loaded_include_resources: BTreeSet<ResourceId>,
    include_dependencies: BTreeMap<ResourceId, BTreeSet<ResourceId>>,
    retained_layers: BTreeMap<ProjectScopeId, RetainedResourceBudget>,
    /// Project files already discovered and parsed, keyed by the directory the
    /// search started from.
    ///
    /// Resolving a document's configuration walks up to the workspace root,
    /// then canonicalizes, reads, hashes and parses the project file. Without
    /// this the work repeats on every keystroke, on the thread that answers
    /// every other request. Discovery depends only on the directory and the
    /// roots, so the directory is a complete key while the roots hold still.
    config_cache: BTreeMap<PathBuf, Option<adocweave_config::ConfigSnapshot>>,
    /// The claim each disk resource holds on its project's filesystem session.
    ///
    /// Releasing a resource means giving up the exact claim its last read
    /// established, rather than naming a path. A claim carries a generation, so
    /// a stale watcher notification cannot release a resource that has since
    /// been read again.
    resource_bindings: BTreeMap<ResourceId, FilesystemResourceBinding>,
    next_disk_version: i64,
    last_load_failed_closed: bool,
}

/// One watched file read through a draft, held open until the update commits.
///
/// Dropping this without committing discards the read, which is what replaces
/// the explicit rollback the previous design needed.
struct PreparedWorkspaceRead {
    text: Arc<str>,
    binding: FilesystemResourceBinding,
    filesystem: Arc<Mutex<LocalFilesystemSession>>,
    draft: LocalFilesystemDraft,
}

struct WorkspaceFilesystemCandidate {
    session: Arc<Mutex<LocalFilesystemSession>>,
    draft: Option<LocalFilesystemDraft>,
}

enum AdmittedIncludeTarget {
    Existing(Box<ExistingIncludeTarget>),
    Missing,
}

struct ExistingIncludeTarget {
    uri: Url,
    path: PathBuf,
    scope: ProjectScopeId,
    plan: adocweave_config::ResolvedResourceLimitPlan,
}

/// A finished analysis together with the workspace state it needs to be adopted.
///
/// Analysis runs on a copy of the workspace, so every include it acquired lives
/// here rather than in the state the editor can see. Dropping this value leaves
/// no trace of the attempt.
pub struct AnalyzedRoot {
    candidate: WorkspaceResources,
    root: ResourceId,
    canonical_options: EffectiveProcessingOptions,
    outcome: AnalyzedRootOutcome,
    /// Every include target the run was allowed to look for, present or not.
    ///
    /// This is what the root depends on, so it is also what the file watcher
    /// must keep watching. A run that failed still contributes here: repairing
    /// a broken include has to produce a notification the document can act on.
    requested_includes: BTreeSet<ResourceId>,
}

enum AnalyzedRootOutcome {
    Complete(Box<WorkspaceAnalysisDraft>),
    Failed(WorkspaceError),
    /// A resource could not be read, so nothing this run produced may be kept.
    ReadFailed(String),
    Cancelled,
}

/// What to report about a run that produced no result.
///
/// This carries the pieces a diagnostic needs rather than the workspace error
/// itself, so the code that publishes diagnostics does not have to know how
/// analysis represents its failures.
pub struct AnalysisFailure {
    pub source_id: Option<String>,
    pub range: Option<adocweave::text::TextRange>,
    pub code: String,
    pub message: String,
}

impl AnalyzedRoot {
    /// Returns the failure when analysis did not produce a result.
    pub fn failure(&self) -> Option<AnalysisFailure> {
        match &self.outcome {
            AnalyzedRootOutcome::Failed(error) => Some(AnalysisFailure {
                source_id: error.source_id.as_ref().map(ToString::to_string),
                range: error.range,
                code: error.diagnostic_code().to_owned(),
                message: error.to_string(),
            }),
            AnalyzedRootOutcome::ReadFailed(message) => Some(AnalysisFailure {
                source_id: None,
                range: None,
                code: "workspace-input-error".to_owned(),
                message: message.clone(),
            }),
            AnalyzedRootOutcome::Complete(_) | AnalyzedRootOutcome::Cancelled => None,
        }
    }
}

/// Passes a borrowed cancellation where the workspace API asks for a sized one.
struct SharedCancellation<'a>(&'a dyn CancellationCheck);

impl CancellationCheck for SharedCancellation<'_> {
    fn is_cancelled(&self) -> bool {
        self.0.is_cancelled()
    }
}

/// How one include requested by a suspended analysis was answered.
enum AcquiredInclude {
    /// The resource was read and is now part of the candidate workspace.
    Found(Arc<str>),
    /// The resource is authoritatively absent, whether it was refused by the
    /// configured authority or simply does not exist on disk.
    ///
    /// Both answers are the same to the preprocessor: no text is available and
    /// the include cannot be executed. Keeping them apart here would only push a
    /// distinction into the analysis that it cannot act on.
    NotFound,
    /// The resource exists but could not be read, so the analysis cannot go on.
    ///
    /// This is reported to the preprocessor rather than raised here, so the
    /// resulting diagnostic points at the include directive that asked for it
    /// instead of at the document as a whole.
    Failed(String),
}

/// Reads the includes one suspended analysis asks for, into a workspace copy.
///
/// The whole point of this type is that nothing it reads becomes visible until
/// the analysis finishes and is adopted. It owns the copy, the filesystem drafts
/// it reads through, and the authority that decides which targets are allowed.
struct IncludeAcquisition<'a> {
    candidate: WorkspaceResources,
    drafts: BTreeMap<ProjectScopeId, WorkspaceFilesystemCandidate>,
    root: ResourceId,
    root_scope: ProjectScopeId,
    allowed_roots: Vec<PathBuf>,
    requested: BTreeSet<ResourceId>,
    /// The first read that failed, if any.
    ///
    /// A failed read leaves its draft unusable, so no part of this run may be
    /// committed once it is set.
    read_failure: Option<String>,
    job: &'a FilesystemJobCoordinator,
}

impl IncludeAcquisition<'_> {
    fn acquire(&mut self, target: &ResourceId) -> Result<AcquiredInclude, String> {
        let admitted =
            self.candidate
                .admit_include_target(&self.root_scope, &self.allowed_roots, target)?;
        let Some(admitted) = admitted else {
            return Ok(AcquiredInclude::NotFound);
        };
        self.record_interest(target)?;
        let AdmittedIncludeTarget::Existing(existing) = admitted else {
            return Ok(AcquiredInclude::NotFound);
        };
        let ExistingIncludeTarget {
            uri,
            path,
            scope,
            plan,
        } = *existing;
        // A resource the starting snapshot already holds never reaches this
        // point, so an identity already present in the copy means an earlier
        // include in this same run acquired it. Its text is reused rather than
        // read twice, which keeps repeated includes off the job's byte budget.
        let id = uri_id(&uri)?;
        if let Some(existing) = self.candidate.inner.get(&id) {
            return Ok(AcquiredInclude::Found(Arc::clone(existing.text())));
        }
        let read = self
            .draft_for(&scope, plan)
            .and_then(|draft| read_scan_candidate(draft, &path));
        let candidate = match read {
            Ok(Some(candidate)) => candidate,
            Ok(None) => return Ok(AcquiredInclude::NotFound),
            Err(message) => {
                self.read_failure.get_or_insert_with(|| message.clone());
                return Ok(AcquiredInclude::Failed(message));
            }
        };
        let text = Arc::clone(&candidate.text);
        self.candidate
            .admit_include_text(id, candidate, scope, plan)?;
        Ok(AcquiredInclude::Found(text))
    }

    fn record_interest(&mut self, target: &ResourceId) -> Result<(), String> {
        if !self.candidate.include_interests.contains(target)
            && self.candidate.include_interests.len() >= MAX_WATCHED_INCLUDE_RESOURCES
        {
            return Err(format!(
                "workspace include dependency limit exceeded: {MAX_WATCHED_INCLUDE_RESOURCES}"
            ));
        }
        self.candidate.include_interests.insert(target.clone());
        self.requested.insert(target.clone());
        Ok(())
    }

    fn draft_for(
        &mut self,
        scope: &ProjectScopeId,
        plan: adocweave_config::ResolvedResourceLimitPlan,
    ) -> Result<&mut LocalFilesystemDraft, String> {
        let candidate = match self.drafts.entry(scope.clone()) {
            std::collections::btree_map::Entry::Occupied(entry) => entry.into_mut(),
            std::collections::btree_map::Entry::Vacant(entry) => {
                let session = self.candidate.session_for(scope, plan)?;
                let draft = session
                    .lock()
                    .map_err(|_| "workspace resource session lock is poisoned".to_owned())?
                    .draft(self.job)
                    .map_err(|error| error.to_string())?;
                entry.insert(WorkspaceFilesystemCandidate {
                    session,
                    draft: Some(draft),
                })
            }
        };
        Ok(candidate.draft.as_mut().expect("draft is active"))
    }

    /// Commits every draft this run opened and returns the workspace copy.
    ///
    /// Commits happen only when the analysis produced a result. A failed or
    /// cancelled run drops its drafts instead, which leaves the live sessions
    /// exactly as they were.
    fn commit(mut self) -> Result<WorkspaceResources, String> {
        for candidate in self.drafts.values_mut() {
            let draft = candidate.draft.take().expect("draft is active");
            let mut session = candidate
                .session
                .lock()
                .map_err(|_| "workspace resource session lock is poisoned".to_owned())?;
            draft
                .prepare_commit(&mut session)
                .map_err(|error| error.to_string())?
                .commit()
                .map_err(|error| error.to_string())?;
        }
        for (scope, candidate) in &self.drafts {
            self.candidate
                .filesystems
                .insert(scope.clone(), Arc::clone(&candidate.session));
        }
        Ok(self.candidate)
    }

    fn root(&self) -> &ResourceId {
        &self.root
    }
}

impl PreparedWorkspaceRead {
    /// Installs the read into the live session.
    ///
    /// Everything that could reject the update has already been decided by the
    /// time this runs, so the only failures left are the session lock and the
    /// draft's own validation.
    fn commit(self) -> Result<Arc<Mutex<LocalFilesystemSession>>, String> {
        let mut session = self
            .filesystem
            .lock()
            .map_err(|_| "workspace resource session lock is poisoned".to_owned())?;
        self.draft
            .prepare_commit(&mut session)
            .map_err(|error| error.to_string())?
            .commit()
            .map_err(|error| error.to_string())?;
        drop(session);
        Ok(self.filesystem)
    }
}

impl WorkspaceResources {
    #[cfg(test)]
    pub fn load_roots(&mut self, roots: &[Url]) -> Result<(), String> {
        self.load_roots_with_limits(roots, adapter_managed_workspace_limits(), &NeverCancel)
    }

    #[cfg(test)]
    pub fn reload_roots_with_open_sources(
        &mut self,
        roots: &[Url],
        open_sources: &[(Url, i64, Arc<str>)],
    ) -> Result<(), String> {
        self.reload_roots_with_open_sources_after_load(roots, open_sources, || {})
    }

    /// Reads the roots into a detached copy of this state.
    ///
    /// Walking the roots and reading every `.adoc` below them takes time
    /// proportional to the size of the workspace. Separating it lets a caller
    /// run it away from the thread that answers requests. The result holds no
    /// borrow of this state, and applying it is a separate, cheap step.
    #[cfg(test)]
    pub fn load_roots_detached(&self, roots: &[Url]) -> LoadedRoots {
        self.load_roots_detached_with_cancellation(roots, &NeverCancel)
    }

    /// Reads roots into a detached copy and stops promptly when superseded.
    #[cfg(test)]
    pub fn load_roots_detached_with_cancellation(
        &self,
        roots: &[Url],
        cancellation: &dyn CancellationCheck,
    ) -> LoadedRoots {
        let job = match FilesystemJobCoordinator::new(workspace_scan_job_limits()) {
            Ok(job) => job,
            Err(error) => {
                return LoadedRoots {
                    replacement: self.clone(),
                    error: Some(error.to_string()),
                };
            }
        };
        self.load_roots_detached_with_job(roots, cancellation, &job)
    }

    pub(crate) fn load_roots_detached_with_job(
        &self,
        roots: &[Url],
        cancellation: &dyn CancellationCheck,
        job: &FilesystemJobCoordinator,
    ) -> LoadedRoots {
        let mut replacement = self.clone();
        let error = replacement
            .load_roots_with_limits_and_job(
                roots,
                adapter_managed_workspace_limits(),
                cancellation,
                job,
            )
            .err();
        LoadedRoots { replacement, error }
    }

    /// Installs a completed read and overlays the documents open right now.
    ///
    /// The open documents are read here rather than when the walk started, so
    /// a document opened while the walk was running is not lost.
    pub fn apply_loaded_roots(
        &mut self,
        loaded: LoadedRoots,
        open_sources: &[(Url, i64, Arc<str>)],
    ) -> Result<(), String> {
        let LoadedRoots { replacement, error } = loaded;
        if let Some(error) = error {
            if replacement.last_load_failed_closed {
                *self = replacement;
            } else {
                self.last_load_failed_closed = false;
            }
            return Err(error);
        }
        self.overlay_open_sources(replacement, open_sources)
    }

    #[cfg(test)]
    fn reload_roots_with_open_sources_after_load(
        &mut self,
        roots: &[Url],
        open_sources: &[(Url, i64, Arc<str>)],
        after_load: impl FnOnce(),
    ) -> Result<(), String> {
        let loaded = self.load_roots_detached(roots);
        if loaded.error.is_some() {
            return self.apply_loaded_roots(loaded, open_sources);
        }
        after_load();
        self.apply_loaded_roots(loaded, open_sources)
    }

    fn overlay_open_sources(
        &mut self,
        mut replacement: Self,
        open_sources: &[(Url, i64, Arc<str>)],
    ) -> Result<(), String> {
        for (uri, version, source) in open_sources {
            let scope_and_plan = match replacement.open_scope_and_plan(uri) {
                Ok(scope_and_plan) => scope_and_plan,
                Err(error) => {
                    let preserve_previous = error.preserves_previous();
                    let error = error.to_string();
                    if preserve_previous {
                        self.last_load_failed_closed = false;
                    } else {
                        replacement.fail_closed(
                            replacement.roots.clone(),
                            adapter_managed_workspace_limits(),
                        );
                        *self = replacement;
                    }
                    return Err(error);
                }
            };
            if let Some((scope, plan)) = scope_and_plan
                && let Err(error) = replacement.upsert_open_with_plan(
                    uri.clone(),
                    *version,
                    Arc::clone(source),
                    scope,
                    plan,
                )
            {
                replacement.fail_closed(
                    replacement.roots.clone(),
                    adapter_managed_workspace_limits(),
                );
                *self = replacement;
                return Err(error);
            }
        }
        *self = replacement;
        Ok(())
    }

    #[cfg(test)]
    fn load_roots_with_limits(
        &mut self,
        roots: &[Url],
        limits: WorkspaceLimits,
        cancellation: &dyn CancellationCheck,
    ) -> Result<(), String> {
        let job = FilesystemJobCoordinator::new(workspace_scan_job_limits())
            .map_err(|error| error.to_string())?;
        self.load_roots_with_limits_after_hooks_and_job(
            roots,
            limits,
            &SharedCancellation(cancellation),
            &job,
            (|| {}, || {}, || {}),
        )
    }

    fn load_roots_with_limits_and_job(
        &mut self,
        roots: &[Url],
        limits: WorkspaceLimits,
        cancellation: &dyn CancellationCheck,
        job: &FilesystemJobCoordinator,
    ) -> Result<(), String> {
        self.load_roots_with_limits_after_hooks_and_job(
            roots,
            limits,
            &SharedCancellation(cancellation),
            job,
            (|| {}, || {}, || {}),
        )
    }

    #[cfg(test)]
    fn load_roots_with_limits_after_authority(
        &mut self,
        roots: &[Url],
        limits: WorkspaceLimits,
        cancellation: &dyn CancellationCheck,
        after_authority: impl FnOnce(),
    ) -> Result<(), String> {
        self.load_roots_with_limits_after_hooks(roots, limits, cancellation, || {}, after_authority)
    }

    #[cfg(test)]
    fn load_roots_with_limits_after_hooks(
        &mut self,
        roots: &[Url],
        limits: WorkspaceLimits,
        cancellation: &dyn CancellationCheck,
        after_root_classification: impl FnOnce(),
        after_authority: impl FnOnce(),
    ) -> Result<(), String> {
        let job = FilesystemJobCoordinator::new(workspace_scan_job_limits())
            .map_err(|error| error.to_string())?;
        self.load_roots_with_limits_after_hooks_and_job(
            roots,
            limits,
            &SharedCancellation(cancellation),
            &job,
            (after_root_classification, after_authority, || {}),
        )
    }

    fn load_roots_with_limits_after_hooks_and_job(
        &mut self,
        roots: &[Url],
        limits: WorkspaceLimits,
        cancellation: &dyn CancellationCheck,
        job: &FilesystemJobCoordinator,
        hooks: (impl FnOnce(), impl FnOnce(), impl FnOnce()),
    ) -> Result<(), String> {
        let (after_root_classification, after_authority, before_filesystem_commit) = hooks;
        self.last_load_failed_closed = false;
        // A reload is the only way the roots or a project file can change, so it
        // is also the only point at which a remembered configuration can go
        // stale.
        self.forget_configs();
        let seed = Generation::new(self.inner.generation().get().saturating_add(1));
        let root_paths = match roots
            .iter()
            .map(|root| {
                root.to_file_path()
                    .map_err(|()| format!("workspace root is not a file URI: {root}"))?
                    .canonicalize()
                    .map_err(|error| format!("cannot canonicalize workspace root: {error}"))
            })
            .collect::<Result<Vec<_>, _>>()
        {
            Ok(paths) => paths,
            Err(error) => {
                let _ = job.finish();
                self.fail_closed(Vec::new(), limits);
                return Err(error);
            }
        };
        let mut directory_roots = Vec::new();
        let mut single_file_roots = BTreeSet::new();
        for path in root_paths {
            if path.is_dir() {
                directory_roots.push(path);
            } else if path.is_file() {
                single_file_roots.insert(path);
            } else {
                let _ = job.finish();
                self.fail_closed(Vec::new(), limits);
                return Err("workspace root is neither a directory nor a regular file".to_owned());
            }
        }
        directory_roots.sort();
        directory_roots.dedup();
        single_file_roots.retain(|path| {
            !directory_roots
                .iter()
                .any(|directory| path.starts_with(directory))
        });
        let mut paths = directory_roots.clone();
        paths.extend(
            single_file_roots
                .iter()
                .filter_map(|path| path.parent().map(Path::to_owned)),
        );
        paths.sort();
        paths.dedup();
        after_root_classification();
        let preserve_previous = std::cell::Cell::new(false);
        let load_result = (|| {
            let authority = (!paths.is_empty())
                .then(|| {
                    LocalFilesystemPolicy::new(
                        paths.clone(),
                        adocweave_host::FilesystemReadLimits::default(),
                    )
                })
                .transpose()
                .map_err(|error| error.to_string())?;
            if let Some(authority) = &authority
                && let Some(changed) = paths
                    .iter()
                    .find(|root| authority.root_policy(root).is_none())
            {
                return Err(format!(
                    "workspace root changed while its filesystem authority was established: {}",
                    changed.display()
                ));
            }
            after_authority();
            let config_session = authority
                .as_ref()
                .filter(|_| !paths.is_empty())
                .map(|policy| {
                    policy
                        .access_existing(paths.clone(), workspace_config_read_limits())?
                        .session()
                })
                .transpose()
                .map_err(|error| error.to_string())?;
            let mut config_draft = config_session
                .as_ref()
                .map(|session| session.draft(job))
                .transpose()
                .map_err(|error| error.to_string())?;
            let mut config_by_directory = BTreeMap::new();
            let mut config_by_path = BTreeMap::new();
            let mut scan_settings = BTreeMap::new();
            for root in &directory_roots {
                let snapshot = scan_config_for_path(
                    &paths,
                    authority.as_ref(),
                    config_draft.as_mut(),
                    root,
                    root.clone(),
                    &mut config_by_directory,
                    &mut config_by_path,
                )
                .map_err(|error| {
                    preserve_previous.set(error.preserves_previous());
                    error.to_string()
                })?;
                scan_settings.insert(
                    root.clone(),
                    snapshot.map_or_else(
                        adocweave_config::WorkspaceScanSettings::default,
                        |snapshot| snapshot.config.workspace.scan,
                    ),
                );
            }
            let discovery = authority
                .as_ref()
                .filter(|_| !directory_roots.is_empty())
                .map(|policy| {
                    policy
                        .access_existing(
                            directory_roots.clone(),
                            adocweave_host::FilesystemReadLimits::default(),
                        )?
                        .session()
                })
                .transpose()
                .map_err(|error| error.to_string())?;
            let mut candidates = match discovery {
                Some(session) => {
                    let draft = session.draft(job).map_err(|error| error.to_string())?;
                    draft
                        .discover_adoc_paths_with_control(
                            |root, relative| {
                                let directory = root.join(relative);
                                let is_nested_workspace_root = directory != root
                                    && directory_roots.binary_search(&directory).is_ok();
                                is_nested_workspace_root
                                    || scan_settings
                                        .get(root)
                                        .is_some_and(|settings| settings.excludes(relative))
                            },
                            || cancellation.is_cancelled(),
                        )
                        .map_err(|error| error.to_string())?
                }
                None => Vec::new(),
            };
            candidates.extend(single_file_roots.iter().cloned());
            candidates.sort();
            candidates.dedup();
            let mut inner = Workspace::new_at_generation(limits, seed);
            let mut filesystem_candidates = BTreeMap::new();
            let mut resource_projects = BTreeMap::new();
            let mut resource_bindings = BTreeMap::new();
            let mut analysis_root_roles = BTreeMap::new();
            let mut project_plans = BTreeMap::new();
            let mut retained_layers: BTreeMap<ProjectScopeId, RetainedResourceBudget> =
                BTreeMap::new();
            let mut next_disk_version = self.next_disk_version;
            for path in candidates {
                if cancellation.is_cancelled() {
                    return Err("workspace scan was cancelled".to_owned());
                }
                let config = match scan_config_for_path(
                    &paths,
                    authority.as_ref(),
                    config_draft.as_mut(),
                    &path,
                    path.parent().unwrap_or(&path).to_owned(),
                    &mut config_by_directory,
                    &mut config_by_path,
                ) {
                    Ok(config) => config,
                    Err(error) => {
                        preserve_previous.set(error.preserves_previous());
                        return Err(error.to_string());
                    }
                };
                let workspace_root = paths
                    .iter()
                    .filter(|root| path.starts_with(root))
                    .max_by_key(|root| root.components().count())
                    .cloned()
                    .expect("discovered resource belongs to a canonical workspace root");
                let scope = ProjectScopeId {
                    workspace_root,
                    config_path: config.as_ref().map(|snapshot| snapshot.path.clone()),
                };
                if !resource_path_is_allowed(config.as_ref(), &path) {
                    continue;
                }
                let plan = config.as_ref().map_or_else(
                    adocweave_config::ResolvedResourceLimitPlan::default,
                    |snapshot| snapshot.config.resources.limit_plan,
                );
                if let Some(previous) = project_plans.insert(scope.clone(), plan)
                    && previous != plan
                {
                    return Err(
                        "project resource limit plan changed during workspace scan".to_owned()
                    );
                }
                let filesystem = match filesystem_candidates.entry(scope.clone()) {
                    std::collections::btree_map::Entry::Occupied(entry) => entry.into_mut(),
                    std::collections::btree_map::Entry::Vacant(entry) => {
                        let session = authority
                            .as_ref()
                            .expect("a discovered candidate has filesystem authority")
                            .access_existing([scope.workspace_root.clone()], plan.filesystem_reads)
                            .and_then(|access| access.session())
                            .map_err(|error| error.to_string())?;
                        let draft = session.draft(job).map_err(|error| error.to_string())?;
                        entry.insert(WorkspaceFilesystemCandidate {
                            session: Arc::new(Mutex::new(session)),
                            draft: Some(draft),
                        })
                    }
                };
                let Some(read) = read_scan_candidate(
                    filesystem.draft.as_mut().expect("draft is active"),
                    &path,
                )?
                else {
                    continue;
                };
                next_disk_version = next_disk_version.saturating_add(1);
                let id =
                    ResourceId::new(read.source_id.as_str()).map_err(|error| error.to_string())?;
                retained_layers
                    .entry(scope.clone())
                    .or_default()
                    .try_replace_layers(
                        id.clone(),
                        RetainedLayerCharge::new(Some(read.text.len() as u64), None),
                        plan.retained_layers,
                    )
                    .map_err(|error| error.to_string())?;
                inner
                    .upsert_disk(id.clone(), Revision::new(next_disk_version), read.text)
                    .map_err(|error| error.to_string())?;
                resource_bindings.insert(id.clone(), read.binding);
                if path_is_analysis_root(&path, &directory_roots, &single_file_roots) {
                    inner
                        .register_root(id.clone())
                        .map_err(|error| error.to_string())?;
                    analysis_root_roles.insert(
                        id.clone(),
                        AnalysisRootRoles {
                            scan_root: true,
                            open_overlay: false,
                        },
                    );
                }
                resource_projects.insert(id, scope);
            }
            before_filesystem_commit();
            if cancellation.is_cancelled() {
                return Err("workspace scan was cancelled".to_owned());
            }
            drop(config_draft);
            for candidate in filesystem_candidates.values_mut() {
                if cancellation.is_cancelled() {
                    return Err("workspace scan was cancelled".to_owned());
                }
                let draft = candidate.draft.take().expect("draft is active");
                let mut session = candidate
                    .session
                    .lock()
                    .map_err(|_| "workspace resource session lock is poisoned".to_owned())?;
                draft
                    .prepare_commit(&mut session)
                    .and_then(adocweave_host::PreparedFilesystemCommit::commit)
                    .map_err(|error| error.to_string())?;
            }
            if cancellation.is_cancelled() {
                return Err("workspace scan was cancelled".to_owned());
            }
            job.finish().map_err(|error| error.to_string())?;
            let filesystems = filesystem_candidates
                .into_iter()
                .map(|(scope, candidate)| (scope, candidate.session))
                .collect();
            self.inner = inner;
            self.analysis_root_roles = analysis_root_roles;
            self.roots = paths.clone();
            self.directory_roots = directory_roots;
            self.single_file_roots = single_file_roots;
            self.scan_settings = scan_settings;
            self.filesystem_policy = authority;
            self.filesystems = filesystems;
            self.project_plans = project_plans;
            self.resource_projects = resource_projects;
            self.resource_bindings = resource_bindings;
            self.include_interests.clear();
            self.loaded_include_resources.clear();
            self.include_dependencies.clear();
            self.retained_layers = retained_layers;
            self.next_disk_version = next_disk_version;
            Ok(())
        })();
        if let Err(error) = load_result {
            if cancellation.is_cancelled() {
                let _ = job.cancel();
            } else {
                let _ = job.finish();
            }
            if !preserve_previous.get() {
                self.fail_closed(paths, limits);
            }
            return Err(error);
        }
        Ok(())
    }

    fn fail_closed(&mut self, roots: Vec<PathBuf>, limits: WorkspaceLimits) {
        let seed = Generation::new(self.inner.generation().get().saturating_add(1));
        self.inner = Workspace::new_at_generation(limits, seed);
        self.analysis_root_roles.clear();
        self.roots = roots;
        self.directory_roots.clear();
        self.single_file_roots.clear();
        self.scan_settings.clear();
        self.filesystem_policy = None;
        self.filesystems.clear();
        self.project_plans.clear();
        self.resource_projects.clear();
        self.include_interests.clear();
        self.loaded_include_resources.clear();
        self.include_dependencies.clear();
        self.retained_layers.clear();
        self.last_load_failed_closed = true;
    }

    pub(crate) const fn last_load_failed_closed(&self) -> bool {
        self.last_load_failed_closed
    }

    /// Returns the effective text held for one resource, if it is known.
    #[cfg(test)]
    pub(crate) fn resource_text(&self, uri: &Url) -> Option<Arc<str>> {
        let id = uri_id(uri).ok()?;
        self.inner
            .get(&id)
            .map(|resource| Arc::clone(resource.text()))
    }

    /// Returns how many resources the workspace holds.
    #[cfg(test)]
    pub(crate) fn resource_count(&self) -> usize {
        self.inner.snapshot().resources().count()
    }

    #[cfg(test)]
    pub fn reload_file(&mut self, uri: Url) -> Result<BTreeSet<String>, String> {
        self.apply_watched_file(uri, WatchedFileKind::Upsert)
            .map(|update| update.affected)
            .map_err(|error| error.message)
    }

    pub(crate) fn apply_watched_file(
        &mut self,
        uri: Url,
        kind: WatchedFileKind,
    ) -> Result<WatchedFileUpdate, WatchedFileError> {
        let path = uri.to_file_path().map_err(|()| WatchedFileError {
            message: format!("workspace resource is not a file URI: {uri}"),
            journal_relevant: false,
        })?;
        let id = uri_id(&uri).map_err(|message| WatchedFileError {
            message,
            journal_relevant: false,
        })?;
        let roles = self
            .analysis_root_roles
            .get(&id)
            .copied()
            .unwrap_or_default();
        let known_include = self.include_interests.contains(&id);
        let tracked = roles.is_root() || known_include;
        let is_adoc = path.extension().and_then(|value| value.to_str()) == Some("adoc");
        if kind == WatchedFileKind::Delete {
            if !tracked && self.inner.get(&id).is_none() {
                return Ok(WatchedFileUpdate::default());
            }
            let affected = self.remove_disk(&uri).map_err(|message| WatchedFileError {
                message,
                journal_relevant: true,
            })?;
            self.loaded_include_resources.remove(&id);
            if let Some(roles) = self.analysis_root_roles.get_mut(&id) {
                roles.scan_root = false;
                if !roles.is_root() {
                    self.analysis_root_roles.remove(&id);
                    self.inner.unregister_root(&id);
                }
            }
            return Ok(WatchedFileUpdate {
                affected,
                journal_relevant: true,
            });
        }
        if !tracked && !is_adoc {
            return Ok(WatchedFileUpdate::default());
        }
        if !self.path_is_analysis_root(&path) {
            return Ok(WatchedFileUpdate::default());
        }
        let journal_relevant = tracked || is_adoc;
        let logical_path =
            workspace_logical_path(&self.roots, self.filesystem_policy.as_ref(), &path).map_err(
                |message| WatchedFileError {
                    message,
                    journal_relevant,
                },
            )?;
        // Scan exclusions are discovery rules, not filesystem authority. For
        // an unknown candidate they can be decided from the normalized URI
        // path before any file or nested project configuration is read.
        if !tracked && self.path_is_scan_excluded(&logical_path) {
            return Ok(WatchedFileUpdate::default());
        }
        let admitted_path =
            workspace_logical_file(&self.roots, self.filesystem_policy.as_ref(), &path).map_err(
                |message| WatchedFileError {
                    message,
                    journal_relevant,
                },
            )?;
        if !self.path_is_analysis_root(&admitted_path) {
            return Ok(WatchedFileUpdate::default());
        }
        let (scope, config) = scope_and_config_for_path_typed(
            &self.roots,
            self.filesystem_policy.as_ref(),
            &admitted_path,
        )
        .map_err(|error| WatchedFileError {
            message: error.to_string(),
            journal_relevant,
        })?;
        let discover_as_root =
            is_adoc && !roles.scan_root && !self.path_is_scan_excluded(&admitted_path);
        if !tracked && !discover_as_root {
            return Ok(WatchedFileUpdate::default());
        }
        if !resource_path_is_allowed(config.as_ref(), &admitted_path) {
            let affected =
                self.remove_outside_authority(&id)
                    .map_err(|message| WatchedFileError {
                        message,
                        journal_relevant,
                    })?;
            return Ok(WatchedFileUpdate {
                affected,
                journal_relevant,
            });
        }
        let plan = config.as_ref().map_or_else(
            adocweave_config::ResolvedResourceLimitPlan::default,
            |snapshot| snapshot.config.resources.limit_plan,
        );
        let prepared = if known_include || roles.open_overlay {
            self.read_workspace_resource(&admitted_path, &scope, plan)
        } else {
            self.read_analysis_root(&admitted_path, &scope, plan)
        }
        .map_err(|message| WatchedFileError {
            message,
            journal_relevant,
        })?;
        let next_disk_version = self.next_disk_version.saturating_add(1);
        let result = (|| {
            let previous_charge = self.retained_charge(&id);
            let charge = RetainedLayerCharge::new(
                Some(prepared.text.len() as u64),
                previous_charge.overlay_bytes(),
            );
            let retained_layers =
                self.move_retained_charge(&id, &scope, charge, plan.retained_layers)?;
            let mut inner = self.inner.clone();
            let affected = inner
                .upsert_disk(
                    id.clone(),
                    Revision::new(next_disk_version),
                    Arc::clone(&prepared.text),
                )
                .map_err(|error| error.to_string())?;
            if discover_as_root {
                if !roles.is_root() {
                    inner
                        .register_root(id.clone())
                        .map_err(|error| error.to_string())?;
                }
            } else if roles.is_root() && !inner.roots().contains(&id) {
                inner
                    .register_root(id.clone())
                    .map_err(|error| error.to_string())?;
            }
            Ok((retained_layers, inner, affected))
        })();
        // Every rejection below leaves through `prepared` unread, which drops
        // its draft and with it the read and the claim the read took.
        let (retained_layers, inner, affected) = match result {
            Ok(committed) => committed,
            Err(message) => {
                return Err(WatchedFileError {
                    message,
                    journal_relevant,
                });
            }
        };
        let previous_scope = self.resource_projects.get(&id).cloned();
        if previous_scope
            .as_ref()
            .is_some_and(|previous| previous != &scope)
            && let Err(message) = self.release_resource_binding(&id)
        {
            return Err(WatchedFileError {
                message,
                journal_relevant,
            });
        }
        let pending_dependents = self.include_dependents(&id);
        let binding = prepared.binding.clone();
        let filesystem = prepared.commit().map_err(|message| WatchedFileError {
            message,
            journal_relevant,
        })?;
        self.inner = inner;
        if discover_as_root {
            self.analysis_root_roles
                .entry(id.clone())
                .or_default()
                .scan_root = true;
        }
        self.retained_layers = retained_layers;
        self.filesystems.insert(scope.clone(), filesystem);
        self.project_plans.insert(scope.clone(), plan);
        self.resource_projects.insert(id.clone(), scope);
        self.resource_bindings.insert(id.clone(), binding);
        if known_include {
            self.loaded_include_resources.insert(id);
        }
        self.next_disk_version = next_disk_version;
        self.gc_scopes();
        let mut affected = strings(affected);
        affected.extend(pending_dependents);
        Ok(WatchedFileUpdate {
            affected,
            journal_relevant: true,
        })
    }

    fn remove_outside_authority(&mut self, id: &ResourceId) -> Result<BTreeSet<String>, String> {
        let Some(scope) = self.resource_projects.get(id).cloned() else {
            return Ok(BTreeSet::new());
        };
        let mut inner = self.inner.clone();
        inner.unregister_root(id);
        let mut affected = inner.close_overlay(id).map_err(|error| error.to_string())?;
        affected.extend(inner.remove_disk(id));
        let mut retained_layers = self.retained_layers.clone();
        let budget = retained_layers
            .get(&scope)
            .cloned()
            .unwrap_or_default()
            .without_resource(id);
        retained_layers.insert(scope.clone(), budget);
        self.release_resource_binding(id)?;
        self.inner = inner;
        self.analysis_root_roles.remove(id);
        self.retained_layers = retained_layers;
        self.resource_projects.remove(id);
        self.include_interests.remove(id);
        self.loaded_include_resources.remove(id);
        self.include_dependencies.remove(id);
        for dependencies in self.include_dependencies.values_mut() {
            dependencies.remove(id);
        }
        let pruned = self.prune_unreferenced_include_resources();
        self.gc_scopes();
        let mut affected = strings(affected);
        affected.extend(pruned);
        affected.insert(id.to_string());
        Ok(affected)
    }

    fn read_analysis_root(
        &self,
        path: &Path,
        scope: &ProjectScopeId,
        plan: adocweave_config::ResolvedResourceLimitPlan,
    ) -> Result<PreparedWorkspaceRead, String> {
        if path.extension().and_then(|value| value.to_str()) != Some("adoc") {
            return Err(format!(
                "workspace resource is not an .adoc file: {}",
                path.display()
            ));
        }
        self.read_workspace_resource(path, scope, plan)
    }

    /// Returns the filesystem session that reads for one project scope.
    fn session_for(
        &self,
        scope: &ProjectScopeId,
        plan: adocweave_config::ResolvedResourceLimitPlan,
    ) -> Result<Arc<Mutex<LocalFilesystemSession>>, String> {
        if let Some(previous) = self.project_plans.get(scope)
            && previous != &plan
        {
            return Err(
                "workspace resource limit plan changed; a full reload is required".to_owned(),
            );
        }
        if let Some(filesystem) = self.filesystems.get(scope) {
            return Ok(Arc::clone(filesystem));
        }
        let session = self
            .filesystem_policy
            .as_ref()
            .ok_or_else(|| "workspace has no retained filesystem authority".to_owned())?
            .access_existing([scope.workspace_root.clone()], plan.filesystem_reads)
            .and_then(|access| access.session())
            .map_err(|error| error.to_string())?;
        Ok(Arc::new(Mutex::new(session)))
    }

    /// Adds an already read include to this workspace copy.
    ///
    /// The caller passes the exact `Arc<str>` it handed to the preprocessor.
    /// Publication compares resources by shared-text identity, so a copy of the
    /// same bytes would be rejected as a different resource.
    fn admit_include_text(
        &mut self,
        id: ResourceId,
        read: ReadCandidate,
        scope: ProjectScopeId,
        plan: adocweave_config::ResolvedResourceLimitPlan,
    ) -> Result<(), String> {
        let charge = RetainedLayerCharge::new(Some(read.text.len() as u64), None);
        let retained_layers =
            self.move_retained_charge(&id, &scope, charge, plan.retained_layers)?;
        let next_disk_version = self.next_disk_version.saturating_add(1);
        self.inner
            .upsert_disk(id.clone(), Revision::new(next_disk_version), read.text)
            .map_err(|error| error.to_string())?;
        self.retained_layers = retained_layers;
        self.project_plans.insert(scope.clone(), plan);
        self.resource_projects.insert(id.clone(), scope);
        self.resource_bindings.insert(id.clone(), read.binding);
        self.loaded_include_resources.insert(id);
        self.next_disk_version = next_disk_version;
        Ok(())
    }

    /// Reads one watched file into a draft, leaving live state untouched.
    ///
    /// The draft stays open in the returned value. Committing it installs the
    /// read; dropping it discards the read together with the claim it took.
    fn read_workspace_resource(
        &self,
        path: &Path,
        scope: &ProjectScopeId,
        plan: adocweave_config::ResolvedResourceLimitPlan,
    ) -> Result<PreparedWorkspaceRead, String> {
        let filesystem = self.session_for(scope, plan)?;
        let job = FilesystemJobCoordinator::new(watched_file_job_limits())
            .map_err(|error| error.to_string())?;
        let mut draft = filesystem
            .lock()
            .map_err(|_| "workspace resource session lock is poisoned".to_owned())?
            .draft(&job)
            .map_err(|error| error.to_string())?;
        let loaded = draft
            .reread_utf8(
                LogicalSourceId::new(path.to_string_lossy().into_owned())
                    .map_err(|error| error.to_string())?,
                path,
            )
            .map_err(|error| error.to_string())?;
        let (_, text, binding) = loaded.into_parts_with_binding();
        Ok(PreparedWorkspaceRead {
            text,
            binding,
            filesystem,
            draft,
        })
    }

    fn retained_charge(&self, id: &ResourceId) -> RetainedLayerCharge {
        self.resource_projects
            .get(id)
            .and_then(|scope| self.retained_layers.get(scope))
            .map_or_else(RetainedLayerCharge::default, |budget| budget.charge(id))
    }

    fn move_retained_charge(
        &self,
        id: &ResourceId,
        scope: &ProjectScopeId,
        charge: RetainedLayerCharge,
        limits: RetainedResourceLimits,
    ) -> Result<BTreeMap<ProjectScopeId, RetainedResourceBudget>, String> {
        let mut retained_layers = self.retained_layers.clone();
        if let Some(previous_scope) = self.resource_projects.get(id)
            && previous_scope != scope
        {
            let previous = retained_layers
                .get(previous_scope)
                .cloned()
                .unwrap_or_default()
                .without_resource(id);
            retained_layers.insert(previous_scope.clone(), previous);
        }
        let replacement = retained_layers
            .get(scope)
            .cloned()
            .unwrap_or_default()
            .with_layers(id.clone(), charge, limits)
            .map_err(|error| error.to_string())?;
        retained_layers.insert(scope.clone(), replacement);
        Ok(retained_layers)
    }

    /// Gives up the claim one resource holds on its project's session.
    ///
    /// Releasing names the claim rather than the path, so a claim taken before
    /// a newer read cannot release what that newer read established. The session
    /// reports such a claim as stale and keeps the resource, which is exactly
    /// what a late watcher notification must not be able to undo.
    fn release_resource_binding(&mut self, id: &ResourceId) -> Result<(), String> {
        let Some(binding) = self.resource_bindings.remove(id) else {
            return Ok(());
        };
        let Some(scope) = self.resource_projects.get(id) else {
            return Ok(());
        };
        let Some(filesystem) = self.filesystems.get(scope).map(Arc::clone) else {
            return Ok(());
        };
        let job = FilesystemJobCoordinator::new(watched_file_job_limits())
            .map_err(|error| error.to_string())?;
        let mut session = filesystem
            .lock()
            .map_err(|_| "workspace resource session lock is poisoned".to_owned())?;
        let mut draft = session.draft(&job).map_err(|error| error.to_string())?;
        draft
            .release_binding(&binding)
            .map_err(|error| error.to_string())?;
        draft
            .prepare_commit(&mut session)
            .map_err(|error| error.to_string())?
            .commit()
            .map_err(|error| error.to_string())?;
        Ok(())
    }

    fn gc_scopes(&mut self) {
        let retained = self
            .resource_projects
            .values()
            .cloned()
            .collect::<BTreeSet<_>>();
        self.retained_layers
            .retain(|scope, budget| retained.contains(scope) || !budget.is_empty());
        self.project_plans
            .retain(|scope, _| retained.contains(scope));
        self.filesystems.retain(|scope, _| retained.contains(scope));
    }

    pub fn get(&self, uri: &Url) -> Option<&adocweave_workspace::Resource> {
        let id = uri_id(uri).ok()?;
        self.inner.get(&id)
    }

    pub fn upsert_open(
        &mut self,
        uri: Url,
        version: i64,
        text: impl Into<Arc<str>>,
    ) -> Result<BTreeSet<String>, String> {
        let Some((scope, plan)) = self
            .open_scope_and_plan(&uri)
            .map_err(|error| error.to_string())?
        else {
            return Err(format!(
                "workspace resource is outside configured resource roots: {uri}"
            ));
        };
        self.upsert_open_with_plan(uri, version, text.into(), scope, plan)
    }

    fn upsert_open_with_plan(
        &mut self,
        uri: Url,
        version: i64,
        text: Arc<str>,
        scope: ProjectScopeId,
        plan: adocweave_config::ResolvedResourceLimitPlan,
    ) -> Result<BTreeSet<String>, String> {
        let id = uri_id(&uri)?;
        let path = uri
            .to_file_path()
            .map_err(|()| format!("workspace resource is not a file URI: {uri}"))?;
        if self
            .project_plans
            .get(&scope)
            .is_some_and(|previous| previous != &plan)
        {
            return Err(
                "workspace resource limit plan changed; a full reload is required".to_owned(),
            );
        }
        let previous_scope = self.resource_projects.get(&id).cloned();
        let previous_charge = self.retained_charge(&id);
        let migrating_disk = previous_scope
            .as_ref()
            .is_some_and(|previous| previous != &scope)
            && previous_charge.disk_bytes().is_some();
        let prepared_disk = migrating_disk
            .then(|| self.read_analysis_root(&path, &scope, plan))
            .transpose()?;
        let next_disk_version = self
            .next_disk_version
            .saturating_add(i64::from(migrating_disk));
        let result = (|| {
            let charge = RetainedLayerCharge::new(
                prepared_disk
                    .as_ref()
                    .map_or(previous_charge.disk_bytes(), |prepared| {
                        Some(prepared.text.len() as u64)
                    }),
                Some(text.len() as u64),
            );
            let retained_layers =
                self.move_retained_charge(&id, &scope, charge, plan.retained_layers)?;
            let mut inner = self.inner.clone();
            if let Some(prepared) = &prepared_disk {
                inner
                    .upsert_disk(
                        id.clone(),
                        Revision::new(next_disk_version),
                        Arc::clone(&prepared.text),
                    )
                    .map_err(|error| error.to_string())?;
            }
            let affected = inner
                .upsert_overlay(id.clone(), Revision::new(version), Arc::clone(&text))
                .map_err(|error| error.to_string())?;
            let was_root = self
                .analysis_root_roles
                .get(&id)
                .copied()
                .is_some_and(AnalysisRootRoles::is_root);
            if !was_root {
                inner
                    .register_root(id.clone())
                    .map_err(|error| error.to_string())?;
            }
            Ok::<_, String>((retained_layers, inner, affected))
        })();
        // A rejection leaves through `prepared_disk` unread, which drops its
        // draft and with it the read and the claim the read took.
        let (retained_layers, inner, affected) = result?;
        if previous_scope
            .as_ref()
            .is_some_and(|previous| previous != &scope)
        {
            self.release_resource_binding(&id)?;
        }
        let committed_disk = prepared_disk
            .map(|prepared| {
                let binding = prepared.binding.clone();
                prepared.commit().map(|filesystem| (filesystem, binding))
            })
            .transpose()?;
        self.inner = inner;
        self.analysis_root_roles
            .entry(id.clone())
            .or_default()
            .open_overlay = true;
        self.retained_layers = retained_layers;
        if let Some((filesystem, binding)) = committed_disk {
            self.filesystems.insert(scope.clone(), filesystem);
            self.resource_bindings.insert(id.clone(), binding);
        }
        self.project_plans.insert(scope.clone(), plan);
        self.resource_projects.insert(id.clone(), scope);
        self.next_disk_version = next_disk_version;
        self.gc_scopes();
        let mut affected = strings(affected);
        affected.insert(id.to_string());
        Ok(affected)
    }

    pub fn remove_disk(&mut self, uri: &Url) -> Result<BTreeSet<String>, String> {
        let id = uri_id(uri)?;
        let scope = self.resource_projects.get(&id).cloned();
        let mut retained_layers = self.retained_layers.clone();
        if let Some(scope) = &scope {
            let plan = self
                .project_plans
                .get(scope)
                .copied()
                .ok_or_else(|| "workspace resource limit plan is missing".to_owned())?;
            let charge = self.retained_charge(&id);
            let budget = retained_layers
                .get(scope)
                .cloned()
                .unwrap_or_default()
                .with_layers(
                    id.clone(),
                    RetainedLayerCharge::new(None, charge.overlay_bytes()),
                    plan.retained_layers,
                )
                .map_err(|error| error.to_string())?;
            retained_layers.insert(scope.clone(), budget);
        }
        let mut inner = self.inner.clone();
        let mut affected = strings(inner.remove_disk(&id));
        affected.extend(self.include_dependents(&id));
        self.release_resource_binding(&id)?;
        self.inner = inner;
        self.retained_layers = retained_layers;
        if self.inner.get(&id).is_none() {
            self.resource_projects.remove(&id);
        }
        self.gc_scopes();
        Ok(affected)
    }

    pub fn close_open(&mut self, uri: &Url) -> Result<BTreeSet<String>, String> {
        let id = uri_id(uri)?;
        let mut retained_layers = self.retained_layers.clone();
        if let Some(project) = self.resource_projects.get(&id).cloned() {
            let plan = self
                .project_plans
                .get(&project)
                .copied()
                .ok_or_else(|| "workspace resource limit plan is missing".to_owned())?;
            let budget = retained_layers
                .get(&project)
                .cloned()
                .unwrap_or_default()
                .with_overlay(id.clone(), None, plan.retained_layers)
                .map_err(|error| error.to_string())?;
            retained_layers.insert(project, budget);
        }
        let mut inner = self.inner.clone();
        let mut affected = inner
            .close_overlay(&id)
            .map_err(|error| error.to_string())?;
        let remove_root = self
            .analysis_root_roles
            .get(&id)
            .copied()
            .is_some_and(|mut roles| {
                roles.open_overlay = false;
                !roles.is_root()
            });
        if remove_root {
            inner.unregister_root(&id);
        }
        affected.remove(&id);
        self.inner = inner;
        if let Some(roles) = self.analysis_root_roles.get_mut(&id) {
            roles.open_overlay = false;
        }
        if remove_root {
            self.analysis_root_roles.remove(&id);
        }
        self.retained_layers = retained_layers;
        if self.inner.get(&id).is_none() {
            self.resource_projects.remove(&id);
        }
        self.gc_scopes();
        Ok(strings(affected))
    }

    /// Decides whether one include target may be read for this analysis root.
    ///
    /// Scan exclusions are intentionally absent here: they choose which files a
    /// workspace walk discovers on its own, not which files a document may
    /// include by name.
    fn admit_include_target(
        &self,
        root_scope: &ProjectScopeId,
        allowed_roots: &[PathBuf],
        target: &ResourceId,
    ) -> Result<Option<AdmittedIncludeTarget>, String> {
        let Ok(target_uri) = Url::parse(target.as_str()) else {
            return Ok(None);
        };
        let Ok(target_path) = target_uri.to_file_path() else {
            return Ok(None);
        };
        let Ok(admitted) = workspace_logical_file_status(
            &self.roots,
            self.filesystem_policy.as_ref(),
            &target_path,
        ) else {
            return Ok(None);
        };
        let canonical = admitted.path();
        let authority_roots = if allowed_roots.is_empty() {
            std::slice::from_ref(&root_scope.workspace_root)
        } else {
            allowed_roots
        };
        if !authority_roots
            .iter()
            .any(|root| canonical.starts_with(root))
        {
            return Ok(None);
        }
        let (scope, config) = scope_and_config_for_path_typed(
            &self.roots,
            self.filesystem_policy.as_ref(),
            canonical,
        )
        .map_err(|error| error.to_string())?;
        if root_scope.config_path.is_none() && scope != *root_scope {
            return Ok(None);
        }
        if !resource_path_is_allowed(config.as_ref(), canonical) {
            return Ok(None);
        }
        let plan = config.as_ref().map_or_else(
            adocweave_config::ResolvedResourceLimitPlan::default,
            |snapshot| snapshot.config.resources.limit_plan,
        );
        Ok(Some(match admitted {
            WorkspaceLogicalFile::Existing(path) => {
                AdmittedIncludeTarget::Existing(Box::new(ExistingIncludeTarget {
                    uri: target_uri,
                    path,
                    scope,
                    plan,
                }))
            }
            WorkspaceLogicalFile::Missing(_) => AdmittedIncludeTarget::Missing,
        }))
    }

    pub fn input(&mut self, root: &Url) -> Result<WorkspaceInput, String> {
        let root_id = uri_id(root)?;
        if self.inner.get(&root_id).is_none() {
            return Err(format!("workspace resource is missing: {root}"));
        }
        let root_scope = self
            .resource_projects
            .get(&root_id)
            .ok_or_else(|| format!("workspace project scope is missing: {root}"))?
            .clone();
        let root_scope = &root_scope;
        let mut allowed_schemes = BTreeSet::new();
        allowed_schemes.insert("file".to_owned());
        let config_snapshot = self.config_for_uri(root)?;
        let project_config = config_snapshot.as_ref().map_or_else(
            adocweave_config::ResolvedProjectConfig::default,
            |snapshot| snapshot.config.clone(),
        );
        let mut options = project_config.preprocess.clone();
        if config_snapshot.is_none() {
            options.enable_includes = true;
        }
        options.base_uri = parent_uri(root);
        options.safe_mode = SafeMode::Server;
        options.allowed_schemes = allowed_schemes;
        let allowed_roots = if options.enable_includes {
            configured_include_roots(
                &project_config,
                &self.roots,
                self.filesystem_policy.as_ref(),
            )?
        } else {
            Vec::new()
        };
        let limits = project_config.resources.limit_plan.analysis_snapshot;
        let mut budget = adocweave_config::AnalysisSnapshotBudget::new(limits);
        let snapshot = self.inner.try_snapshot_resources(|id, resource| {
            let resource_scope = self.resource_projects.get(id);
            let same_scope = resource_scope.is_some_and(|scope| {
                scope.workspace_root == root_scope.workspace_root
                    && (root_scope.config_path.is_some() || scope == root_scope)
            });
            let allowed = if !same_scope {
                false
            } else if id == &root_id {
                true
            } else if !options.enable_includes {
                false
            } else if allowed_roots.is_empty() {
                true
            } else {
                Url::parse(id.as_str())
                    .ok()
                    .and_then(|uri| uri.to_file_path().ok())
                    .is_some_and(|path| allowed_roots.iter().any(|root| path.starts_with(root)))
            };
            if !allowed {
                return Ok::<bool, String>(false);
            }
            budget
                .charge(resource.text().len() as u64)
                .map_err(|error| error.to_string())?;
            Ok::<bool, String>(true)
        })?;
        Ok(WorkspaceInput {
            generation: snapshot.generation(),
            root: root_id,
            snapshot,
            options,
            config_sha256: config_snapshot.map(|snapshot| snapshot.content_sha256),
            project_config,
        })
    }

    pub fn input_is_current(&mut self, input: &WorkspaceInput) -> bool {
        input.generation == self.generation()
            && self.config_for_id(&input.root).is_ok_and(|snapshot| {
                snapshot.map(|value| value.content_sha256) == input.config_sha256
            })
    }

    /// Analyses one root, reading each missing include as it is requested.
    ///
    /// Everything happens on a copy of this workspace, so the method takes
    /// `&self` and can run on a worker thread while the editor keeps using the
    /// current state. One suspension is answered at a time and the same
    /// continuation resumes, so an include never restarts the analysis.
    ///
    /// The reads share `job`, which bounds the work of the whole analysis rather
    /// than of each file. Abandoning the returned value drops the filesystem
    /// drafts and leaves no acquired resource behind.
    pub(crate) fn analyze_root_detached(
        &self,
        input: &WorkspaceInput,
        analysis_options: &adocweave::AnalysisOptions,
        cancellation: &dyn CancellationCheck,
        job: &FilesystemJobCoordinator,
    ) -> Result<AnalyzedRoot, String> {
        let options =
            EffectiveProcessingOptions::new(analysis_options.clone(), input.options.clone())
                .map_err(|error| error.to_string())?;
        let root_scope = self
            .resource_projects
            .get(&input.root)
            .ok_or_else(|| format!("workspace project scope is missing: {}", input.root))?
            .clone();
        let allowed_roots = if input.options.enable_includes {
            configured_include_roots(
                &input.project_config,
                &self.roots,
                self.filesystem_policy.as_ref(),
            )?
        } else {
            Vec::new()
        };
        let mut acquisition = IncludeAcquisition {
            candidate: self.clone(),
            drafts: BTreeMap::new(),
            root: input.root.clone(),
            root_scope,
            allowed_roots,
            requested: BTreeSet::new(),
            read_failure: None,
            job,
        };
        let mut step = input.snapshot.analyze_resumable(
            acquisition.root(),
            &options,
            ProjectionLimits::default(),
            &SharedCancellation(cancellation),
        );
        loop {
            match step {
                WorkspaceAnalysisStep::Complete(draft) => {
                    let requested_includes = acquisition.requested.clone();
                    // A read that failed leaves its draft unusable, so a run
                    // that got this far anyway must still keep nothing.
                    if let Some(message) = acquisition.read_failure {
                        return Ok(AnalyzedRoot {
                            candidate: self.clone(),
                            root: input.root.clone(),
                            canonical_options: options,
                            outcome: AnalyzedRootOutcome::ReadFailed(message),
                            requested_includes,
                        });
                    }
                    return Ok(AnalyzedRoot {
                        candidate: acquisition.commit()?,
                        root: input.root.clone(),
                        canonical_options: options,
                        outcome: AnalyzedRootOutcome::Complete(draft),
                        requested_includes,
                    });
                }
                WorkspaceAnalysisStep::Failed(error) => {
                    return Ok(AnalyzedRoot {
                        candidate: self.clone(),
                        root: input.root.clone(),
                        canonical_options: options,
                        outcome: AnalyzedRootOutcome::Failed(error),
                        requested_includes: acquisition.requested,
                    });
                }
                WorkspaceAnalysisStep::Cancelled => {
                    return Ok(AnalyzedRoot {
                        candidate: self.clone(),
                        root: input.root.clone(),
                        canonical_options: options,
                        outcome: AnalyzedRootOutcome::Cancelled,
                        requested_includes: acquisition.requested,
                    });
                }
                WorkspaceAnalysisStep::NeedResource(suspended) => {
                    let target = ResourceId::new(suspended.request().target())
                        .map_err(|error| error.to_string())?;
                    let response = match acquisition.acquire(&target)? {
                        AcquiredInclude::Found(text) => suspended.request().found(text),
                        AcquiredInclude::NotFound => suspended.request().not_found(),
                        AcquiredInclude::Failed(message) => {
                            suspended.request().load_failed(message)
                        }
                    };
                    step = suspended.resume(response, &SharedCancellation(cancellation));
                }
            }
        }
    }

    /// Installs one finished analysis and the resources it acquired.
    ///
    /// The starting generation and the configuration are checked before
    /// anything moves, so a workspace that changed while the analysis ran
    /// discards the result instead of publishing a stale view.
    pub(crate) fn apply_analyzed_root(
        &mut self,
        analyzed: AnalyzedRoot,
    ) -> Result<Option<WorkspaceAnalysis>, String> {
        let AnalyzedRoot {
            candidate,
            root,
            canonical_options,
            outcome,
            requested_includes,
        } = analyzed;
        let AnalyzedRootOutcome::Complete(draft) = outcome else {
            self.watch_requested_includes(&root, requested_includes);
            return Ok(None);
        };
        if !draft.matches_canonical_context(self.generation(), &canonical_options) {
            self.watch_requested_includes(&root, requested_includes);
            return Ok(None);
        }
        // Publication is decided on the copy, so installing it below is the last
        // step and cannot fail. Finalising against the live state instead would
        // leave the acquired includes installed with no analysis to justify them
        // whenever that check rejected the draft.
        let mut candidate = candidate;
        let analysis = candidate
            .inner
            .finalize_draft(draft)
            .map_err(|error| error.to_string())?;
        candidate.accept_for_root(&root, &analysis, requested_includes)?;
        *self = candidate;
        Ok(Some(analysis))
    }

    /// Keeps watching what a run asked for even though it produced no result.
    ///
    /// A document whose include could not be read is exactly the document that
    /// needs to hear about the repair. Recording the request here, rather than
    /// when the read was attempted, keeps a run that is still in flight from
    /// changing anything the editor can see.
    fn watch_requested_includes(&mut self, root: &ResourceId, requested: BTreeSet<ResourceId>) {
        for id in &requested {
            if !self.include_interests.contains(id)
                && self.include_interests.len() >= MAX_WATCHED_INCLUDE_RESOURCES
            {
                break;
            }
            self.include_interests.insert(id.clone());
        }
        let watched = requested
            .into_iter()
            .filter(|id| self.include_interests.contains(id))
            .collect();
        self.include_dependencies.insert(root.clone(), watched);
        self.prune_unreferenced_include_resources();
    }

    /// Publishes one analysis and records what its root depends on.
    ///
    /// The dependency set has two sources. The analysis reports the resources it
    /// actually used, which covers includes the starting snapshot already held.
    /// The run reports what it asked the host for, which covers includes it
    /// acquired and, importantly, includes that turned out to be missing. A
    /// missing target is still something the document is waiting for, so it has
    /// to stay watched.
    pub fn accept_for_root(
        &mut self,
        root: &ResourceId,
        analysis: &WorkspaceAnalysis,
        requested_includes: BTreeSet<ResourceId>,
    ) -> Result<(), String> {
        if analysis.root() != root {
            return Err("workspace analysis root does not match the adoption root".to_owned());
        }
        self.inner
            .accept(analysis)
            .map_err(|error| error.to_string())?;
        let dependencies = analysis
            .dependencies()
            .into_iter()
            .chain(requested_includes)
            .filter(|id| self.include_interests.contains(id))
            .collect();
        self.include_dependencies.insert(root.clone(), dependencies);
        self.prune_unreferenced_include_resources();
        Ok(())
    }

    pub fn forget_include_dependencies(&mut self, root: &Url) -> Result<BTreeSet<String>, String> {
        let root = uri_id(root)?;
        self.include_dependencies.remove(&root);
        Ok(self.prune_unreferenced_include_resources())
    }

    fn prune_unreferenced_include_resources(&mut self) -> BTreeSet<String> {
        let retained = self
            .include_dependencies
            .values()
            .flat_map(BTreeSet::iter)
            .cloned()
            .collect::<BTreeSet<_>>();
        let stale = self
            .include_interests
            .difference(&retained)
            .cloned()
            .collect::<Vec<_>>();
        let mut affected = BTreeSet::new();
        for id in stale {
            self.include_interests.remove(&id);
            let was_loaded_include = self.loaded_include_resources.remove(&id);
            if !was_loaded_include
                || self
                    .analysis_root_roles
                    .get(&id)
                    .copied()
                    .is_some_and(AnalysisRootRoles::is_root)
            {
                continue;
            }
            let Ok(uri) = Url::parse(id.as_str()) else {
                continue;
            };
            if let Ok(removed) = self.remove_disk(&uri) {
                affected.extend(removed);
            }
        }
        affected
    }

    /// Returns the analysis roots that asked for one include target.
    ///
    /// A target that is currently missing is not a workspace resource, so the
    /// workspace's own dependency graph cannot report it. This lookup is what
    /// lets creating a missing include re-analyse the documents waiting for it.
    fn include_dependents(&self, id: &ResourceId) -> BTreeSet<String> {
        self.include_dependencies
            .iter()
            .filter(|(_, dependencies)| dependencies.contains(id))
            .map(|(root, _)| root.to_string())
            .collect()
    }

    pub const fn generation(&self) -> Generation {
        self.inner.generation()
    }

    fn config_for_id(
        &mut self,
        id: &ResourceId,
    ) -> Result<Option<adocweave_config::ConfigSnapshot>, String> {
        let uri = Url::parse(id.as_str()).map_err(|error| error.to_string())?;
        self.config_for_uri(&uri)
    }

    fn config_for_uri(
        &mut self,
        uri: &Url,
    ) -> Result<Option<adocweave_config::ConfigSnapshot>, String> {
        let path = uri
            .to_file_path()
            .map_err(|()| format!("workspace resource is not a file URI: {uri}"))?;
        self.cached_config_for_path(&path)
            .map_err(|error| error.to_string())
    }

    /// Resolves a path's project file, reading it at most once per directory.
    fn cached_config_for_path(
        &mut self,
        path: &Path,
    ) -> Result<Option<adocweave_config::ConfigSnapshot>, ScopeConfigError> {
        let cache_key = path.parent().unwrap_or(path).to_owned();
        if let Some(cached) = self.config_cache.get(&cache_key) {
            return Ok(cached.clone());
        }
        let config = config_for_path_typed(&self.roots, self.filesystem_policy.as_ref(), path)?;
        self.config_cache.insert(cache_key, config.clone());
        Ok(config)
    }

    /// Forgets every remembered project file.
    ///
    /// Called when a project file or the set of roots changes. A snapshot found
    /// for one directory can come from an ancestor, so a single edited file can
    /// invalidate entries recorded under many directories; clearing all of them
    /// keeps the cache from ever answering with a stale configuration.
    fn forget_configs(&mut self) {
        self.config_cache.clear();
    }

    fn open_scope_and_plan(
        &self,
        uri: &Url,
    ) -> Result<
        Option<(ProjectScopeId, adocweave_config::ResolvedResourceLimitPlan)>,
        ScopeConfigError,
    > {
        let path = uri.to_file_path().map_err(|()| {
            ScopeConfigError::Other(format!("workspace resource is not a file URI: {uri}"))
        })?;
        if !self.path_is_analysis_root(&path) {
            return Ok(None);
        }
        let admission_path = if self.roots.is_empty() {
            path.clone()
        } else {
            workspace_logical_file(&self.roots, self.filesystem_policy.as_ref(), &path)
                .map_err(ScopeConfigError::Other)?
        };
        let (scope, config) = scope_and_config_for_path_typed(
            &self.roots,
            self.filesystem_policy.as_ref(),
            &admission_path,
        )?;
        if !resource_path_is_allowed(config.as_ref(), &admission_path) {
            return Ok(None);
        }
        let plan = config.as_ref().map_or_else(
            adocweave_config::ResolvedResourceLimitPlan::default,
            |snapshot| snapshot.config.resources.limit_plan,
        );
        Ok(Some((scope, plan)))
    }

    fn path_is_analysis_root(&self, path: &Path) -> bool {
        path_is_analysis_root(path, &self.directory_roots, &self.single_file_roots)
    }

    fn path_is_scan_excluded(&self, path: &Path) -> bool {
        if self.single_file_roots.contains(path) {
            return false;
        }
        let Some(root) = self
            .directory_roots
            .iter()
            .filter(|root| path.starts_with(root))
            .max_by_key(|root| root.components().count())
        else {
            return false;
        };
        let Some(settings) = self.scan_settings.get(root) else {
            return false;
        };
        let mut directory = path.parent();
        while let Some(candidate) = directory {
            if candidate == root {
                break;
            }
            if let Ok(relative) = candidate.strip_prefix(root)
                && settings.excludes(relative)
            {
                return true;
            }
            directory = candidate.parent();
        }
        false
    }
}

/// One file read through a draft, together with the claim it established.
///
/// The binding is what later releases this resource's charge on its session. It
/// names a generation, so a claim from an earlier read cannot release a
/// resource that has since been read again.
struct ReadCandidate {
    source_id: LogicalSourceId,
    text: Arc<str>,
    binding: FilesystemResourceBinding,
}

fn read_scan_candidate(
    filesystem: &mut LocalFilesystemDraft,
    path: &Path,
) -> Result<Option<ReadCandidate>, String> {
    let uri = Url::from_file_path(path)
        .map_err(|()| format!("cannot convert workspace path to URI: {}", path.display()))?;
    let outcome = filesystem
        .read_utf8_outcome(
            LogicalSourceId::new(uri.to_string()).map_err(|error| error.to_string())?,
            path,
        )
        .map_err(|error| error.to_string())?;
    Ok(match outcome {
        FilesystemReadOutcome::Found(file) => {
            let (source_id, text, binding) = file.into_parts_with_binding();
            Some(ReadCandidate {
                source_id,
                text,
                binding,
            })
        }
        FilesystemReadOutcome::NotFound { .. } => None,
    })
}

const fn adapter_managed_workspace_limits() -> WorkspaceLimits {
    WorkspaceLimits {
        resources: RetainedResourceLimits {
            max_files: usize::MAX,
            max_total_bytes: u64::MAX,
            max_resource_bytes: u64::MAX,
        },
        max_roots: usize::MAX,
    }
}

fn uri_id(uri: &Url) -> Result<ResourceId, String> {
    ResourceId::new(uri.to_string()).map_err(|error| error.to_string())
}

fn path_is_analysis_root(
    path: &Path,
    directory_roots: &[PathBuf],
    single_file_roots: &BTreeSet<PathBuf>,
) -> bool {
    (directory_roots.is_empty() && single_file_roots.is_empty())
        || single_file_roots.contains(path)
        || directory_roots.iter().any(|root| path.starts_with(root))
}

fn resource_path_is_allowed(
    config: Option<&adocweave_config::ConfigSnapshot>,
    path: &Path,
) -> bool {
    config.is_none_or(|snapshot| {
        snapshot.config.resources.roots.is_empty()
            || snapshot
                .config
                .resources
                .roots
                .iter()
                .any(|root| path.starts_with(root))
    })
}

fn configured_include_roots(
    config: &adocweave_config::ResolvedProjectConfig,
    workspace_roots: &[PathBuf],
    filesystem_policy: Option<&LocalFilesystemPolicy>,
) -> Result<Vec<PathBuf>, String> {
    config
        .resources
        .roots
        .iter()
        .map(|root| {
            let boundary = workspace_roots
                .iter()
                .filter(|workspace_root| root.starts_with(workspace_root))
                .max_by_key(|workspace_root| workspace_root.components().count())
                .ok_or_else(|| {
                    format!(
                        "configured root is outside the workspace: {}",
                        root.display()
                    )
                })?;
            let policy = filesystem_policy
                .and_then(|filesystem| filesystem.root_policy(boundary))
                .ok_or_else(|| "workspace root has no retained filesystem authority".to_owned())?;
            policy
                .inspect_directory_no_symlinks(root)
                .map_err(|error| format!("cannot verify configured root: {error}"))
        })
        .collect()
}

enum WorkspaceLogicalFile {
    Existing(PathBuf),
    Missing(PathBuf),
}

impl WorkspaceLogicalFile {
    fn path(&self) -> &Path {
        match self {
            Self::Existing(path) | Self::Missing(path) => path,
        }
    }
}

fn workspace_logical_file_status(
    roots: &[PathBuf],
    filesystem_policy: Option<&LocalFilesystemPolicy>,
    path: &Path,
) -> Result<WorkspaceLogicalFile, String> {
    let logical = workspace_logical_path(roots, filesystem_policy, path)?;
    let boundary = roots
        .iter()
        .filter(|root| logical.starts_with(root))
        .max_by_key(|root| root.components().count())
        .ok_or_else(|| "normalized workspace resource left its workspace boundary".to_owned())?;
    let policy = filesystem_policy
        .and_then(|filesystem| filesystem.root_policy(boundary))
        .ok_or_else(|| "workspace root has no retained filesystem authority".to_owned())?;
    match policy.inspect_candidate(&logical) {
        Ok(canonical) => Ok(WorkspaceLogicalFile::Existing(canonical)),
        Err(adocweave_host::LocalTargetError::Missing(_)) => {
            Ok(WorkspaceLogicalFile::Missing(logical))
        }
        Err(error) => Err(error.to_string()),
    }
}

fn workspace_logical_path(
    roots: &[PathBuf],
    filesystem_policy: Option<&LocalFilesystemPolicy>,
    path: &Path,
) -> Result<PathBuf, String> {
    let boundary = roots
        .iter()
        .filter(|root| path.starts_with(root))
        .max_by_key(|root| root.components().count())
        .ok_or_else(|| {
            format!(
                "workspace resource is outside every workspace root: {} (roots: {})",
                path.display(),
                roots
                    .iter()
                    .map(|root| root.display().to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        })?;
    let policy = filesystem_policy
        .and_then(|filesystem| filesystem.root_policy(boundary))
        .ok_or_else(|| "workspace root has no retained filesystem authority".to_owned())?;
    let logical = policy
        .normalize_candidate(path)
        .map_err(|error| error.to_string())?;
    Ok(logical)
}

fn workspace_logical_file(
    roots: &[PathBuf],
    filesystem_policy: Option<&LocalFilesystemPolicy>,
    path: &Path,
) -> Result<PathBuf, String> {
    match workspace_logical_file_status(roots, filesystem_policy, path)? {
        WorkspaceLogicalFile::Existing(path) | WorkspaceLogicalFile::Missing(path) => Ok(path),
    }
}

fn config_for_path_typed(
    roots: &[PathBuf],
    filesystem_policy: Option<&LocalFilesystemPolicy>,
    path: &Path,
) -> Result<Option<adocweave_config::ConfigSnapshot>, ScopeConfigError> {
    let boundary = roots
        .iter()
        .filter(|root| path.starts_with(root))
        .max_by_key(|root| root.components().count());
    let Some(boundary) = boundary else {
        return Ok(None);
    };
    let policy = filesystem_policy
        .and_then(|filesystem| filesystem.root_policy(boundary))
        .ok_or_else(|| {
            ScopeConfigError::Other(
                "workspace root has no retained filesystem authority".to_owned(),
            )
        })?;
    adocweave_config::discover_and_load_with_policy(path, policy).map_err(ScopeConfigError::Config)
}

fn scan_config_for_path(
    roots: &[PathBuf],
    filesystem_policy: Option<&LocalFilesystemPolicy>,
    filesystem: Option<&mut LocalFilesystemDraft>,
    path: &Path,
    cache_key: PathBuf,
    by_directory: &mut BTreeMap<PathBuf, Option<adocweave_config::ConfigSnapshot>>,
    by_path: &mut BTreeMap<PathBuf, adocweave_config::ConfigSnapshot>,
) -> Result<Option<adocweave_config::ConfigSnapshot>, ScopeConfigError> {
    if let Some(cached) = by_directory.get(&cache_key) {
        return Ok(cached.clone());
    }
    let boundary = roots
        .iter()
        .filter(|root| path.starts_with(root))
        .max_by_key(|root| root.components().count());
    let Some(boundary) = boundary else {
        by_directory.insert(cache_key, None);
        return Ok(None);
    };
    let policy = filesystem_policy
        .and_then(|filesystem| filesystem.root_policy(boundary))
        .ok_or_else(|| {
            ScopeConfigError::Other(
                "workspace root has no retained filesystem authority".to_owned(),
            )
        })?;
    let discovered =
        adocweave_config::discover_with_policy(path, policy).map_err(ScopeConfigError::Config)?;
    let snapshot = match discovered {
        None => None,
        Some(config_path) => {
            if let Some(cached) = by_path.get(&config_path) {
                Some(cached.clone())
            } else {
                let filesystem = filesystem.ok_or_else(|| {
                    ScopeConfigError::Other(
                        "workspace configuration has no filesystem draft".to_owned(),
                    )
                })?;
                let uri = Url::from_file_path(&config_path).map_err(|()| {
                    ScopeConfigError::Other(format!(
                        "cannot convert project configuration path to URI: {}",
                        config_path.display()
                    ))
                })?;
                let source_id = LogicalSourceId::new(uri.to_string())
                    .map_err(|error| ScopeConfigError::Other(error.to_string()))?;
                let loaded = match filesystem.read_utf8_no_symlinks_outcome(source_id, &config_path)
                {
                    Ok(FilesystemReadOutcome::Found(loaded)) => loaded,
                    Ok(FilesystemReadOutcome::NotFound { .. }) => {
                        return Err(ScopeConfigError::Transient(
                            "the project file disappeared while it was read".to_owned(),
                        ));
                    }
                    Err(error @ FilesystemDraftError::Job(_)) => {
                        return Err(ScopeConfigError::Other(error.to_string()));
                    }
                    Err(error) => {
                        return Err(ScopeConfigError::Transient(error.to_string()));
                    }
                };
                let snapshot = adocweave_config::ConfigSnapshot::from_filesystem_source(&loaded)
                    .map_err(ScopeConfigError::Config)?;
                by_path.insert(config_path, snapshot.clone());
                Some(snapshot)
            }
        }
    };
    by_directory.insert(cache_key, snapshot.clone());
    Ok(snapshot)
}

fn scope_and_config_for_path_typed(
    roots: &[PathBuf],
    filesystem_policy: Option<&LocalFilesystemPolicy>,
    path: &Path,
) -> Result<(ProjectScopeId, Option<adocweave_config::ConfigSnapshot>), ScopeConfigError> {
    let boundary = roots
        .iter()
        .filter(|root| path.starts_with(root))
        .max_by_key(|root| root.components().count());
    let Some(boundary) = boundary else {
        if roots.is_empty() {
            return Ok((
                ProjectScopeId {
                    workspace_root: path.parent().unwrap_or_else(|| Path::new("")).to_owned(),
                    config_path: None,
                },
                None,
            ));
        }
        return Err(ScopeConfigError::Other(
            "workspace resource is outside every workspace root".to_owned(),
        ));
    };
    let policy = filesystem_policy
        .and_then(|filesystem| filesystem.root_policy(boundary))
        .ok_or_else(|| {
            ScopeConfigError::Other(
                "workspace root has no retained filesystem authority".to_owned(),
            )
        })?;
    let config = adocweave_config::discover_and_load_with_policy(path, policy)
        .map_err(ScopeConfigError::Config)?;
    Ok((
        ProjectScopeId {
            workspace_root: boundary.clone(),
            config_path: config.as_ref().map(|snapshot| snapshot.path.clone()),
        },
        config,
    ))
}

fn strings(values: BTreeSet<ResourceId>) -> BTreeSet<String> {
    values.into_iter().map(|value| value.to_string()).collect()
}

fn parent_uri(uri: &Url) -> Option<String> {
    uri.join(".").ok().map(|uri| uri.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_DIRECTORY: AtomicU64 = AtomicU64::new(0);

    struct TestDirectory(PathBuf);

    impl TestDirectory {
        fn new() -> Self {
            let id = NEXT_DIRECTORY.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "adocweave-lsp-filesystem-session-{}-{id}",
                std::process::id()
            ));
            std::fs::create_dir(&path).expect("workspace root");
            Self(path)
        }
    }

    impl Drop for TestDirectory {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// Analyses one root the way an analysis worker does.
    ///
    /// The reads happen on a copy, so `resources` is unchanged until the result
    /// is installed with [`WorkspaceResources::apply_analyzed_root`].
    fn analyze_root(
        resources: &mut WorkspaceResources,
        root: &Url,
    ) -> Result<AnalyzedRoot, String> {
        let input = resources.input(root)?;
        let job = FilesystemJobCoordinator::new(document_analysis_job_limits())
            .map_err(|error| error.to_string())?;
        resources.analyze_root_detached(
            &input,
            &adocweave::AnalysisOptions::default(),
            &NeverCancel,
            &job,
        )
    }

    /// Analyses one root and installs the result, as the server does on completion.
    fn analyze_and_apply(
        resources: &mut WorkspaceResources,
        root: &Url,
    ) -> Result<Option<WorkspaceAnalysis>, String> {
        let analyzed = analyze_root(resources, root)?;
        resources.apply_analyzed_root(analyzed)
    }

    fn write_resource_config(
        directory: &Path,
        max_files: usize,
        max_total_bytes: u64,
        max_resource_bytes: u64,
        include: bool,
    ) {
        std::fs::write(
            directory.join(adocweave_config::FILE_NAME),
            format!(
                "schema-version = 1\n[resources]\ninclude = {include}\nroots = [\".\"]\nmax-files = {max_files}\nmax-total-bytes = {max_total_bytes}\nmax-resource-bytes = {max_resource_bytes}\n"
            ),
        )
        .expect("project configuration");
    }

    #[test]
    fn a_project_file_is_read_once_per_directory_and_forgotten_when_it_changes() {
        let root = TestDirectory::new();
        let source = root.0.join("a.adoc");
        std::fs::write(&source, "first\n").expect("source");
        write_resource_config(&root.0, 8, 4096, 4096, true);
        let root_uri = Url::from_directory_path(&root.0).expect("root URI");
        let source_uri = Url::from_file_path(&source).expect("source URI");
        let mut resources = WorkspaceResources::default();
        resources
            .load_roots(std::slice::from_ref(&root_uri))
            .expect("load workspace");

        let first = resources.input(&source_uri).expect("workspace input");
        assert_eq!(resources.config_cache.len(), 1);

        // Replacing the file on disk without reloading must not change the
        // answer: repeated keystrokes read the remembered configuration.
        write_resource_config(&root.0, 4, 2048, 2048, true);
        let repeated = resources.input(&source_uri).expect("workspace input");
        assert_eq!(repeated.config_sha256, first.config_sha256);

        // A reload is what tells the server the project file may have changed.
        resources.load_roots(&[root_uri]).expect("reload workspace");
        let reloaded = resources.input(&source_uri).expect("workspace input");
        assert_ne!(reloaded.config_sha256, first.config_sha256);
    }

    #[test]
    fn filesystem_scan_ingests_logical_resources_before_snapshot_analysis() {
        let root = TestDirectory::new();
        let first = root.0.join("a.adoc");
        let second = root.0.join("b.adoc");
        std::fs::write(&first, "first\n").expect("first source");
        std::fs::write(&second, "second\n").expect("second source");
        std::fs::write(root.0.join("ignored.txt"), "ignored\n").expect("ignored source");
        let root_uri = Url::from_directory_path(&root.0).expect("root URI");
        let first_uri = Url::from_file_path(&first).expect("first URI");
        let second_uri = Url::from_file_path(&second).expect("second URI");
        let mut resources = WorkspaceResources::default();

        resources.load_roots(&[root_uri]).expect("load workspace");

        assert_eq!(
            resources.inner.roots(),
            &BTreeSet::from([
                uri_id(&first_uri).expect("first resource ID"),
                uri_id(&second_uri).expect("second resource ID"),
            ])
        );
        assert_eq!(
            resources
                .get(&first_uri)
                .expect("first resource")
                .text()
                .as_ref(),
            "first\n"
        );
        assert_eq!(
            resources
                .get(&second_uri)
                .expect("second resource")
                .text()
                .as_ref(),
            "second\n"
        );

        let input = resources.input(&first_uri).expect("workspace input");
        std::fs::remove_file(first).expect("remove first source after snapshot");
        std::fs::remove_file(second).expect("remove second source after snapshot");
        assert_eq!(
            input
                .snapshot
                .get(&input.root)
                .expect("snapshot resource")
                .text()
                .as_ref(),
            "first\n"
        );
    }

    #[test]
    fn scan_candidate_disappearance_does_not_hide_a_remaining_resource() {
        let root = TestDirectory::new();
        let vanished = root.0.join("vanished.adoc");
        let remaining = root.0.join("remaining.adoc");
        std::fs::write(&vanished, "vanished\n").expect("vanishing source");
        std::fs::write(&remaining, "remaining\n").expect("remaining source");
        let candidates = [vanished.clone(), remaining.clone()];
        let session = LocalFilesystemPolicy::new(
            [root.0.clone()],
            adocweave_host::FilesystemReadLimits::default(),
        )
        .expect("filesystem policy")
        .session()
        .expect("filesystem session");
        let job = FilesystemJobCoordinator::new(workspace_scan_job_limits()).expect("scan job");
        let mut filesystem = session.draft(&job).expect("filesystem draft");
        std::fs::remove_file(&vanished).expect("remove discovered source");

        assert!(
            read_scan_candidate(&mut filesystem, &candidates[0])
                .expect("vanished candidate")
                .is_none()
        );
        let read = read_scan_candidate(&mut filesystem, &candidates[1])
            .expect("remaining candidate")
            .expect("remaining source");
        assert_eq!(
            read.source_id.as_str(),
            Url::from_file_path(&remaining).unwrap().as_str()
        );
        assert_eq!(read.text.as_ref(), "remaining\n");
    }

    #[cfg(unix)]
    #[test]
    fn project_config_replaced_by_a_symlink_after_discovery_is_not_read() {
        use std::os::unix::fs::symlink;

        let root = TestDirectory::new();
        let config = root.0.join(adocweave_config::FILE_NAME);
        let replacement = root.0.join("replacement.toml");
        std::fs::write(&config, "schema-version = 1\n").expect("project configuration");
        std::fs::write(&replacement, "schema-version = 1\n").expect("replacement");
        let policy = LocalFilesystemPolicy::new([root.0.clone()], FilesystemReadLimits::DEFAULT)
            .expect("filesystem policy");
        let discovered = adocweave_config::discover_with_policy(
            &root.0,
            policy.root_policy(&root.0).expect("root policy"),
        )
        .expect("configuration discovery")
        .expect("configuration path");
        std::fs::remove_file(&config).expect("remove discovered configuration");
        symlink(&replacement, &config).expect("replace configuration with symlink");
        let session = policy
            .access_existing([root.0.clone()], workspace_config_read_limits())
            .and_then(|access| access.session())
            .expect("configuration session");
        let job = FilesystemJobCoordinator::new(workspace_scan_job_limits()).expect("scan job");
        let mut draft = session.draft(&job).expect("configuration draft");
        let source_id = LogicalSourceId::new(
            Url::from_file_path(&discovered)
                .expect("configuration URI")
                .to_string(),
        )
        .expect("source ID");

        assert!(
            draft
                .read_utf8_no_symlinks_outcome(source_id, &discovered)
                .is_err()
        );
        let usage = job.usage().expect("job usage");
        assert_eq!(usage.read_operations, 1);
        assert_eq!(usage.read_bytes, 0);
    }

    #[test]
    fn workspace_scan_accounts_for_discovery_and_multiple_project_scopes_in_one_job() {
        let root = TestDirectory::new();
        let nested = root.0.join("nested");
        std::fs::create_dir(&nested).expect("nested project");
        std::fs::write(root.0.join("root.adoc"), "root\n").expect("root source");
        std::fs::write(
            nested.join(adocweave_config::FILE_NAME),
            "schema-version = 1\n",
        )
        .expect("nested project configuration");
        std::fs::write(nested.join("nested.adoc"), "nested\n").expect("nested source");
        let root_uri = Url::from_directory_path(&root.0).expect("root URI");
        let resources = WorkspaceResources::default();
        let job = FilesystemJobCoordinator::new(workspace_scan_job_limits()).expect("scan job");

        let loaded = resources.load_roots_detached_with_job(
            std::slice::from_ref(&root_uri),
            &NeverCancel,
            &job,
        );

        assert_eq!(loaded.error, None);
        let usage = job.usage().expect("job usage");
        assert_eq!(usage.sessions, 4);
        assert_eq!(usage.read_operations, 3);
        assert_eq!(usage.read_bytes, 31);
        assert_eq!(usage.candidate_changes, 3);
    }

    #[test]
    fn workspace_scan_read_limit_is_shared_across_project_scopes() {
        let root = TestDirectory::new();
        let nested = root.0.join("nested");
        std::fs::create_dir(&nested).expect("nested project");
        std::fs::write(root.0.join("root.adoc"), "root\n").expect("root source");
        std::fs::write(
            nested.join(adocweave_config::FILE_NAME),
            "schema-version = 1\n",
        )
        .expect("nested project configuration");
        std::fs::write(nested.join("nested.adoc"), "nested\n").expect("nested source");
        let root_uri = Url::from_directory_path(&root.0).expect("root URI");
        let mut resources = WorkspaceResources::default();
        resources
            .load_roots(std::slice::from_ref(&root_uri))
            .expect("initial workspace");
        assert!(!resources.inner.roots().is_empty());
        let job = FilesystemJobCoordinator::new(FilesystemJobLimits {
            max_read_operations: 2,
            ..workspace_scan_job_limits()
        })
        .expect("scan job");

        let loaded = resources.load_roots_detached_with_job(
            std::slice::from_ref(&root_uri),
            &NeverCancel,
            &job,
        );

        assert_eq!(
            loaded.error.as_deref(),
            Some("filesystem job limit exceeded: read operations (2)")
        );
        let usage = job.usage().expect("job usage");
        assert_eq!(usage.sessions, 4);
        assert_eq!(usage.read_operations, 2);
        assert_eq!(usage.candidate_changes, 2);
        assert!(
            resources
                .apply_loaded_roots(loaded, &[])
                .expect_err("job limit must fail closed")
                .contains("filesystem job limit exceeded")
        );
        assert!(resources.inner.roots().is_empty());
        assert!(resources.last_load_failed_closed());
    }

    #[test]
    fn cancelled_workspace_scan_cancels_its_filesystem_job() {
        let root = TestDirectory::new();
        std::fs::write(root.0.join("document.adoc"), "document\n").expect("source");
        let root_uri = Url::from_directory_path(&root.0).expect("root URI");
        let resources = WorkspaceResources::default();
        let cancellation = adocweave::CancellationToken::new();
        cancellation.cancel();
        let job = FilesystemJobCoordinator::new(workspace_scan_job_limits()).expect("scan job");

        let loaded = resources.load_roots_detached_with_job(
            std::slice::from_ref(&root_uri),
            &cancellation,
            &job,
        );

        assert_eq!(
            loaded.error.as_deref(),
            Some("local resource cannot be verified: local filesystem scan was cancelled")
        );
        assert_eq!(
            job.finish(),
            Err(adocweave_host::FilesystemJobError::Cancelled)
        );
        assert!(resources.inner.roots().is_empty());
    }

    #[test]
    fn cancellation_after_the_last_read_discards_the_candidate_before_commit() {
        let root = TestDirectory::new();
        let document = root.0.join("document.adoc");
        std::fs::write(&document, "before\n").expect("source");
        let root_uri = Url::from_directory_path(&root.0).expect("root URI");
        let document_uri = Url::from_file_path(&document).expect("document URI");
        let mut resources = WorkspaceResources::default();
        resources
            .load_roots(std::slice::from_ref(&root_uri))
            .expect("initial workspace");
        let previous = resources
            .get(&document_uri)
            .expect("previous source")
            .text()
            .clone();
        std::fs::write(&document, "after\n").expect("replacement source");
        let mut replacement = resources.clone();
        let cancellation = adocweave::CancellationToken::new();
        let job = FilesystemJobCoordinator::new(workspace_scan_job_limits()).expect("scan job");

        let error = replacement
            .load_roots_with_limits_after_hooks_and_job(
                std::slice::from_ref(&root_uri),
                adapter_managed_workspace_limits(),
                &cancellation,
                &job,
                (|| {}, || {}, || cancellation.cancel()),
            )
            .expect_err("cancelled candidate");

        assert_eq!(error, "workspace scan was cancelled");
        assert_eq!(
            job.finish(),
            Err(adocweave_host::FilesystemJobError::Cancelled)
        );
        assert_eq!(
            resources.get(&document_uri).map(|resource| resource.text()),
            Some(&previous)
        );
        assert!(replacement.inner.roots().is_empty());
    }

    #[test]
    fn scan_exclusion_defers_include_loading_without_promoting_the_resource_to_a_root() {
        let root = TestDirectory::new();
        let generated = root.0.join("nested/generated");
        std::fs::create_dir_all(&generated).expect("generated directory");
        std::fs::write(
            root.0.join(adocweave_config::FILE_NAME),
            concat!(
                "schema-version = 1\n",
                "[resources]\ninclude = true\nroots = [\".\"]\n",
                "[workspace.scan]\nexclude = [\"**/generated\"]\n",
            ),
        )
        .expect("project configuration");
        let source = root.0.join("root.adoc");
        let included = generated.join("part.adoc");
        std::fs::write(&source, "include::nested/generated/part.adoc[]\n").expect("source");
        std::fs::write(&included, "included\n").expect("included source");
        let root_uri = Url::from_directory_path(&root.0).expect("root URI");
        let source_uri = Url::from_file_path(&source).expect("source URI");
        let included_uri = Url::from_file_path(&included).expect("included URI");
        let mut resources = WorkspaceResources::default();

        resources.load_roots(&[root_uri]).expect("load workspace");

        assert_eq!(
            resources.inner.roots(),
            &BTreeSet::from([uri_id(&source_uri).expect("source ID")])
        );
        assert!(resources.get(&included_uri).is_none());

        let analysis = analyze_and_apply(&mut resources, &source_uri)
            .expect("workspace analysis")
            .expect("adopted analysis");

        assert_eq!(
            resources.inner.roots(),
            &BTreeSet::from([uri_id(&source_uri).expect("source ID")]),
            "an excluded include acquired during analysis must not become a root",
        );
        assert!(analysis.analysis.source().contains("included"));

        std::fs::write(&included, "updated include\n").expect("updated include");
        resources
            .reload_file(included_uri.clone())
            .expect("reload known include");
        assert_eq!(
            resources.inner.roots(),
            &BTreeSet::from([uri_id(&source_uri).expect("source ID")]),
            "a watched include must not become an analysis root",
        );
        assert_eq!(
            resources
                .get(&included_uri)
                .expect("updated include resource")
                .text()
                .as_ref(),
            "updated include\n",
        );

        let unrelated = generated.join("unrelated.adoc");
        let unrelated_uri = Url::from_file_path(&unrelated).expect("unrelated URI");
        std::fs::write(&unrelated, "unrelated\n").expect("unrelated source");
        assert!(
            resources
                .reload_file(unrelated_uri.clone())
                .expect("ignore excluded watcher discovery")
                .is_empty()
        );
        assert!(resources.get(&unrelated_uri).is_none());
        assert_eq!(
            resources.inner.roots(),
            &BTreeSet::from([uri_id(&source_uri).expect("source ID")]),
        );
    }

    /// A result the workspace has moved past is rejected without installing any
    /// part of it, including the includes the run acquired along the way.
    #[test]
    fn a_result_from_an_older_generation_installs_nothing() {
        let root = TestDirectory::new();
        let generated = root.0.join("generated");
        std::fs::create_dir_all(&generated).expect("generated directory");
        std::fs::write(
            root.0.join(adocweave_config::FILE_NAME),
            concat!(
                "schema-version = 1\n",
                "[resources]\ninclude = true\nroots = [\".\"]\n",
                "[workspace.scan]\nexclude = [\"generated\"]\n",
            ),
        )
        .expect("project configuration");
        let source = root.0.join("root.adoc");
        let included = generated.join("part.adoc");
        let unrelated = root.0.join("unrelated.adoc");
        std::fs::write(&source, "include::generated/part.adoc[]\n").expect("source");
        std::fs::write(&included, "included\n").expect("included source");
        let root_uri = Url::from_directory_path(&root.0).expect("root URI");
        let source_uri = Url::from_file_path(&source).expect("source URI");
        let included_uri = Url::from_file_path(&included).expect("included URI");
        let mut resources = WorkspaceResources::default();
        resources.load_roots(&[root_uri]).expect("load workspace");
        let analyzed = analyze_root(&mut resources, &source_uri).expect("workspace analysis");

        // The workspace moves on before the result comes back.
        std::fs::write(&unrelated, "unrelated\n").expect("unrelated source");
        resources
            .reload_file(Url::from_file_path(&unrelated).expect("unrelated URI"))
            .expect("discover an unrelated source");
        let generation = resources.generation();

        assert!(
            resources
                .apply_analyzed_root(analyzed)
                .expect("apply a superseded analysis")
                .is_none()
        );
        assert_eq!(resources.generation(), generation);
        assert!(
            resources.get(&included_uri).is_none(),
            "a superseded result must not install the include it acquired"
        );
    }

    #[test]
    fn an_analysis_that_is_never_adopted_leaves_no_include_behind() {
        let root = TestDirectory::new();
        let generated = root.0.join("nested/generated");
        std::fs::create_dir_all(&generated).expect("generated directory");
        std::fs::write(
            root.0.join(adocweave_config::FILE_NAME),
            concat!(
                "schema-version = 1\n",
                "[resources]\ninclude = true\nroots = [\".\"]\n",
                "[workspace.scan]\nexclude = [\"**/generated\"]\n",
            ),
        )
        .expect("project configuration");
        let source = root.0.join("root.adoc");
        let included = generated.join("part.adoc");
        std::fs::write(&source, "include::nested/generated/part.adoc[]\n").expect("source");
        std::fs::write(&included, "included\n").expect("included source");
        let root_uri = Url::from_directory_path(&root.0).expect("root URI");
        let source_uri = Url::from_file_path(&source).expect("source URI");
        let included_uri = Url::from_file_path(&included).expect("included URI");
        let mut resources = WorkspaceResources::default();
        resources.load_roots(&[root_uri]).expect("load workspace");
        let before = resources.generation();

        let analyzed = analyze_root(&mut resources, &source_uri).expect("workspace analysis");
        drop(analyzed);

        assert!(
            resources.get(&included_uri).is_none(),
            "an abandoned analysis must not leave the include it read"
        );
        assert_eq!(resources.generation(), before);
    }

    #[test]
    fn closing_an_open_include_removes_only_its_open_root_role() {
        let root = TestDirectory::new();
        let generated = root.0.join("generated");
        std::fs::create_dir_all(&generated).expect("generated directory");
        std::fs::write(
            root.0.join(adocweave_config::FILE_NAME),
            concat!(
                "schema-version = 1\n",
                "[resources]\ninclude = true\nroots = [\".\"]\n",
                "[workspace.scan]\nexclude = [\"generated\"]\n",
            ),
        )
        .expect("project configuration");
        let source = root.0.join("root.adoc");
        let included = generated.join("part.adoc");
        std::fs::write(&source, "include::generated/part.adoc[]\n").expect("source");
        std::fs::write(&included, "included\n").expect("included source");
        let root_uri = Url::from_directory_path(&root.0).expect("root URI");
        let source_uri = Url::from_file_path(&source).expect("source URI");
        let included_uri = Url::from_file_path(&included).expect("included URI");
        let included_id = uri_id(&included_uri).expect("included ID");
        let mut resources = WorkspaceResources::default();
        resources.load_roots(&[root_uri]).expect("load workspace");

        analyze_and_apply(&mut resources, &source_uri)
            .expect("workspace analysis")
            .expect("adopted analysis");
        resources
            .upsert_open(included_uri.clone(), 1, "open include\n")
            .expect("open include");
        assert!(resources.inner.roots().contains(&included_id));

        resources.close_open(&included_uri).expect("close include");

        assert!(resources.get(&included_uri).is_some());
        assert!(resources.include_interests.contains(&included_id));
        assert!(!resources.inner.roots().contains(&included_id));
    }

    #[test]
    fn closing_an_open_scan_root_preserves_its_scan_root_role() {
        let root = TestDirectory::new();
        let source = root.0.join("root.adoc");
        std::fs::write(&source, "disk source\n").expect("source");
        let root_uri = Url::from_directory_path(&root.0).expect("root URI");
        let source_uri = Url::from_file_path(&source).expect("source URI");
        let source_id = uri_id(&source_uri).expect("source ID");
        let mut resources = WorkspaceResources::default();
        resources.load_roots(&[root_uri]).expect("load workspace");

        resources
            .upsert_open(source_uri.clone(), 1, "open source\n")
            .expect("open scan root");
        resources.close_open(&source_uri).expect("close scan root");

        assert!(resources.inner.roots().contains(&source_id));
        assert_eq!(
            resources.analysis_root_roles.get(&source_id),
            Some(&AnalysisRootRoles {
                scan_root: true,
                open_overlay: false,
            })
        );
        assert_eq!(
            resources
                .get(&source_uri)
                .expect("retained disk source")
                .text()
                .as_ref(),
            "disk source\n"
        );
    }

    #[test]
    fn failed_initial_include_read_keeps_a_bounded_watch_interest() {
        let root = TestDirectory::new();
        let generated = root.0.join("generated");
        std::fs::create_dir_all(&generated).expect("generated directory");
        std::fs::write(
            root.0.join(adocweave_config::FILE_NAME),
            concat!(
                "schema-version = 1\n",
                "[resources]\ninclude = true\nroots = [\".\"]\n",
                "[workspace.scan]\nexclude = [\"generated\"]\n",
            ),
        )
        .expect("project configuration");
        let source = root.0.join("root.adoc");
        let included = generated.join("part.txt");
        std::fs::write(&source, "include::generated/part.txt[]\n").expect("source");
        std::fs::write(&included, [0xff]).expect("invalid include");
        let root_uri = Url::from_directory_path(&root.0).expect("root URI");
        let source_uri = Url::from_file_path(&source).expect("source URI");
        let included_uri = Url::from_file_path(&included).expect("included URI");
        let included_id = uri_id(&included_uri).expect("included ID");
        let mut resources = WorkspaceResources::default();
        resources.load_roots(&[root_uri]).expect("load workspace");

        let analyzed = analyze_root(&mut resources, &source_uri).expect("workspace analysis");
        assert!(
            resources
                .apply_analyzed_root(analyzed)
                .expect("apply failed analysis")
                .is_none(),
            "an unreadable include must not produce a published analysis"
        );

        assert!(resources.include_interests.contains(&included_id));
        assert!(
            resources
                .include_dependencies
                .get(&uri_id(&source_uri).expect("source ID"))
                .is_some_and(|dependencies| dependencies.contains(&included_id)),
            "a failed read must keep the document waiting for the repair"
        );

        std::fs::write(&included, "repaired\n").expect("repair include");
        let update = resources
            .apply_watched_file(included_uri.clone(), WatchedFileKind::Upsert)
            .expect("reload repaired include");
        assert!(update.affected.contains(source_uri.as_str()));
        assert_eq!(
            resources
                .get(&included_uri)
                .expect("include")
                .text()
                .as_ref(),
            "repaired\n"
        );
        assert!(!resources.inner.roots().contains(&included_id));
    }

    #[test]
    fn created_excluded_adoc_include_recovers_without_a_scan_root_role() {
        let root = TestDirectory::new();
        let generated = root.0.join("generated");
        std::fs::create_dir_all(&generated).expect("generated directory");
        std::fs::write(
            root.0.join(adocweave_config::FILE_NAME),
            concat!(
                "schema-version = 1\n",
                "[resources]\ninclude = true\nroots = [\".\"]\n",
                "[workspace.scan]\nexclude = [\"generated\"]\n",
            ),
        )
        .expect("project configuration");
        let source = root.0.join("root.adoc");
        let included = generated.join("part.adoc");
        std::fs::write(&source, "include::generated/part.adoc[]\n").expect("source");
        let root_uri = Url::from_directory_path(&root.0).expect("root URI");
        let source_uri = Url::from_file_path(&source).expect("source URI");
        let included_uri = Url::from_file_path(&included).expect("included URI");
        let included_id = uri_id(&included_uri).expect("included ID");
        let mut resources = WorkspaceResources::default();
        resources.load_roots(&[root_uri]).expect("load workspace");

        let analyzed = analyze_root(&mut resources, &source_uri).expect("workspace analysis");
        resources
            .apply_analyzed_root(analyzed)
            .expect("apply analysis with a missing include");
        assert!(
            resources.get(&included_uri).is_none(),
            "the include does not exist yet"
        );

        std::fs::write(&included, "created\n").expect("create include");
        let update = resources
            .apply_watched_file(included_uri.clone(), WatchedFileKind::Upsert)
            .expect("load created include");

        assert!(update.affected.contains(source_uri.as_str()));
        assert!(resources.get(&included_uri).is_some());
        assert!(!resources.inner.roots().contains(&included_id));
        assert_eq!(
            resources.analysis_root_roles.get(&included_id),
            None,
            "include interest must not imply a scan root role"
        );
    }

    #[test]
    fn missing_include_interests_share_the_dependency_count_limit() {
        let root = TestDirectory::new();
        std::fs::write(
            root.0.join(adocweave_config::FILE_NAME),
            "schema-version = 1\n[resources]\ninclude = true\nroots = [\".\"]\n",
        )
        .expect("project configuration");
        let source = root.0.join("root.adoc");
        std::fs::write(&source, "include::missing.txt[]\n").expect("source");
        let root_uri = Url::from_directory_path(&root.0).expect("root URI");
        let source_uri = Url::from_file_path(&source).expect("source URI");
        let target_uri = Url::from_file_path(root.0.join("missing.txt")).expect("target URI");
        let target = uri_id(&target_uri).expect("target ID");
        let mut resources = WorkspaceResources::default();
        resources.load_roots(&[root_uri]).expect("load workspace");
        resources.include_interests = (0..MAX_WATCHED_INCLUDE_RESOURCES)
            .map(|index| {
                ResourceId::new(format!("file:///retained/{index}.txt")).expect("interest ID")
            })
            .collect();
        resources.include_dependencies.insert(
            uri_id(&source_uri).expect("source ID"),
            resources.include_interests.clone(),
        );

        let error = analyze_root(&mut resources, &source_uri)
            .err()
            .expect("interest count limit");

        assert!(error.contains("include dependency limit"));
        assert_eq!(
            resources.include_interests.len(),
            MAX_WATCHED_INCLUDE_RESOURCES
        );
        assert!(!resources.include_interests.contains(&target));
    }

    #[test]
    fn excluded_unknown_watch_candidate_is_ignored_before_nested_config_is_read() {
        let root = TestDirectory::new();
        let generated = root.0.join("generated");
        std::fs::create_dir_all(&generated).expect("generated directory");
        std::fs::write(
            root.0.join(adocweave_config::FILE_NAME),
            "schema-version = 1\n[workspace.scan]\nexclude = [\"generated\"]\n",
        )
        .expect("root configuration");
        std::fs::write(
            generated.join(adocweave_config::FILE_NAME),
            "schema-version = 99\n",
        )
        .expect("invalid nested configuration");
        let hidden = generated.join("hidden.adoc");
        std::fs::write(&hidden, "hidden\n").expect("hidden source");
        let root_uri = Url::from_directory_path(&root.0).expect("root URI");
        let hidden_uri = Url::from_file_path(&hidden).expect("hidden URI");
        let mut resources = WorkspaceResources::default();
        resources.load_roots(&[root_uri]).expect("load workspace");

        let update = resources
            .apply_watched_file(hidden_uri.clone(), WatchedFileKind::Upsert)
            .expect("ignore excluded candidate");

        assert!(!update.journal_relevant);
        assert!(resources.get(&hidden_uri).is_none());
    }

    #[test]
    #[cfg(unix)]
    fn recursive_scan_exclusion_prunes_a_non_utf8_subtree_before_reading_it() {
        use std::ffi::OsString;
        use std::os::unix::ffi::OsStringExt;

        let root = TestDirectory::new();
        std::fs::write(
            root.0.join(adocweave_config::FILE_NAME),
            "schema-version = 1\n[workspace.scan]\nexclude = [\"**\"]\n",
        )
        .expect("project configuration");
        let opaque = root.0.join(OsString::from_vec(vec![b'n', 0x80]));
        std::fs::create_dir(&opaque).expect("non-UTF-8 directory");
        std::fs::write(opaque.join("invalid.adoc"), [0xff]).expect("invalid source");
        let root_uri = Url::from_directory_path(&root.0).expect("root URI");
        let mut resources = WorkspaceResources::default();

        resources
            .load_roots(&[root_uri])
            .expect("excluded subtree is not read");
        assert!(resources.inner.roots().is_empty());
    }

    #[test]
    fn each_directory_root_uses_only_its_own_scan_patterns() {
        let first = TestDirectory::new();
        let second = TestDirectory::new();
        for (root, excluded, retained) in [
            (&first, "first-only", "second-only"),
            (&second, "second-only", "first-only"),
        ] {
            std::fs::write(
                root.0.join(adocweave_config::FILE_NAME),
                format!("schema-version = 1\n[workspace.scan]\nexclude = [\"{excluded}\"]\n"),
            )
            .expect("project configuration");
            std::fs::create_dir(root.0.join(excluded)).expect("excluded directory");
            std::fs::create_dir(root.0.join(retained)).expect("retained directory");
            std::fs::write(root.0.join(excluded).join("hidden.adoc"), "hidden\n")
                .expect("excluded source");
            std::fs::write(root.0.join(retained).join("kept.adoc"), "kept\n")
                .expect("retained source");
        }
        let roots =
            [&first, &second].map(|root| Url::from_directory_path(&root.0).expect("root URI"));
        let expected = BTreeSet::from([
            uri_id(
                &Url::from_file_path(first.0.join("second-only/kept.adoc"))
                    .expect("first retained URI"),
            )
            .expect("first retained ID"),
            uri_id(
                &Url::from_file_path(second.0.join("first-only/kept.adoc"))
                    .expect("second retained URI"),
            )
            .expect("second retained ID"),
        ]);
        let mut resources = WorkspaceResources::default();

        resources.load_roots(&roots).expect("load workspaces");

        assert_eq!(resources.inner.roots(), &expected);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn one_root_authority_covers_configuration_scan_and_document_read() {
        let root = TestDirectory::new();
        let document = root.0.join("root.adoc");
        std::fs::write(
            root.0.join(adocweave_config::FILE_NAME),
            "schema-version = 1\n",
        )
        .expect("trusted configuration");
        std::fs::write(&document, "= Trusted\n").expect("trusted document");
        let root_uri = Url::from_directory_path(&root.0).expect("root URI");
        let document_uri = Url::from_file_path(&document).expect("document URI");
        let displaced = root.0.with_extension("anchored-workspace");
        let mut resources = WorkspaceResources::default();

        let loaded = resources.load_roots_with_limits_after_authority(
            std::slice::from_ref(&root_uri),
            adapter_managed_workspace_limits(),
            &NeverCancel,
            || {
                std::fs::rename(&root.0, &displaced).expect("displace trusted workspace");
                std::fs::create_dir(&root.0).expect("replacement workspace");
                std::fs::write(
                    root.0.join(adocweave_config::FILE_NAME),
                    "schema-version = 99\n",
                )
                .expect("replacement configuration");
                std::fs::write(root.0.join("root.adoc"), "= Replacement\n")
                    .expect("replacement document");
            },
        );

        std::fs::remove_dir_all(&root.0).expect("remove replacement workspace");
        std::fs::rename(&displaced, &root.0).expect("restore trusted workspace");
        loaded.expect("load through retained authority");
        assert_eq!(
            resources
                .resource_text(&document_uri)
                .expect("trusted resource")
                .as_ref(),
            "= Trusted\n",
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn retained_root_covers_reload_open_and_missing_include_after_replacement() {
        let root = TestDirectory::new();
        let generated = root.0.join("generated");
        std::fs::create_dir(&generated).expect("generated directory");
        std::fs::write(
            root.0.join(adocweave_config::FILE_NAME),
            concat!(
                "schema-version = 1\n",
                "[resources]\ninclude = true\nroots = [\".\"]\n",
                "[workspace.scan]\nexclude = [\"generated\"]\n",
            ),
        )
        .expect("trusted configuration");
        let document = root.0.join("root.adoc");
        let included = generated.join("part.txt");
        std::fs::write(&document, "include::generated/part.txt[]\n").expect("trusted document");
        std::fs::write(&included, "trusted include\n").expect("trusted include");
        let root_uri = Url::from_directory_path(&root.0).expect("root URI");
        let document_uri = Url::from_file_path(&document).expect("document URI");
        let _include_uri = Url::from_file_path(&included).expect("include URI");
        let mut resources = WorkspaceResources::default();
        resources
            .load_roots(std::slice::from_ref(&root_uri))
            .expect("initial load");

        let displaced = root.0.with_extension("retained-reload");
        std::fs::rename(&root.0, &displaced).expect("displace trusted workspace");
        std::fs::create_dir_all(root.0.join("generated")).expect("replacement workspace");
        std::fs::write(
            root.0.join(adocweave_config::FILE_NAME),
            "schema-version = 99\n",
        )
        .expect("replacement configuration");
        std::fs::write(&document, "replacement document\n").expect("replacement document");
        std::fs::write(root.0.join("generated/part.txt"), "replacement include\n")
            .expect("replacement include");
        std::fs::write(
            displaced.join("root.adoc"),
            "include::generated/part.txt[]\ntrusted reload\n",
        )
        .expect("trusted reload");

        resources
            .reload_file(document_uri.clone())
            .expect("reload through retained root");
        assert!(
            resources
                .resource_text(&document_uri)
                .expect("reloaded resource")
                .contains("trusted reload")
        );
        resources
            .upsert_open(
                document_uri.clone(),
                1,
                "include::generated/part.txt[]\noverlay\n",
            )
            .expect("open through retained configuration");
        let analysis = analyze_and_apply(&mut resources, &document_uri)
            .expect("workspace analysis")
            .expect("adopted analysis");
        assert!(analysis.analysis.source().contains("trusted include"));
        assert!(!analysis.analysis.source().contains("replacement include"));

        std::fs::remove_dir_all(&root.0).expect("remove replacement workspace");
        std::fs::rename(&displaced, &root.0).expect("restore trusted workspace");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn root_replacement_before_authority_fails_without_panicking() {
        use std::os::unix::fs::symlink;

        let root = TestDirectory::new();
        let outside = TestDirectory::new();
        std::fs::write(root.0.join("trusted.adoc"), "trusted\n").expect("trusted source");
        std::fs::write(outside.0.join("outside.adoc"), "outside\n").expect("outside source");
        let root_uri = Url::from_directory_path(&root.0).expect("root URI");
        let displaced = root.0.with_extension("before-authority");
        let mut resources = WorkspaceResources::default();

        let loaded = resources.load_roots_with_limits_after_hooks(
            std::slice::from_ref(&root_uri),
            adapter_managed_workspace_limits(),
            &NeverCancel,
            || {
                std::fs::rename(&root.0, &displaced).expect("displace trusted workspace");
                symlink(&outside.0, &root.0).expect("redirect workspace root");
            },
            || {},
        );

        std::fs::remove_file(&root.0).expect("remove replacement symlink");
        std::fs::rename(&displaced, &root.0).expect("restore trusted workspace");
        let error = loaded.expect_err("changed authority must fail closed");
        assert!(
            error.contains("workspace root changed while its filesystem authority was established")
        );
        assert!(resources.inner.roots().is_empty());
        assert!(resources.last_load_failed_closed());
    }

    #[test]
    fn nested_directory_root_applies_its_own_scan_patterns() {
        let outer = TestDirectory::new();
        let inner = outer.0.join("nested");
        let excluded = inner.join("generated");
        std::fs::create_dir_all(&excluded).expect("excluded directory");
        std::fs::write(
            inner.join(adocweave_config::FILE_NAME),
            "schema-version = 1\n[workspace.scan]\nexclude = [\"generated\"]\n",
        )
        .expect("inner project configuration");
        std::fs::write(excluded.join("hidden.adoc"), "hidden\n").expect("excluded source");
        let roots = [
            Url::from_directory_path(&outer.0).expect("outer root URI"),
            Url::from_directory_path(&inner).expect("inner root URI"),
        ];
        let hidden =
            uri_id(&Url::from_file_path(excluded.join("hidden.adoc")).expect("hidden source URI"))
                .expect("hidden source ID");
        let mut resources = WorkspaceResources::default();

        resources
            .load_roots(&roots)
            .expect("load nested workspaces");

        assert!(!resources.inner.roots().contains(&hidden));
    }

    #[test]
    fn single_file_root_registers_only_the_selected_document() {
        let root = TestDirectory::new();
        let selected = root.0.join("selected.adoc");
        let included = root.0.join("included.adoc");
        let unrelated = root.0.join("unrelated.adoc");
        std::fs::write(&selected, "include::included.adoc[]\n").expect("selected source");
        std::fs::write(&included, "included\n").expect("included source");
        std::fs::write(&unrelated, "unrelated\n").expect("unrelated source");
        let selected_uri = Url::from_file_path(&selected).expect("selected URI");
        let included_uri = Url::from_file_path(&included).expect("included URI");
        let unrelated_uri = Url::from_file_path(&unrelated).expect("unrelated URI");
        let mut resources = WorkspaceResources::default();

        resources
            .load_roots(std::slice::from_ref(&selected_uri))
            .expect("load single-file workspace");

        assert_eq!(
            resources.inner.roots(),
            &BTreeSet::from([uri_id(&selected_uri).expect("selected resource ID")])
        );
        assert!(resources.get(&included_uri).is_none());
        assert!(resources.get(&unrelated_uri).is_none());
        resources
            .reload_file(unrelated_uri.clone())
            .expect("ignore unrelated resource");
        assert!(resources.get(&unrelated_uri).is_none());

        assert!(resources.input(&selected_uri).is_ok());
    }

    #[test]
    fn directory_root_supersedes_a_nested_single_file_root() {
        let root = TestDirectory::new();
        let nested = root.0.join("docs");
        std::fs::create_dir_all(&nested).expect("nested directory");
        let first = nested.join("first.adoc");
        let second = nested.join("second.adoc");
        std::fs::write(&first, "first\n").expect("first source");
        std::fs::write(&second, "second\n").expect("second source");
        let directory_uri = Url::from_directory_path(&root.0).expect("directory URI");
        let first_uri = Url::from_file_path(&first).expect("first URI");
        let mut resources = WorkspaceResources::default();

        resources
            .load_roots(&[directory_uri, first_uri])
            .expect("load mixed roots");

        assert!(resources.single_file_roots.is_empty());
        assert_eq!(
            resources.roots,
            vec![root.0.canonicalize().expect("canonical directory")]
        );
        assert_eq!(resources.inner.roots().len(), 2);
    }

    #[test]
    fn resolved_default_plan_names_each_budget_domain() {
        let plan = adocweave_config::ResolvedResourceLimitPlan::default();
        assert_eq!(
            plan.filesystem_reads,
            adocweave_host::FilesystemReadLimits::default()
        );
        assert_eq!(
            plan.retained_layers,
            adocweave_workspace::RetainedResourceLimits::default()
        );
        assert_eq!(
            plan.analysis_snapshot.max_resources,
            plan.filesystem_reads.max_files
        );
    }

    #[test]
    fn watched_file_reload_reads_the_new_filesystem_snapshot() {
        let root = TestDirectory::new();
        let path = root.0.join("document.adoc");
        std::fs::write(&path, "first\n").expect("initial source");
        let root_uri = Url::from_directory_path(&root.0).expect("root URI");
        let document_uri = Url::from_file_path(&path).expect("document URI");
        let mut resources = WorkspaceResources::default();
        resources.load_roots(&[root_uri]).expect("load workspace");
        assert_eq!(
            resources
                .get(&document_uri)
                .expect("initial resource")
                .text()
                .as_ref(),
            "first\n"
        );

        std::fs::write(&path, "second\n").expect("updated source");
        resources
            .reload_file(document_uri.clone())
            .expect("reload resource");

        assert_eq!(
            resources
                .get(&document_uri)
                .expect("updated resource")
                .text()
                .as_ref(),
            "second\n"
        );
    }

    #[test]
    fn watched_file_reloads_share_the_workspace_filesystem_budget() {
        let root = TestDirectory::new();
        let first = root.0.join("first.adoc");
        let second = root.0.join("second.adoc");
        std::fs::write(&first, "first\n").expect("initial source");
        let root_uri = Url::from_directory_path(&root.0).expect("root URI");
        let mut resources = WorkspaceResources::default();
        let mut limits = WorkspaceLimits::default();
        limits.resources.max_files = 1;
        resources
            .load_roots_with_limits(&[root_uri], limits, &NeverCancel)
            .expect("load workspace");

        std::fs::write(&second, "second\n").expect("new source");
        let second_uri = Url::from_file_path(&second).expect("document URI");
        let error = resources
            .reload_file(second_uri)
            .expect_err("shared file limit");

        assert!(error.contains("file limit"), "{error}");
    }

    #[test]
    fn removing_a_disk_resource_releases_its_filesystem_charge() {
        let root = TestDirectory::new();
        let first = root.0.join("first.adoc");
        let second = root.0.join("second.adoc");
        std::fs::write(&first, "first\n").expect("initial source");
        let root_uri = Url::from_directory_path(&root.0).expect("root URI");
        let first_uri = Url::from_file_path(&first).expect("first URI");
        let mut resources = WorkspaceResources::default();
        let mut limits = WorkspaceLimits::default();
        limits.resources.max_files = 1;
        resources
            .load_roots_with_limits(&[root_uri], limits, &NeverCancel)
            .expect("load workspace");

        std::fs::remove_file(&first).expect("remove first");
        resources.remove_disk(&first_uri).expect("remove disk");
        std::fs::write(&second, "second\n").expect("new source");
        let second_uri = Url::from_file_path(&second).expect("second URI");
        resources
            .reload_file(second_uri.clone())
            .expect("released file charge");

        assert_eq!(
            resources
                .get(&second_uri)
                .expect("second resource")
                .text()
                .as_ref(),
            "second\n"
        );
    }

    #[test]
    fn nearest_project_plan_rejects_an_oversized_disk_resource_before_ingest() {
        let root = TestDirectory::new();
        write_resource_config(&root.0, 2, 8, 4, false);
        let path = root.0.join("document.adoc");
        std::fs::write(&path, "12345").expect("source");
        let root_uri = Url::from_directory_path(&root.0).expect("root URI");
        let document_uri = Url::from_file_path(&path).expect("document URI");
        let mut resources = WorkspaceResources::default();

        let error = resources
            .load_roots(&[root_uri])
            .expect_err("strict disk limit");

        assert!(error.contains("too large"), "{error}");
        assert!(resources.get(&document_uri).is_none());
    }

    #[test]
    fn separate_project_sessions_and_retained_budgets_do_not_compete() {
        let root = TestDirectory::new();
        let first = root.0.join("first");
        let second = root.0.join("second");
        std::fs::create_dir(&first).expect("first project");
        std::fs::create_dir(&second).expect("second project");
        write_resource_config(&first, 1, 4, 4, false);
        write_resource_config(&second, 1, 4, 4, false);
        let first_path = first.join("document.adoc");
        let second_path = second.join("document.adoc");
        std::fs::write(&first_path, "one").expect("first source");
        std::fs::write(&second_path, "two").expect("second source");
        let root_uri = Url::from_directory_path(&root.0).expect("root URI");
        let first_uri = Url::from_file_path(first_path).expect("first URI");
        let second_uri = Url::from_file_path(second_path).expect("second URI");
        let mut resources = WorkspaceResources::default();

        resources.load_roots(&[root_uri]).expect("load workspace");

        assert!(resources.get(&first_uri).is_some());
        assert!(resources.get(&second_uri).is_some());
        assert_eq!(resources.filesystems.len(), 2);
        assert_eq!(resources.retained_layers.len(), 2);
    }

    #[test]
    fn unconfigured_workspace_roots_have_independent_scopes() {
        let first = TestDirectory::new();
        let second = TestDirectory::new();
        std::fs::write(first.0.join("document.adoc"), "one").expect("first source");
        std::fs::write(second.0.join("document.adoc"), "two").expect("second source");
        let mut resources = WorkspaceResources::default();

        resources
            .load_roots(&[
                Url::from_directory_path(&first.0).expect("first root"),
                Url::from_directory_path(&second.0).expect("second root"),
            ])
            .expect("load roots");

        assert_eq!(resources.filesystems.len(), 2);
        assert_eq!(resources.retained_layers.len(), 2);
        assert!(
            resources
                .project_plans
                .keys()
                .all(|scope| scope.config_path.is_none())
        );
    }

    #[test]
    fn configless_multi_root_input_excludes_an_include_from_another_scope() {
        let root = TestDirectory::new();
        let second = root.0.join("second");
        std::fs::create_dir(&second).expect("second root");
        let first_path = root.0.join("document.adoc");
        let second_path = second.join("private.adoc");
        std::fs::write(&first_path, "include::second/private.adoc[]\n").expect("first source");
        std::fs::write(&second_path, "private\n").expect("second source");
        let first_uri = Url::from_file_path(&first_path).expect("first URI");
        let second_id = ResourceId::new(
            Url::from_file_path(&second_path)
                .expect("second URI")
                .as_str(),
        )
        .expect("second ID");
        let mut resources = WorkspaceResources::default();
        resources
            .load_roots(&[
                Url::from_directory_path(&root.0).expect("first root URI"),
                Url::from_directory_path(&second).expect("second root URI"),
            ])
            .expect("load roots");

        let input = resources.input(&first_uri).expect("first input");
        assert!(input.options.enable_includes);
        assert_eq!(input.snapshot.resources().count(), 1);
        assert!(input.snapshot.get(&second_id).is_none());
        let analyzed = analyze_root(&mut resources, &first_uri).expect("workspace analysis");

        // The include is refused by the root's authority, so the run answers
        // that the resource is absent rather than leaving the preprocessor to
        // fail on a lookup it cannot complete. The classification names what
        // actually happened: the resource is not available to this root.
        assert_eq!(
            analyzed
                .failure()
                .expect("cross-scope include is unavailable")
                .code,
            adocweave_workspace::WorkspaceErrorCode::MissingResource.as_str()
        );
    }

    #[test]
    fn configured_multi_root_without_explicit_roots_excludes_another_workspace_root() {
        let first = TestDirectory::new();
        let second = TestDirectory::new();
        std::fs::write(
            first.0.join(adocweave_config::FILE_NAME),
            "schema-version = 1\n[resources]\ninclude = true\nroots = []\n",
        )
        .expect("first config");
        let first_path = first.0.join("document.adoc");
        let second_path = second.0.join("private.adoc");
        std::fs::write(&first_path, "first").expect("first source");
        std::fs::write(&second_path, "private").expect("second source");
        let first_uri = Url::from_file_path(&first_path).expect("first URI");
        let second_id =
            uri_id(&Url::from_file_path(&second_path).expect("second URI")).expect("second ID");
        let mut resources = WorkspaceResources::default();
        resources
            .load_roots(&[
                Url::from_directory_path(&first.0).expect("first root URI"),
                Url::from_directory_path(&second.0).expect("second root URI"),
            ])
            .expect("load roots");

        let input = resources.input(&first_uri).expect("first input");
        assert!(input.options.enable_includes);
        assert_eq!(input.snapshot.resources().count(), 1);
        assert!(input.snapshot.get(&second_id).is_none());
    }

    #[test]
    fn open_outside_configured_roots_preserves_workspace_and_budgets() {
        let root = TestDirectory::new();
        let docs = root.0.join("docs");
        let other = root.0.join("other");
        std::fs::create_dir(&docs).expect("docs");
        std::fs::create_dir(&other).expect("other");
        std::fs::write(
            root.0.join(adocweave_config::FILE_NAME),
            "schema-version = 1\n[resources]\nroots = [\"docs\"]\n",
        )
        .expect("config");
        let accepted = docs.join("accepted.adoc");
        let rejected = other.join("rejected.adoc");
        std::fs::write(&accepted, "accepted").expect("accepted source");
        std::fs::write(&rejected, "rejected").expect("rejected source");
        let rejected_uri = Url::from_file_path(&rejected).expect("rejected URI");
        let rejected_id = uri_id(&rejected_uri).expect("rejected ID");
        let mut resources = WorkspaceResources::default();
        resources
            .load_roots(&[Url::from_directory_path(&root.0).expect("root URI")])
            .expect("load root");
        let generation = resources.generation();
        let projects = resources.resource_projects.clone();
        let budgets = resources.retained_layers.clone();

        let error = resources
            .upsert_open(rejected_uri, 1, "open")
            .expect_err("outside authority");
        assert!(
            error.contains("outside configured resource roots"),
            "{error}"
        );
        assert_eq!(resources.generation(), generation);
        assert_eq!(resources.resource_projects, projects);
        assert!(
            resources
                .get(&Url::from_file_path(rejected).expect("URI"))
                .is_none()
        );
        assert!(!resources.resource_projects.contains_key(&rejected_id));
        assert_eq!(resources.retained_layers, budgets);
    }

    #[test]
    fn project_migration_releases_the_previous_scope_and_collects_it() {
        let root = TestDirectory::new();
        let nested = root.0.join("nested");
        std::fs::create_dir(&nested).expect("nested");
        let path = nested.join("document.adoc");
        std::fs::write(&path, "old").expect("source");
        let root_uri = Url::from_directory_path(&root.0).expect("root URI");
        let document_uri = Url::from_file_path(&path).expect("document URI");
        let mut resources = WorkspaceResources::default();
        resources.load_roots(&[root_uri]).expect("load workspace");
        let previous_scope = resources
            .resource_projects
            .get(&uri_id(&document_uri).expect("resource ID"))
            .cloned()
            .expect("previous scope");

        write_resource_config(&nested, 1, 8, 8, false);
        std::fs::write(&path, "new").expect("new source");
        resources
            .reload_file(document_uri.clone())
            .expect("migrate project");

        let current_scope = resources
            .resource_projects
            .get(&uri_id(&document_uri).expect("resource ID"))
            .expect("current scope");
        assert_ne!(current_scope, &previous_scope);
        assert!(!resources.filesystems.contains_key(&previous_scope));
        assert!(!resources.retained_layers.contains_key(&previous_scope));
        assert!(!resources.project_plans.contains_key(&previous_scope));
        assert_eq!(
            resources
                .get(&document_uri)
                .expect("migrated")
                .text()
                .as_ref(),
            "new"
        );
    }

    #[test]
    fn failed_project_migration_preserves_every_committed_layer() {
        let root = TestDirectory::new();
        let nested = root.0.join("nested");
        std::fs::create_dir(&nested).expect("nested");
        let path = nested.join("document.adoc");
        std::fs::write(&path, "old").expect("source");
        let root_uri = Url::from_directory_path(&root.0).expect("root URI");
        let document_uri = Url::from_file_path(&path).expect("document URI");
        let mut resources = WorkspaceResources::default();
        resources.load_roots(&[root_uri]).expect("load workspace");
        let id = uri_id(&document_uri).expect("resource ID");
        let previous_scope = resources
            .resource_projects
            .get(&id)
            .cloned()
            .expect("previous scope");
        let previous_generation = resources.generation();
        let previous_budget = resources
            .filesystems
            .get(&previous_scope)
            .expect("filesystem")
            .lock()
            .expect("lock")
            .budget();

        write_resource_config(&nested, 1, 2, 2, false);
        std::fs::write(&path, "oversized").expect("oversized source");
        resources
            .reload_file(document_uri.clone())
            .expect_err("migration limit");

        assert_eq!(resources.generation(), previous_generation);
        assert_eq!(resources.resource_projects.get(&id), Some(&previous_scope));
        assert_eq!(
            resources
                .get(&document_uri)
                .expect("old state")
                .text()
                .as_ref(),
            "old"
        );
        assert_eq!(
            resources
                .filesystems
                .get(&previous_scope)
                .expect("filesystem")
                .lock()
                .expect("lock")
                .budget(),
            previous_budget
        );
        assert_eq!(resources.filesystems.len(), 1);
        assert_eq!(resources.retained_layers.len(), 1);
    }

    #[test]
    fn failed_overlay_registration_is_atomic_across_workspace_and_budget() {
        let root = TestDirectory::new();
        let disk = root.0.join("disk.adoc");
        let overlay = root.0.join("overlay.adoc");
        std::fs::write(&disk, "disk").expect("disk source");
        let mut resources = WorkspaceResources::default();
        resources
            .load_roots_with_limits(
                &[Url::from_directory_path(&root.0).expect("root URI")],
                WorkspaceLimits {
                    resources: RetainedResourceLimits {
                        max_files: usize::MAX,
                        max_total_bytes: u64::MAX,
                        max_resource_bytes: u64::MAX,
                    },
                    max_roots: 1,
                },
                &NeverCancel,
            )
            .expect("load workspace");
        let overlay_uri = Url::from_file_path(overlay).expect("overlay URI");
        let previous_generation = resources.generation();
        let previous_retained = resources.retained_layers.clone();

        resources
            .upsert_open(overlay_uri.clone(), 1, "open")
            .expect_err("root limit");

        assert_eq!(resources.generation(), previous_generation);
        assert!(resources.get(&overlay_uri).is_none());
        assert_eq!(resources.retained_layers.len(), previous_retained.len());
        assert!(
            !resources
                .resource_projects
                .contains_key(&uri_id(&overlay_uri).expect("resource ID"))
        );
    }

    #[test]
    fn valid_stricter_reload_clears_disk_and_open_overlay() {
        let root = TestDirectory::new();
        write_resource_config(&root.0, 1, 8, 8, false);
        let path = root.0.join("document.adoc");
        std::fs::write(&path, "disk").expect("disk source");
        let root_uri = Url::from_directory_path(&root.0).expect("root URI");
        let document_uri = Url::from_file_path(&path).expect("document URI");
        let mut resources = WorkspaceResources::default();
        resources
            .load_roots(std::slice::from_ref(&root_uri))
            .expect("load workspace");
        resources
            .upsert_open(document_uri.clone(), 1, "open")
            .expect("open overlay");
        write_resource_config(&root.0, 1, 4, 4, false);
        resources
            .reload_roots_with_open_sources(
                &[root_uri],
                &[(document_uri.clone(), 2, Arc::from("too large"))],
            )
            .expect_err("overlay limit");

        assert!(resources.last_load_failed_closed());
        assert!(resources.get(&document_uri).is_none());
    }

    #[test]
    fn retained_layer_plan_rejects_overlay_bytes_before_workspace_ingest() {
        let root = TestDirectory::new();
        write_resource_config(&root.0, 1, 3, 3, false);
        let path = root.0.join("document.adoc");
        std::fs::write(&path, "a\n").expect("source");
        let root_uri = Url::from_directory_path(&root.0).expect("root URI");
        let document_uri = Url::from_file_path(&path).expect("document URI");
        let mut resources = WorkspaceResources::default();
        resources.load_roots(&[root_uri]).expect("load workspace");

        let error = resources
            .upsert_open(document_uri.clone(), 1, "b\n")
            .expect_err("disk and overlay byte limit");

        assert!(error.contains("retained resource byte"), "{error}");
        assert_eq!(
            resources
                .get(&document_uri)
                .expect("disk layer remains effective")
                .text()
                .as_ref(),
            "a\n"
        );
    }

    #[test]
    fn configured_read_charge_is_released_before_a_new_reload() {
        let root = TestDirectory::new();
        write_resource_config(&root.0, 1, 8, 8, false);
        let first = root.0.join("first.adoc");
        let second = root.0.join("second.adoc");
        std::fs::write(&first, "first\n").expect("first source");
        let root_uri = Url::from_directory_path(&root.0).expect("root URI");
        let first_uri = Url::from_file_path(&first).expect("first URI");
        let second_uri = Url::from_file_path(&second).expect("second URI");
        let mut resources = WorkspaceResources::default();
        resources.load_roots(&[root_uri]).expect("load workspace");

        std::fs::remove_file(first).expect("remove first source");
        resources.remove_disk(&first_uri).expect("remove disk");
        std::fs::write(&second, "new\n").expect("second source");
        resources
            .reload_file(second_uri.clone())
            .expect("released read and retention charge");

        assert_eq!(
            resources
                .get(&second_uri)
                .expect("reloaded resource")
                .text()
                .as_ref(),
            "new\n"
        );
    }

    #[test]
    fn changed_project_plan_is_rejected_before_the_existing_session_reads() {
        let root = TestDirectory::new();
        write_resource_config(&root.0, 2, 8, 8, false);
        let first = root.0.join("first.adoc");
        let second = root.0.join("second.adoc");
        std::fs::write(&first, "a").expect("first source");
        let root_uri = Url::from_directory_path(&root.0).expect("root URI");
        let first_uri = Url::from_file_path(&first).expect("first URI");
        let second_uri = Url::from_file_path(&second).expect("second URI");
        let mut resources = WorkspaceResources::default();
        resources.load_roots(&[root_uri]).expect("load workspace");

        write_resource_config(&root.0, 2, 1, 1, false);
        std::fs::write(&first, "bb").expect("oversized replacement");
        let error = resources
            .reload_file(first_uri)
            .expect_err("changed plan requires full reload");
        assert!(error.contains("full reload"), "{error}");

        write_resource_config(&root.0, 2, 8, 8, false);
        std::fs::write(&second, "1234567").expect("second source");
        resources
            .reload_file(second_uri)
            .expect("rejected reread did not consume the old session budget");
    }

    #[test]
    fn retained_byte_rejection_rolls_back_replaced_filesystem_charge() {
        let root = TestDirectory::new();
        write_resource_config(&root.0, 2, 4, 4, false);
        let first = root.0.join("first.adoc");
        let second = root.0.join("second.adoc");
        std::fs::write(&first, "a").expect("first source");
        let root_uri = Url::from_directory_path(&root.0).expect("root URI");
        let first_uri = Url::from_file_path(&first).expect("first URI");
        let second_uri = Url::from_file_path(&second).expect("second URI");
        let mut resources = WorkspaceResources::default();
        resources.load_roots(&[root_uri]).expect("load workspace");
        resources
            .upsert_open(first_uri.clone(), 1, "xxx")
            .expect("overlay");

        std::fs::write(&first, "bb").expect("grown disk source");
        let error = resources
            .reload_file(first_uri.clone())
            .expect_err("disk and overlay exceed retained budget");
        assert!(error.contains("retained resource byte"), "{error}");
        resources.close_open(&first_uri).expect("close overlay");

        std::fs::write(&second, "yyy").expect("second source");
        resources
            .reload_file(second_uri)
            .expect("old filesystem charge was restored");
    }

    #[test]
    fn retained_count_rejection_rolls_back_a_new_filesystem_charge() {
        let root = TestDirectory::new();
        write_resource_config(&root.0, 1, 8, 8, false);
        let overlay = root.0.join("overlay.adoc");
        let rejected = root.0.join("rejected.adoc");
        let accepted = root.0.join("accepted.adoc");
        let root_uri = Url::from_directory_path(&root.0).expect("root URI");
        let overlay_uri = Url::from_file_path(&overlay).expect("overlay URI");
        let rejected_uri = Url::from_file_path(&rejected).expect("rejected URI");
        let accepted_uri = Url::from_file_path(&accepted).expect("accepted URI");
        let mut resources = WorkspaceResources::default();
        resources.load_roots(&[root_uri]).expect("load workspace");
        resources
            .upsert_open(overlay_uri.clone(), 1, "open")
            .expect("overlay");

        std::fs::write(&rejected, "disk").expect("rejected source");
        let error = resources
            .reload_file(rejected_uri)
            .expect_err("overlay already consumes retained count");
        assert!(error.contains("retained resource count"), "{error}");
        resources.close_open(&overlay_uri).expect("close overlay");

        std::fs::write(&accepted, "disk").expect("accepted source");
        resources
            .reload_file(accepted_uri)
            .expect("new filesystem charge was removed");
    }

    #[test]
    fn transient_configuration_read_failure_preserves_the_previous_snapshot() {
        let root = TestDirectory::new();
        write_resource_config(&root.0, 1, 8, 8, false);
        let path = root.0.join("document.adoc");
        std::fs::write(&path, "old").expect("source");
        let root_uri = Url::from_directory_path(&root.0).expect("root URI");
        let document_uri = Url::from_file_path(&path).expect("document URI");
        let mut resources = WorkspaceResources::default();
        resources
            .load_roots(std::slice::from_ref(&root_uri))
            .expect("load workspace");

        std::fs::remove_file(root.0.join(adocweave_config::FILE_NAME)).expect("remove config");
        std::fs::create_dir(root.0.join(adocweave_config::FILE_NAME))
            .expect("unreadable config path");
        let error = resources
            .load_roots(&[root_uri])
            .expect_err("configuration read failure");

        assert!(error.contains("read-failed"), "{error}");
        assert!(!resources.last_load_failed_closed());
        assert_eq!(
            resources
                .get(&document_uri)
                .expect("previous snapshot")
                .text()
                .as_ref(),
            "old"
        );
    }

    #[test]
    fn read_failure_after_fail_closed_reload_is_classified_for_the_current_attempt() {
        let root = TestDirectory::new();
        let config = root.0.join(adocweave_config::FILE_NAME);
        std::fs::write(&config, "schema-version = 99\n").expect("invalid config");
        std::fs::write(root.0.join("document.adoc"), "source").expect("source");
        let root_uri = Url::from_directory_path(&root.0).expect("root URI");
        let mut resources = WorkspaceResources::default();

        resources
            .reload_roots_with_open_sources(std::slice::from_ref(&root_uri), &[])
            .expect_err("invalid configuration");
        assert!(resources.last_load_failed_closed());
        let failed_closed_generation = resources.generation();

        std::fs::remove_file(&config).expect("remove invalid config");
        std::fs::create_dir(&config).expect("unreadable config path");
        let error = resources
            .reload_roots_with_open_sources(&[root_uri], &[])
            .expect_err("configuration read failure");

        assert!(error.contains("read-failed"), "{error}");
        assert!(!resources.last_load_failed_closed());
        assert_eq!(resources.generation(), failed_closed_generation);
    }

    #[test]
    fn read_failure_after_load_preserves_previous_view_before_invalid_failure_closes_it() {
        let root = TestDirectory::new();
        write_resource_config(&root.0, 2, 64, 64, false);
        let config = root.0.join(adocweave_config::FILE_NAME);
        let path = root.0.join("document.adoc");
        std::fs::write(&path, "disk").expect("source");
        let root_uri = Url::from_directory_path(&root.0).expect("root URI");
        let document_uri = Url::from_file_path(&path).expect("document URI");
        let mut resources = WorkspaceResources::default();
        resources
            .load_roots(std::slice::from_ref(&root_uri))
            .expect("initial load");
        resources
            .upsert_open(document_uri.clone(), 1, "previous overlay")
            .expect("initial overlay");
        let previous_generation = resources.generation();

        let error = resources
            .reload_roots_with_open_sources_after_load(
                std::slice::from_ref(&root_uri),
                &[(document_uri.clone(), 2, Arc::from("new overlay"))],
                || {
                    std::fs::remove_file(&config).expect("remove config");
                    std::fs::create_dir(&config).expect("make config unreadable");
                },
            )
            .expect_err("post-load configuration read failure");
        assert!(error.contains("read-failed"), "{error}");
        assert!(!resources.last_load_failed_closed());
        assert_eq!(resources.generation(), previous_generation);
        assert_eq!(
            resources
                .get(&document_uri)
                .expect("previous view")
                .text()
                .as_ref(),
            "previous overlay"
        );

        std::fs::remove_dir(&config).expect("remove unreadable config");
        std::fs::write(&config, "invalid = true\n").expect("invalid config");
        resources
            .reload_roots_with_open_sources(&[root_uri], &[])
            .expect_err("invalid configuration");
        assert!(resources.last_load_failed_closed());
        assert!(resources.get(&document_uri).is_none());
    }

    #[test]
    fn invalid_configuration_clears_state_and_rejects_new_input() {
        let root = TestDirectory::new();
        write_resource_config(&root.0, 1, 8, 8, false);
        let path = root.0.join("document.adoc");
        std::fs::write(&path, "old").expect("source");
        let root_uri = Url::from_directory_path(&root.0).expect("root URI");
        let document_uri = Url::from_file_path(&path).expect("document URI");
        let mut resources = WorkspaceResources::default();
        resources
            .load_roots(std::slice::from_ref(&root_uri))
            .expect("load workspace");

        std::fs::write(root.0.join(adocweave_config::FILE_NAME), "invalid = true\n")
            .expect("invalid config");
        resources
            .load_roots(&[root_uri])
            .expect_err("invalid configuration");

        assert!(resources.last_load_failed_closed());
        assert!(resources.get(&document_uri).is_none());
        assert!(resources.input(&document_uri).is_err());
    }

    #[test]
    fn initial_invalid_configuration_commits_an_empty_trusted_state() {
        let root = TestDirectory::new();
        std::fs::write(root.0.join("document.adoc"), "source").expect("source");
        std::fs::write(root.0.join(adocweave_config::FILE_NAME), "invalid = true\n")
            .expect("invalid config");
        let root_uri = Url::from_directory_path(&root.0).expect("root URI");
        let mut resources = WorkspaceResources::default();
        let generation = resources.generation();
        let next_disk_version = resources.next_disk_version;

        resources
            .load_roots(&[root_uri])
            .expect_err("invalid configuration");

        assert!(resources.generation() > generation);
        assert_eq!(resources.next_disk_version, next_disk_version);
        assert_eq!(resources.roots, vec![root.0.canonicalize().expect("root")]);
        assert!(resources.inner.roots().is_empty());
        assert!(resources.last_load_failed_closed());
        assert!(resources.filesystems.is_empty());
        assert!(resources.project_plans.is_empty());
        assert!(resources.resource_projects.is_empty());
        assert!(resources.retained_layers.is_empty());
    }

    #[test]
    fn failed_old_scope_release_rolls_back_reload_and_open_migrations() {
        for migrate_open in [false, true] {
            let root = TestDirectory::new();
            let nested = root.0.join("nested");
            std::fs::create_dir(&nested).expect("nested");
            let path = nested.join("document.adoc");
            std::fs::write(&path, "disk").expect("source");
            let root_uri = Url::from_directory_path(&root.0).expect("root URI");
            let document_uri = Url::from_file_path(&path).expect("document URI");
            let mut resources = WorkspaceResources::default();
            resources.load_roots(&[root_uri]).expect("load workspace");
            if migrate_open {
                resources
                    .upsert_open(document_uri.clone(), 1, "old overlay")
                    .expect("initial overlay");
            }
            let id = uri_id(&document_uri).expect("resource ID");
            let previous_scope = resources
                .resource_projects
                .get(&id)
                .cloned()
                .expect("previous scope");
            let previous_generation = resources.generation();
            let filesystem = Arc::clone(
                resources
                    .filesystems
                    .get(&previous_scope)
                    .expect("old filesystem"),
            );
            let _ = std::thread::spawn(move || {
                let _guard = filesystem.lock().expect("lock before poison");
                panic!("poison old scope");
            })
            .join();
            write_resource_config(&nested, 2, 64, 64, false);

            let error = if migrate_open {
                resources
                    .upsert_open(document_uri.clone(), 2, "new overlay")
                    .expect_err("old release failure")
            } else {
                std::fs::write(&path, "new disk").expect("changed source");
                resources
                    .reload_file(document_uri.clone())
                    .expect_err("old release failure")
            };

            assert!(error.contains("lock is poisoned"), "{error}");
            assert_eq!(resources.generation(), previous_generation);
            assert_eq!(resources.resource_projects.get(&id), Some(&previous_scope));
            assert_eq!(resources.filesystems.len(), 1);
            assert_eq!(
                resources
                    .get(&document_uri)
                    .expect("previous resource")
                    .text()
                    .as_ref(),
                if migrate_open { "old overlay" } else { "disk" }
            );
        }
    }

    #[test]
    fn analysis_snapshot_uses_the_root_documents_nearest_plan() {
        let root = TestDirectory::new();
        write_resource_config(&root.0, 1, 16, 16, true);
        let root_path = root.0.join("root.adoc");
        std::fs::write(&root_path, "root\n").expect("root source");
        let nested = root.0.join("nested");
        std::fs::create_dir(&nested).expect("nested directory");
        write_resource_config(&nested, 1, 16, 16, false);
        std::fs::write(nested.join("child.adoc"), "child\n").expect("child source");
        let root_uri = Url::from_directory_path(&root.0).expect("root URI");
        let document_uri = Url::from_file_path(&root_path).expect("document URI");
        let mut resources = WorkspaceResources::default();
        resources.load_roots(&[root_uri]).expect("load workspace");

        let error = resources
            .input(&document_uri)
            .expect_err("root snapshot count limit");

        assert!(error.contains("analysis snapshot"), "{error}");
    }

    #[test]
    fn shared_scope_fixture_has_the_same_root_and_include_count_contract() {
        let root = TestDirectory::new();
        write_resource_config(&root.0, 1, 64, 64, true);
        let root_path = root.0.join("root.adoc");
        std::fs::write(
            &root_path,
            include_bytes!("../../../fixtures/resource-limits/root-with-include.adoc"),
        )
        .expect("root source");
        std::fs::write(
            root.0.join("part.adoc"),
            include_bytes!("../../../fixtures/resource-limits/part.adoc"),
        )
        .expect("included source");
        let root_uri = Url::from_directory_path(&root.0).expect("root URI");
        let mut resources = WorkspaceResources::default();

        let error = resources
            .load_roots(&[root_uri])
            .expect_err("root and include exceed count");

        assert!(error.contains("file limit"), "{error}");
    }

    #[test]
    fn analysis_snapshot_does_not_charge_resources_outside_configured_roots() {
        let root = TestDirectory::new();
        std::fs::create_dir(root.0.join("docs")).expect("docs");
        std::fs::create_dir(root.0.join("other")).expect("other");
        std::fs::write(
            root.0.join(adocweave_config::FILE_NAME),
            "schema-version = 1\n[resources]\ninclude = true\nroots = [\"docs\"]\nmax-files = 1\nmax-total-bytes = 8\nmax-resource-bytes = 8\n",
        )
        .expect("root config");
        std::fs::write(root.0.join("docs/root.adoc"), "root").expect("root source");
        write_resource_config(&root.0.join("other"), 1, 8, 8, false);
        std::fs::write(root.0.join("other/outside.adoc"), "outside").expect("outside source");
        let root_uri = Url::from_directory_path(&root.0).expect("root URI");
        let document_uri =
            Url::from_file_path(root.0.join("docs/root.adoc")).expect("document URI");
        let mut resources = WorkspaceResources::default();
        resources.load_roots(&[root_uri]).expect("load workspace");

        let input = resources
            .input(&document_uri)
            .expect("outside resource is not charged");
        assert_eq!(input.snapshot.resources().count(), 1);
    }

    #[test]
    fn watched_resource_outside_configured_roots_is_not_ingested() {
        let root = TestDirectory::new();
        let docs = root.0.join("docs");
        let other = root.0.join("other");
        std::fs::create_dir(&docs).expect("docs");
        std::fs::create_dir(&other).expect("other");
        std::fs::write(
            root.0.join(adocweave_config::FILE_NAME),
            "schema-version = 1\n[resources]\nroots = [\"docs\"]\nmax-files = 1\nmax-total-bytes = 8\nmax-resource-bytes = 8\n",
        )
        .expect("project configuration");
        std::fs::write(docs.join("root.adoc"), "root").expect("root document");
        let outside = other.join("new.adoc");
        let root_uri = Url::from_directory_path(&root.0).expect("root URI");
        let outside_uri = Url::from_file_path(&outside).expect("outside URI");
        let mut resources = WorkspaceResources::default();
        resources.load_roots(&[root_uri]).expect("load workspace");
        std::fs::write(&outside, "outside").expect("outside document");

        let affected = resources
            .reload_file(outside_uri.clone())
            .expect("ignored outside resource");

        assert!(affected.is_empty());
        assert!(resources.get(&outside_uri).is_none());
    }

    #[test]
    fn analysis_snapshot_applies_root_single_resource_limit_to_nested_projects() {
        let root = TestDirectory::new();
        write_resource_config(&root.0, 2, 6, 2, true);
        let root_path = root.0.join("root.adoc");
        std::fs::write(&root_path, "a").expect("root source");
        let nested = root.0.join("nested");
        std::fs::create_dir(&nested).expect("nested");
        write_resource_config(&nested, 1, 4, 4, false);
        std::fs::write(nested.join("child.adoc"), "bbb").expect("child source");
        let root_uri = Url::from_directory_path(&root.0).expect("root URI");
        let document_uri = Url::from_file_path(root_path).expect("document URI");
        let mut resources = WorkspaceResources::default();
        resources.load_roots(&[root_uri]).expect("load workspace");

        let error = resources
            .input(&document_uri)
            .expect_err("root single-resource snapshot limit");
        assert!(error.contains("analysis snapshot"), "{error}");
    }

    #[test]
    fn analysis_snapshot_checked_addition_applies_root_total_limit() {
        let root = TestDirectory::new();
        write_resource_config(&root.0, 2, 3, 3, true);
        let root_path = root.0.join("root.adoc");
        std::fs::write(&root_path, "aa").expect("root source");
        let nested = root.0.join("nested");
        std::fs::create_dir(&nested).expect("nested");
        write_resource_config(&nested, 1, 3, 3, false);
        std::fs::write(nested.join("child.adoc"), "bb").expect("child source");
        let root_uri = Url::from_directory_path(&root.0).expect("root URI");
        let document_uri = Url::from_file_path(root_path).expect("document URI");
        let mut resources = WorkspaceResources::default();
        resources.load_roots(&[root_uri]).expect("load workspace");

        let error = resources
            .input(&document_uri)
            .expect_err("root total snapshot limit");
        assert!(error.contains("analysis snapshot"), "{error}");
    }
}
