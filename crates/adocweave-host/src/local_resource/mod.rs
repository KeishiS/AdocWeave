use std::collections::BTreeMap;
#[cfg(not(target_os = "linux"))]
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::filesystem_job::{FilesystemJobCoordinator, FilesystemReadPermit};
use crate::filesystem_limits::FilesystemReadLimits;
use crate::local_target::{
    CoordinatedLocalTargetError, FilesystemRaceResistance, LocalTargetError, LocalTargetPolicy,
    LocalTargetSession, LocalTargetTextRollback,
};

mod budget;
mod discovery;
mod error;

pub use budget::ResourceBudget;
use discovery::LocalFilesystemView;
pub use error::{FilesystemDraftError, ResourceError};

/// Maximum number of directory authorities retained by one policy.
///
/// A Linux authority owns one file descriptor per root. This bound is kept
/// separate from the number of files a session may read so configuration alone
/// cannot exhaust the process file-descriptor table before any read begins.
const MAX_FILESYSTEM_POLICY_ROOTS: usize = LocalFilesystemPolicy::MAX_ROOTS;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LocalFilesystemPolicy {
    roots: Vec<PathBuf>,
    root_policies: Vec<LocalTargetPolicy>,
    limits: FilesystemReadLimits,
}

impl LocalFilesystemPolicy {
    /// Maximum number of directory authorities retained by one policy.
    pub const MAX_ROOTS: usize = 128;
}

/// Filesystem roots derived from one retained anchor.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct DerivedFilesystemRoots {
    /// Roots which must remain below the retained `anchor`.
    pub confined: Vec<PathBuf>,
    /// Roots explicitly selected by the caller as independent authorities.
    pub independent: Vec<PathBuf>,
}

/// Host-defined identity which is safe to expose in diagnostics and source maps.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct LogicalSourceId(Arc<str>);

impl LogicalSourceId {
    pub fn new(value: impl Into<Arc<str>>) -> Result<Self, ResourceError> {
        let value = value.into();
        if value.is_empty() || value.chars().any(char::is_control) {
            return Err(ResourceError::InvalidSourceId);
        }
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct FilesystemProvenance {
    canonical_path: PathBuf,
}

/// Immutable UTF-8 source paired with its logical identity.
#[derive(Clone, Debug)]
pub struct LoadedFilesystemSource {
    source_id: LogicalSourceId,
    source: Arc<str>,
    provenance: FilesystemProvenance,
    binding: FilesystemResourceBinding,
}

/// Result of reading one authorized filesystem resource.
///
/// A missing target is a normal observation rather than an I/O failure. It
/// carries no binding because there is no retained filesystem resource to
/// release. Callers decide whether absence is allowed by their own document
/// semantics.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum FilesystemReadOutcome {
    Found(LoadedFilesystemSource),
    NotFound {
        source_id: LogicalSourceId,
        candidate_path: PathBuf,
    },
}

impl FilesystemReadOutcome {
    /// Requires a loaded resource and maps absence to the legacy error type.
    ///
    /// This conversion does not undo state already changed by an outcome API.
    pub fn into_loaded(self) -> Result<LoadedFilesystemSource, ResourceError> {
        match self {
            Self::Found(loaded) => Ok(loaded),
            Self::NotFound { candidate_path, .. } => Err(ResourceError::Missing(candidate_path)),
        }
    }
}

/// Stable opaque identity of one local-filesystem session.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct LocalFilesystemSessionId(u64);

/// Generation-specific ownership of one candidate path in a session.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FilesystemResourceBinding {
    session_id: LocalFilesystemSessionId,
    candidate_path: PathBuf,
    canonical_path: PathBuf,
    generation: u64,
}

impl FilesystemResourceBinding {
    pub const fn session_id(&self) -> LocalFilesystemSessionId {
        self.session_id
    }

    pub fn candidate_path(&self) -> &Path {
        &self.candidate_path
    }

    pub fn canonical_path(&self) -> &Path {
        &self.canonical_path
    }
}

/// Result of releasing a generation-specific binding from a draft.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FilesystemReleaseOutcome {
    Released,
    Stale,
    Missing,
}

/// An isolated candidate state for one filesystem session.
///
/// Dropping this value leaves the live resource state unchanged. Binding
/// generations are deliberately consumed across all drafts and are never
/// reused, including when a draft is dropped. [`Self::prepare_commit`]
/// validates draft-local identity and revision conditions. The prepared commit
/// then verifies that the originating job is still active while holding its
/// lock across the live state replacement.
#[must_use = "a filesystem draft must be committed or dropped"]
#[derive(Debug)]
pub struct LocalFilesystemDraft {
    session_id: LocalFilesystemSessionId,
    base_revision: u64,
    candidate: LocalFilesystemState,
    lease: FilesystemDraftLease,
    binding_generations: Arc<AtomicU64>,
    job: FilesystemJobCoordinator,
    poisoned: bool,
}

#[derive(Debug)]
struct FilesystemDraftLease {
    active: Arc<AtomicU64>,
    token: u64,
}

/// A filesystem state replacement prepared from one draft and job.
#[must_use = "a prepared filesystem commit must be committed or dropped"]
pub struct PreparedFilesystemCommit<'a> {
    live: &'a mut LocalFilesystemSession,
    candidate: LocalFilesystemState,
    next_revision: u64,
    job: FilesystemJobCoordinator,
    _lease: FilesystemDraftLease,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct FilesystemCharge {
    bytes: u64,
    generation: u64,
}

impl LoadedFilesystemSource {
    pub fn source_id(&self) -> &LogicalSourceId {
        &self.source_id
    }

    pub fn canonical_path(&self) -> &Path {
        &self.provenance.canonical_path
    }

    pub fn source(&self) -> &str {
        &self.source
    }

    pub const fn binding(&self) -> &FilesystemResourceBinding {
        &self.binding
    }

    pub fn into_parts(self) -> (LogicalSourceId, Arc<str>) {
        (self.source_id, self.source)
    }

    pub fn into_parts_with_binding(self) -> (LogicalSourceId, Arc<str>, FilesystemResourceBinding) {
        (self.source_id, self.source, self.binding)
    }
}

impl PartialEq for LoadedFilesystemSource {
    fn eq(&self, other: &Self) -> bool {
        self.source_id == other.source_id
            && self.source == other.source
            && self.provenance == other.provenance
    }
}

impl Eq for LoadedFilesystemSource {}

/// Per-command filesystem capability shared by all native resource consumers.
///
/// Construction opens one policy for each canonical root. Reads are delegated
/// to the same handle-relative implementation used by local-target checks, and
/// one budget is enforced across every root.
#[derive(Debug)]
pub struct LocalFilesystemSession {
    session_id: LocalFilesystemSessionId,
    revision: u64,
    active_draft: Arc<AtomicU64>,
    next_binding_generation: Arc<AtomicU64>,
    state: LocalFilesystemState,
}

#[derive(Debug)]
struct LocalFilesystemState {
    roots: Vec<PathBuf>,
    sessions: Vec<LocalTargetSession>,
    limits: FilesystemReadLimits,
    budget: ResourceBudget,
    charged: BTreeMap<PathBuf, FilesystemCharge>,
    candidates: BTreeMap<PathBuf, FilesystemCandidateBinding>,
    /// Shared with every [`LocalTargetSession`] above and with any draft cloned
    /// from this state, so discovery and reads land in one set of counters and a
    /// discarded draft does not un-count the work it performed.

    #[cfg(test)]
    clone_count: Arc<AtomicU64>,
}

impl Clone for LocalFilesystemState {
    fn clone(&self) -> Self {
        #[cfg(test)]
        {
            self.clone_count.fetch_add(1, Ordering::Relaxed);
            FORCE_DRAFT_STATE_CLONE_PANIC.with(|forced| {
                assert!(!forced.get(), "forced filesystem draft clone panic");
            });
        }
        Self {
            roots: self.roots.clone(),
            sessions: self.sessions.clone(),
            limits: self.limits,
            budget: self.budget,
            charged: self.charged.clone(),
            candidates: self.candidates.clone(),
            #[cfg(test)]
            clone_count: Arc::clone(&self.clone_count),
        }
    }
}

struct LocalFilesystemMutationCursor<'a> {
    session_id: LocalFilesystemSessionId,
    binding_generations: &'a Arc<AtomicU64>,
    job: Option<&'a FilesystemJobCoordinator>,
    state: &'a mut LocalFilesystemState,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum MissingDisposition {
    PreserveLegacyState,
    ApplyNotFound,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct FilesystemCandidateBinding {
    canonical_path: PathBuf,
    generation: u64,
}

static NEXT_FILESYSTEM_SESSION_ID: AtomicU64 = AtomicU64::new(1);

#[cfg(test)]
thread_local! {
    static FORCE_DRAFT_STATE_CLONE_PANIC: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

impl LocalFilesystemPolicy {
    pub fn new(
        roots: impl IntoIterator<Item = PathBuf>,
        limits: FilesystemReadLimits,
    ) -> Result<Self, ResourceError> {
        let mut unique = BTreeMap::new();
        for path in roots {
            let policy = LocalTargetPolicy::new(&path)
                .map_err(|error| map_policy_root_error(path, error))?;
            let root = policy.root().to_owned();
            if !unique.contains_key(&root) && unique.len() >= MAX_FILESYSTEM_POLICY_ROOTS {
                return Err(ResourceError::RootLimit {
                    limit: MAX_FILESYSTEM_POLICY_ROOTS,
                });
            }
            unique.entry(root).or_insert(policy);
        }
        let root_policies = unique.into_values().collect::<Vec<_>>();
        if root_policies.is_empty() {
            return Err(ResourceError::NoRoots);
        }
        let roots = root_policies
            .iter()
            .map(|policy| policy.root().to_owned())
            .collect();
        Ok(Self {
            roots,
            root_policies,
            limits,
        })
    }

    pub fn roots(&self) -> &[PathBuf] {
        &self.roots
    }

    pub const fn limits(&self) -> FilesystemReadLimits {
        self.limits
    }

    /// Selects a narrowed policy from roots this policy already retains.
    pub fn access_existing(
        &self,
        roots: impl IntoIterator<Item = PathBuf>,
        limits: FilesystemReadLimits,
    ) -> Result<Self, ResourceError> {
        validate_derived_limits(self.limits, limits)?;
        let policies = roots
            .into_iter()
            .map(|root| {
                self.root_policy(&root)
                    .cloned()
                    .ok_or(ResourceError::OutsideRoots(root))
            })
            .collect::<Result<Vec<_>, _>>()?;
        Self::from_selected(policies, limits)
    }

    /// Extends the retained authority transactionally and returns one opaque
    /// access set for the requested roots.
    pub fn access_derived(
        &mut self,
        anchor: &Path,
        roots: DerivedFilesystemRoots,
        limits: FilesystemReadLimits,
    ) -> Result<Self, ResourceError> {
        validate_derived_limits(self.limits, limits)?;
        let anchor_policy = self
            .root_policy(anchor)
            .cloned()
            .ok_or_else(|| ResourceError::OutsideRoots(anchor.to_owned()))?;
        let mut pending = BTreeMap::new();
        let mut selected = Vec::new();
        for path in roots.confined {
            let policy = if path == anchor {
                anchor_policy.clone()
            } else {
                anchor_policy
                    .derive_confined_directory(&path)
                    .map_err(|error| map_policy_root_error(path, error))?
            };
            let root = policy.root().to_owned();
            self.retain_pending_policy(&pending, &root)?;
            pending.entry(root.clone()).or_insert(policy);
            selected.push(root);
        }
        for path in roots.independent {
            let policy = LocalTargetPolicy::new(&path)
                .map_err(|error| map_policy_root_error(path, error))?;
            let root = policy.root().to_owned();
            self.retain_pending_policy(&pending, &root)?;
            pending.entry(root.clone()).or_insert(policy);
            selected.push(root);
        }
        self.insert_policies(pending.into_values());
        self.access_existing(selected, limits)
    }

    fn retain_pending_policy(
        &self,
        pending: &BTreeMap<PathBuf, LocalTargetPolicy>,
        root: &Path,
    ) -> Result<(), ResourceError> {
        if self.root_policy(root).is_none()
            && !pending.contains_key(root)
            && self.root_policies.len() + pending.len() >= MAX_FILESYSTEM_POLICY_ROOTS
        {
            return Err(ResourceError::RootLimit {
                limit: MAX_FILESYSTEM_POLICY_ROOTS,
            });
        }
        Ok(())
    }

    fn insert_policies(&mut self, policies: impl IntoIterator<Item = LocalTargetPolicy>) {
        let mut unique = std::mem::take(&mut self.root_policies)
            .into_iter()
            .map(|policy| (policy.root().to_owned(), policy))
            .collect::<BTreeMap<_, _>>();
        for policy in policies {
            let root = policy.root().to_owned();
            unique.entry(root).or_insert(policy);
        }
        self.roots = unique.keys().cloned().collect();
        self.root_policies = unique.into_values().collect();
    }

    /// Returns the retained authority for one exact canonical root.
    pub fn root_policy(&self, root: &Path) -> Option<&LocalTargetPolicy> {
        self.root_policies
            .iter()
            .find(|policy| policy.root() == root)
    }
}

impl LocalFilesystemPolicy {
    fn from_selected(
        mut root_policies: Vec<LocalTargetPolicy>,
        limits: FilesystemReadLimits,
    ) -> Result<Self, ResourceError> {
        root_policies.sort_by(|left, right| left.root().cmp(right.root()));
        root_policies.dedup_by(|left, right| left.root() == right.root());
        if root_policies.is_empty() {
            return Err(ResourceError::NoRoots);
        }
        if root_policies.len() > MAX_FILESYSTEM_POLICY_ROOTS {
            return Err(ResourceError::RootLimit {
                limit: MAX_FILESYSTEM_POLICY_ROOTS,
            });
        }
        let roots = root_policies
            .iter()
            .map(|policy| policy.root().to_owned())
            .collect();
        Ok(Self {
            roots,
            root_policies,
            limits,
        })
    }

    /// Selects the deepest retained root containing `path`.
    pub fn policy_for_path(&self, path: &Path) -> Option<&LocalTargetPolicy> {
        self.root_policies
            .iter()
            .filter(|policy| path.starts_with(policy.root()))
            .max_by_key(|policy| policy.root().components().count())
    }

    /// Creates a session with a fresh shared budget for these selected roots.
    pub fn session(&self) -> Result<LocalFilesystemSession, ResourceError> {
        let sessions = self
            .root_policies
            .iter()
            .cloned()
            .map(|policy| {
                LocalTargetSession::new(
                    policy,
                    self.limits.max_files,
                    FilesystemReadLimits {
                        max_files: usize::MAX,
                        max_total_bytes: u64::MAX,
                        max_resource_bytes: self.limits.max_resource_bytes,
                    },
                )
            })
            .collect();
        let session_id = NEXT_FILESYSTEM_SESSION_ID
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, next_session_id)
            .map_err(|_| ResourceError::SessionIdentityExhausted)?;
        Ok(LocalFilesystemSession {
            session_id: LocalFilesystemSessionId(session_id),
            revision: 0,
            active_draft: Arc::new(AtomicU64::new(0)),
            next_binding_generation: Arc::new(AtomicU64::new(1)),
            state: LocalFilesystemState {
                roots: self.roots.clone(),
                sessions,
                limits: self.limits,
                budget: ResourceBudget::default(),
                charged: BTreeMap::new(),
                candidates: BTreeMap::new(),
                #[cfg(test)]
                clone_count: Arc::new(AtomicU64::new(0)),
            },
        })
    }
}

fn validate_derived_limits(
    policy: FilesystemReadLimits,
    requested: FilesystemReadLimits,
) -> Result<(), ResourceError> {
    if requested.max_files > policy.max_files
        || requested.max_total_bytes > policy.max_total_bytes
        || requested.max_resource_bytes > policy.max_resource_bytes
    {
        return Err(ResourceError::Unverifiable(
            "filesystem access limits exceed the authority limits".to_owned(),
        ));
    }
    Ok(())
}

fn map_policy_root_error(path: PathBuf, error: LocalTargetError) -> ResourceError {
    match error {
        LocalTargetError::Missing(_) => ResourceError::Missing(path),
        LocalTargetError::PermissionDenied(_) => ResourceError::PermissionDenied(path),
        LocalTargetError::OutsideRoot(_) => ResourceError::OutsideRoots(path),
        LocalTargetError::NotDirectory(_) | LocalTargetError::NotFile(_) => {
            ResourceError::InvalidRoot
        }
        error => ResourceError::Inspect {
            path,
            source: error.to_string(),
        },
    }
}

const fn next_session_id(current: u64) -> Option<u64> {
    current.checked_add(1)
}

impl LocalFilesystemSession {
    /// Maximum directory entries inspected by one recursive scan.
    pub const MAX_SCAN_ENTRIES: usize = 100_000;

    pub const fn id(&self) -> LocalFilesystemSessionId {
        self.session_id
    }

    /// Creates an isolated candidate state without changing this live session.
    pub fn draft(
        &self,
        job: &FilesystemJobCoordinator,
    ) -> Result<LocalFilesystemDraft, FilesystemDraftError> {
        job.register_session(self.session_id)?;
        let token = self
            .revision
            .checked_add(1)
            .ok_or(FilesystemDraftError::SessionRevisionExhausted)?;
        self.active_draft
            .compare_exchange(0, token, Ordering::AcqRel, Ordering::Acquire)
            .map_err(|_| FilesystemDraftError::DraftBusy)?;
        let lease = FilesystemDraftLease {
            active: Arc::clone(&self.active_draft),
            token,
        };
        Ok(LocalFilesystemDraft {
            session_id: self.session_id,
            base_revision: self.revision,
            candidate: self.clone_for_draft(),
            lease,
            binding_generations: Arc::clone(&self.next_binding_generation),
            job: job.clone(),
            poisoned: false,
        })
    }

    fn clone_for_draft(&self) -> LocalFilesystemState {
        self.state.clone()
    }

    pub fn roots(&self) -> &[PathBuf] {
        &self.state.roots
    }

    pub const fn limits(&self) -> FilesystemReadLimits {
        self.state.limits
    }

    /// Returns the retained authority for the deepest root containing `path`.
    pub fn policy_for_path(&self, path: &Path) -> Option<&LocalTargetPolicy> {
        self.state
            .sessions
            .iter()
            .map(LocalTargetSession::policy)
            .filter(|policy| path.starts_with(policy.root()))
            .max_by_key(|policy| policy.root().components().count())
    }

    /// Scans every configured root for regular `.adoc` files.
    ///
    /// Directory entries and candidates are sorted before reading, symlinks are
    /// not followed, and all reads consume this session's shared resource
    /// budget. The caller supplies logical identities so canonical filesystem
    /// paths do not become semantic source IDs.
    pub fn scan_utf8(
        &mut self,
        source_id: impl FnMut(&Path) -> Result<LogicalSourceId, ResourceError>,
    ) -> Result<Vec<LoadedFilesystemSource>, ResourceError> {
        self.invalidate_active_draft();
        self.mutation_cursor()
            .scan_utf8(source_id)
            .map_err(ResourceError::from)
    }

    /// Discovers canonical `.adoc` candidate paths without reading file content.
    ///
    /// This split lets an adapter resolve the nearest project configuration
    /// before selecting the read budget used for each candidate.
    pub fn discover_adoc_paths(&self) -> Result<Vec<PathBuf>, ResourceError> {
        self.discover_adoc_paths_with(|_, _| false)
    }

    /// Discovers `.adoc` candidates while pruning selected directories.
    ///
    /// The predicate receives the canonical scan root and a non-empty path
    /// relative to that root. It is evaluated only for real directories after
    /// symlinks have been rejected and before the directory contents are read.
    pub fn discover_adoc_paths_with(
        &self,
        exclude_directory: impl FnMut(&Path, &Path) -> bool,
    ) -> Result<Vec<PathBuf>, ResourceError> {
        self.discover_adoc_paths_with_control(exclude_directory, || false)
    }

    /// Discovers `.adoc` candidates with directory pruning and cancellation.
    ///
    /// Cancellation is checked before inspecting each queued path and after
    /// each directory entry is observed. It returns an error so a caller never
    /// mistakes a partial walk for a complete workspace snapshot.
    pub fn discover_adoc_paths_with_control(
        &self,
        exclude_directory: impl FnMut(&Path, &Path) -> bool,
        is_cancelled: impl FnMut() -> bool,
    ) -> Result<Vec<PathBuf>, ResourceError> {
        LocalFilesystemView {
            state: &self.state,
            job: None,
        }
        .discover_adoc_paths_with_control(
            Self::MAX_SCAN_ENTRIES,
            exclude_directory,
            is_cancelled,
        )
    }

    #[cfg(test)]
    fn discover_adoc_paths_with_limit(
        &self,
        scan_entry_limit: usize,
        exclude_directory: impl FnMut(&Path, &Path) -> bool,
        is_cancelled: impl FnMut() -> bool,
    ) -> Result<Vec<PathBuf>, ResourceError> {
        LocalFilesystemView {
            state: &self.state,
            job: None,
        }
        .discover_adoc_paths_with_control(
            scan_entry_limit,
            exclude_directory,
            is_cancelled,
        )
    }
}

impl LocalFilesystemSession {
    /// Returns this process-local session identity.
    pub const fn session_id(&self) -> LocalFilesystemSessionId {
        self.session_id
    }

    /// Returns the concurrent-filesystem guarantee of all configured roots.
    pub fn race_resistance(&self) -> FilesystemRaceResistance {
        self.state
            .sessions
            .iter()
            .map(|session| session.policy().race_resistance())
            .min_by_key(|resistance| match resistance {
                FilesystemRaceResistance::StaticSnapshotOnly => 0,
                FilesystemRaceResistance::HandleRelative => 1,
            })
            .unwrap_or(FilesystemRaceResistance::StaticSnapshotOnly)
    }

    /// Reads one absolute filesystem path below exactly one configured root.
    pub fn read_utf8(
        &mut self,
        source_id: LogicalSourceId,
        path: &Path,
    ) -> Result<LoadedFilesystemSource, ResourceError> {
        self.invalidate_active_draft();
        self.mutation_cursor()
            .read_utf8_preserving_missing(source_id, path)
            .map_err(ResourceError::from)?
            .into_loaded()
    }

    /// Reads one absolute path and reports an absent target as a normal result.
    ///
    /// `NotFound` immediately releases this path's live binding. Cached text is
    /// released when its selected root has no alias; the shared charge is
    /// released when no alias remains across all roots.
    pub fn read_utf8_outcome(
        &mut self,
        source_id: LogicalSourceId,
        path: &Path,
    ) -> Result<FilesystemReadOutcome, ResourceError> {
        self.invalidate_active_draft();
        self.mutation_cursor()
            .read_utf8(source_id, path)
            .map_err(ResourceError::from)
    }

    /// Resolves and reads one authored target relative to an absolute base.
    pub fn read_target_utf8(
        &mut self,
        source_id: LogicalSourceId,
        base: &Path,
        target: &str,
    ) -> Result<LoadedFilesystemSource, ResourceError> {
        self.invalidate_active_draft();
        self.mutation_cursor()
            .read_target_utf8_preserving_missing(source_id, base, target)
            .map_err(ResourceError::from)?
            .into_loaded()
    }

    /// Resolves one authored target and reports absence as a normal result.
    ///
    /// `NotFound` immediately releases this path's live binding. Cached text is
    /// released when its selected root has no alias; the shared charge is
    /// released when no alias remains across all roots.
    pub fn read_target_utf8_outcome(
        &mut self,
        source_id: LogicalSourceId,
        base: &Path,
        target: &str,
    ) -> Result<FilesystemReadOutcome, ResourceError> {
        self.invalidate_active_draft();
        self.mutation_cursor()
            .read_target_utf8(source_id, base, target)
            .map_err(ResourceError::from)
    }

    /// Reopens an absolute path while retaining this session's shared budget.
    pub fn reread_utf8(
        &mut self,
        source_id: LogicalSourceId,
        path: &Path,
    ) -> Result<LoadedFilesystemSource, ResourceError> {
        self.invalidate_active_draft();
        self.mutation_cursor()
            .reread_utf8_preserving_missing(source_id, path)
            .map_err(ResourceError::from)?
            .into_loaded()
    }

    /// Reopens one absolute path and reports absence as a normal result.
    ///
    /// `NotFound` immediately releases this path's live binding. Cached text is
    /// released when its selected root has no alias; the shared charge is
    /// released when no alias remains across all roots.
    pub fn reread_utf8_outcome(
        &mut self,
        source_id: LogicalSourceId,
        path: &Path,
    ) -> Result<FilesystemReadOutcome, ResourceError> {
        self.invalidate_active_draft();
        self.mutation_cursor()
            .reread_utf8(source_id, path)
            .map_err(ResourceError::from)
    }

    fn mutation_cursor(&mut self) -> LocalFilesystemMutationCursor<'_> {
        LocalFilesystemMutationCursor {
            session_id: self.session_id,
            binding_generations: &self.next_binding_generation,
            job: None,
            state: &mut self.state,
        }
    }

    fn invalidate_active_draft(&mut self) {
        if self.active_draft.load(Ordering::Acquire) != 0
            && let Some(revision) = self.revision.checked_add(1)
        {
            self.revision = revision;
        }
    }

    #[cfg(test)]
    fn read_utf8_after_open(
        &mut self,
        source_id: LogicalSourceId,
        path: &Path,
        after_open: impl FnOnce(),
    ) -> Result<LoadedFilesystemSource, ResourceError> {
        let job = FilesystemJobCoordinator::new(crate::FilesystemJobLimits::unbounded())
            .map_err(FilesystemDraftError::from)?;
        let mut draft = self.draft(&job)?;
        let loaded = draft
            .mutation_cursor()
            .read_utf8_with(source_id, path, false, after_open)?;
        draft.prepare_commit(self)?.commit()?;
        match loaded {
            FilesystemReadOutcome::Found(loaded) => Ok(loaded),
            FilesystemReadOutcome::NotFound { candidate_path, .. } => {
                Err(ResourceError::Missing(candidate_path))
            }
        }
    }

    #[cfg(test)]
    fn read_target_utf8_after_open(
        &mut self,
        source_id: LogicalSourceId,
        base: &Path,
        target: &str,
        after_open: impl FnOnce(),
    ) -> Result<LoadedFilesystemSource, ResourceError> {
        let index = self.root_index(base)?;
        let candidate = self.state.sessions[index]
            .candidate(base, target)
            .map_err(ResourceError::from)?;
        let binding_generation = self.reserve_binding_generation()?;
        let max_resource_bytes = self.state.limits.max_resource_bytes;
        let loaded = self.state.sessions[index]
            .read_candidate_utf8_with_capacity(&candidate, true, true, after_open, |_| {
                crate::local_target::CandidateReadCapacity {
                    allow_file: true,
                    max_total_bytes: u64::MAX,
                    max_resource_bytes,
                }
            })
            .map_err(ResourceError::from)?;
        self.finish_read(
            self.session_id,
            binding_generation,
            source_id,
            &candidate,
            loaded,
        )
    }

    /// Only the test-only read path below still needs these helpers; every
    /// production read now goes through a draft.
    #[cfg(test)]
    fn root_index(&self, path: &Path) -> Result<usize, ResourceError> {
        if !path.is_absolute() {
            return Err(ResourceError::PathNotAbsolute(path.to_owned()));
        }
        self.state
            .roots
            .iter()
            .enumerate()
            .filter(|(_, root)| path.starts_with(root))
            .max_by_key(|(_, root)| root.components().count())
            .map(|(index, _)| index)
            .ok_or_else(|| ResourceError::OutsideRoots(path.to_owned()))
    }

    #[cfg(test)]
    fn finish_read(
        &mut self,
        session_id: LocalFilesystemSessionId,
        binding_generation: u64,
        source_id: LogicalSourceId,
        candidate: &Path,
        loaded: crate::local_target::LoadedLocalTarget,
    ) -> Result<LoadedFilesystemSource, ResourceError> {
        let (canonical_path, source) = loaded.into_shared_parts();
        let bytes = source.len() as u64;
        let previous_candidate = self.state.candidates.get(candidate).cloned();
        let displaced_charge = previous_candidate
            .as_ref()
            .filter(|previous| previous.canonical_path.as_path() != canonical_path)
            .filter(|previous| {
                !self.state.candidates.iter().any(|(other, binding)| {
                    other.as_path() != candidate
                        && binding.canonical_path == previous.canonical_path
                })
            })
            .and_then(|previous| {
                self.state
                    .charged
                    .get(&previous.canonical_path)
                    .copied()
                    .map(|charge| (previous.canonical_path.clone(), charge))
            });
        let previous_charge = self.state.charged.get(&canonical_path).copied();
        let mut next_budget = self.state.budget;
        if let Some((_, charge)) = &displaced_charge {
            next_budget.release(charge.bytes);
        }
        next_budget.replace(
            &canonical_path,
            previous_charge.map(|charge| charge.bytes),
            bytes,
            self.state.limits,
        )?;
        self.state.budget = next_budget;
        if let Some((path, _)) = &displaced_charge {
            self.state.charged.remove(path);
        }
        self.state.charged.insert(
            canonical_path.clone(),
            FilesystemCharge {
                bytes,
                generation: binding_generation,
            },
        );
        self.state.candidates.insert(
            candidate.to_owned(),
            FilesystemCandidateBinding {
                canonical_path: canonical_path.clone(),
                generation: binding_generation,
            },
        );
        let binding = FilesystemResourceBinding {
            session_id,
            candidate_path: candidate.to_owned(),
            canonical_path: canonical_path.clone(),
            generation: binding_generation,
        };
        Ok(LoadedFilesystemSource {
            source_id,
            source,
            provenance: FilesystemProvenance { canonical_path },
            binding,
        })
    }

    pub const fn budget(&self) -> ResourceBudget {
        self.state.budget
    }

    #[cfg(test)]
    fn reserve_binding_generation(&mut self) -> Result<u64, FilesystemDraftError> {
        self.next_binding_generation
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |generation| {
                generation.checked_add(1)
            })
            .map_err(|_| FilesystemDraftError::BindingGenerationExhausted)
    }
}

impl LocalFilesystemMutationCursor<'_> {
    fn begin_read(&self) -> Result<Option<FilesystemReadPermit>, FilesystemDraftError> {
        self.job
            .map(|job| job.begin_read(self.session_id))
            .transpose()
            .map_err(FilesystemDraftError::from)
    }

    fn record_candidate_change(&self) -> Result<(), FilesystemDraftError> {
        self.job
            .map(FilesystemJobCoordinator::record_candidate_change)
            .transpose()
            .map(|_| ())
            .map_err(FilesystemDraftError::from)
    }

    fn scan_utf8(
        &mut self,
        mut source_id: impl FnMut(&Path) -> Result<LogicalSourceId, ResourceError>,
    ) -> Result<Vec<LoadedFilesystemSource>, FilesystemDraftError> {
        let paths = LocalFilesystemView {
            state: self.state,
            job: self.job.map(|job| (self.session_id, job)),
        }
        .discover_adoc_paths_with_control(
            LocalFilesystemSession::MAX_SCAN_ENTRIES,
            |_, _| false,
            || false,
        )?;
        if paths.len() > self.state.limits.max_files {
            return Err(ResourceError::FileLimit {
                limit: self.state.limits.max_files,
            }
            .into());
        }
        let mut loaded = Vec::with_capacity(paths.len());
        for path in paths {
            let source_id = source_id(&path)?;
            loaded.push(
                self.read_utf8_preserving_missing(source_id, &path)?
                    .into_loaded()?,
            );
        }
        Ok(loaded)
    }

    fn read_utf8(
        &mut self,
        source_id: LogicalSourceId,
        path: &Path,
    ) -> Result<FilesystemReadOutcome, FilesystemDraftError> {
        self.read_utf8_with(source_id, path, false, || {})
    }

    fn read_utf8_no_symlinks(
        &mut self,
        source_id: LogicalSourceId,
        path: &Path,
    ) -> Result<FilesystemReadOutcome, FilesystemDraftError> {
        self.read_utf8_with_disposition(
            source_id,
            path,
            false,
            false,
            || {},
            MissingDisposition::ApplyNotFound,
        )
    }

    fn read_utf8_preserving_missing(
        &mut self,
        source_id: LogicalSourceId,
        path: &Path,
    ) -> Result<FilesystemReadOutcome, FilesystemDraftError> {
        self.read_utf8_with_disposition(
            source_id,
            path,
            false,
            true,
            || {},
            MissingDisposition::PreserveLegacyState,
        )
    }

    fn read_target_utf8(
        &mut self,
        source_id: LogicalSourceId,
        base: &Path,
        target: &str,
    ) -> Result<FilesystemReadOutcome, FilesystemDraftError> {
        self.read_target_utf8_with_disposition(
            source_id,
            base,
            target,
            MissingDisposition::ApplyNotFound,
        )
    }

    fn read_target_utf8_preserving_missing(
        &mut self,
        source_id: LogicalSourceId,
        base: &Path,
        target: &str,
    ) -> Result<FilesystemReadOutcome, FilesystemDraftError> {
        self.read_target_utf8_with_disposition(
            source_id,
            base,
            target,
            MissingDisposition::PreserveLegacyState,
        )
    }

    fn read_target_utf8_with_disposition(
        &mut self,
        source_id: LogicalSourceId,
        base: &Path,
        target: &str,
        missing: MissingDisposition,
    ) -> Result<FilesystemReadOutcome, FilesystemDraftError> {
        let mut permit = self.begin_read()?;
        let index = self.root_index(base)?;
        let candidate = self.state.sessions[index]
            .candidate(base, target)
            .map_err(ResourceError::from)?;
        let candidate_rollback = (missing == MissingDisposition::PreserveLegacyState
            && self.state.candidates.contains_key(&candidate))
        .then(|| self.state.sessions[index].candidate_rollback(&candidate));
        let binding_generation = self.reserve_binding_generation()?;
        let budget = self.state.budget;
        let charged = &self.state.charged;
        let candidates = &self.state.candidates;
        let limits = self.state.limits;
        let file_limit_denied = std::cell::Cell::new(false);
        let loaded = match read_candidate_with_optional_job(
            &mut self.state.sessions[index],
            &candidate,
            false,
            true,
            || {},
            |canonical| {
                shared_read_capacity(
                    budget,
                    charged,
                    candidates,
                    limits,
                    &candidate,
                    canonical,
                    &file_limit_denied,
                )
            },
            permit.as_mut(),
        ) {
            Ok(loaded) => loaded,
            Err(CoordinatedLocalTargetError::Target(LocalTargetError::Missing(_))) => {
                if missing == MissingDisposition::ApplyNotFound {
                    self.apply_not_found(&candidate, index)?;
                } else if let Some(rollback) = candidate_rollback {
                    self.state.sessions[index].rollback_candidate(rollback);
                }
                return Ok(FilesystemReadOutcome::NotFound {
                    source_id,
                    candidate_path: candidate,
                });
            }
            Err(error) => {
                return Err(map_coordinated_read_error(
                    error,
                    limits,
                    file_limit_denied.get(),
                ));
            }
        };
        self.finish_read(binding_generation, source_id, &candidate, loaded)
            .map(FilesystemReadOutcome::Found)
    }

    fn reread_utf8(
        &mut self,
        source_id: LogicalSourceId,
        path: &Path,
    ) -> Result<FilesystemReadOutcome, FilesystemDraftError> {
        self.reread_utf8_with_disposition(source_id, path, MissingDisposition::ApplyNotFound)
    }

    fn reread_utf8_preserving_missing(
        &mut self,
        source_id: LogicalSourceId,
        path: &Path,
    ) -> Result<FilesystemReadOutcome, FilesystemDraftError> {
        self.reread_utf8_with_disposition(source_id, path, MissingDisposition::PreserveLegacyState)
    }

    fn reread_utf8_with_disposition(
        &mut self,
        source_id: LogicalSourceId,
        path: &Path,
        missing: MissingDisposition,
    ) -> Result<FilesystemReadOutcome, FilesystemDraftError> {
        let mut permit = self.begin_read()?;
        let index = self.root_index(path)?;
        let candidate_rollback = self.state.sessions[index].candidate_rollback(path);
        let binding_generation = self.reserve_binding_generation()?;
        let budget = self.state.budget;
        let charged = &self.state.charged;
        let candidates = &self.state.candidates;
        let limits = self.state.limits;
        let file_limit_denied = std::cell::Cell::new(false);
        let (loaded, text_rollback) = match reread_candidate_with_optional_job(
            &mut self.state.sessions[index],
            path,
            |canonical| {
                shared_read_capacity(
                    budget,
                    charged,
                    candidates,
                    limits,
                    path,
                    canonical,
                    &file_limit_denied,
                )
            },
            permit.as_mut(),
        ) {
            Ok(loaded) => loaded,
            Err(CoordinatedLocalTargetError::Target(LocalTargetError::Missing(_))) => {
                if missing == MissingDisposition::ApplyNotFound {
                    self.apply_not_found(path, index)?;
                } else {
                    self.state.sessions[index].rollback_candidate(candidate_rollback);
                }
                return Ok(FilesystemReadOutcome::NotFound {
                    source_id,
                    candidate_path: path.to_owned(),
                });
            }
            Err(error) => {
                self.state.sessions[index].rollback_candidate(candidate_rollback);
                return Err(map_coordinated_read_error(
                    error,
                    limits,
                    file_limit_denied.get(),
                ));
            }
        };
        match self.finish_read(binding_generation, source_id, path, loaded) {
            Ok(loaded) => Ok(FilesystemReadOutcome::Found(loaded)),
            Err(error) => {
                self.state.sessions[index].rollback_cached_text(text_rollback);
                self.state.sessions[index].rollback_candidate(candidate_rollback);
                Err(error)
            }
        }
    }

    /// Acquires one resource by absolute path.
    ///
    /// The attempt is counted before anything can reject it, so a path outside
    /// every root and a file the limits refuse both leave a record that work was
    /// requested.
    fn read_utf8_with(
        &mut self,
        source_id: LogicalSourceId,
        path: &Path,
        reuse_cached_text: bool,
        after_open: impl FnOnce(),
    ) -> Result<FilesystemReadOutcome, FilesystemDraftError> {
        self.read_utf8_with_disposition(
            source_id,
            path,
            reuse_cached_text,
            true,
            after_open,
            MissingDisposition::ApplyNotFound,
        )
    }

    fn read_utf8_with_disposition(
        &mut self,
        source_id: LogicalSourceId,
        path: &Path,
        reuse_cached_text: bool,
        follow_symlinks: bool,
        after_open: impl FnOnce(),
        missing: MissingDisposition,
    ) -> Result<FilesystemReadOutcome, FilesystemDraftError> {
        let mut permit = self.begin_read()?;
        if !path.is_absolute() {
            return Err(ResourceError::PathNotAbsolute(path.to_owned()).into());
        }
        let index = self.root_index(path)?;
        let candidate = path.to_owned();
        if candidate == self.state.roots[index] {
            return Err(ResourceError::NotRegularFile(candidate).into());
        }
        let candidate_rollback = (missing == MissingDisposition::PreserveLegacyState
            && self.state.candidates.contains_key(&candidate))
        .then(|| self.state.sessions[index].candidate_rollback(&candidate));
        let binding_generation = self.reserve_binding_generation()?;
        let budget = self.state.budget;
        let charged = &self.state.charged;
        let candidates = &self.state.candidates;
        let limits = self.state.limits;
        let file_limit_denied = std::cell::Cell::new(false);
        let loaded = match read_candidate_with_optional_job(
            &mut self.state.sessions[index],
            &candidate,
            reuse_cached_text,
            follow_symlinks,
            after_open,
            |canonical| {
                shared_read_capacity(
                    budget,
                    charged,
                    candidates,
                    limits,
                    &candidate,
                    canonical,
                    &file_limit_denied,
                )
            },
            permit.as_mut(),
        ) {
            Ok(loaded) => loaded,
            Err(CoordinatedLocalTargetError::Target(LocalTargetError::Missing(_))) => {
                if missing == MissingDisposition::ApplyNotFound {
                    self.apply_not_found(&candidate, index)?;
                } else if let Some(rollback) = candidate_rollback {
                    self.state.sessions[index].rollback_candidate(rollback);
                }
                return Ok(FilesystemReadOutcome::NotFound {
                    source_id,
                    candidate_path: candidate,
                });
            }
            Err(error) => {
                return Err(map_coordinated_read_error(
                    error,
                    limits,
                    file_limit_denied.get(),
                ));
            }
        };
        self.finish_read(binding_generation, source_id, &candidate, loaded)
            .map(FilesystemReadOutcome::Found)
    }

    fn root_index(&self, path: &Path) -> Result<usize, FilesystemDraftError> {
        if !path.is_absolute() {
            return Err(ResourceError::PathNotAbsolute(path.to_owned()).into());
        }
        self.state
            .roots
            .iter()
            .enumerate()
            .filter(|(_, root)| path.starts_with(root))
            .max_by_key(|(_, root)| root.components().count())
            .map(|(index, _)| index)
            .ok_or_else(|| ResourceError::OutsideRoots(path.to_owned()).into())
    }

    fn finish_read(
        &mut self,
        binding_generation: u64,
        source_id: LogicalSourceId,
        candidate: &Path,
        loaded: crate::local_target::LoadedLocalTarget,
    ) -> Result<LoadedFilesystemSource, FilesystemDraftError> {
        let (canonical_path, source) = loaded.into_shared_parts();
        let bytes = source.len() as u64;
        let previous_candidate = self.state.candidates.get(candidate).cloned();
        let displaced_charge = previous_candidate
            .as_ref()
            .filter(|previous| previous.canonical_path.as_path() != canonical_path)
            .filter(|previous| {
                !self.state.candidates.iter().any(|(other, binding)| {
                    other.as_path() != candidate
                        && binding.canonical_path == previous.canonical_path
                })
            })
            .and_then(|previous| {
                self.state
                    .charged
                    .get(&previous.canonical_path)
                    .copied()
                    .map(|charge| (previous.canonical_path.clone(), charge))
            });
        let previous_charge = self.state.charged.get(&canonical_path).copied();
        let mut next_budget = self.state.budget;
        if let Some((_, charge)) = &displaced_charge {
            next_budget.release(charge.bytes);
        }
        next_budget.replace(
            &canonical_path,
            previous_charge.map(|charge| charge.bytes),
            bytes,
            self.state.limits,
        )?;
        self.record_candidate_change()?;
        self.state.budget = next_budget;
        if let Some((path, _)) = &displaced_charge {
            self.state.charged.remove(path);
        }
        self.state.charged.insert(
            canonical_path.clone(),
            FilesystemCharge {
                bytes,
                generation: binding_generation,
            },
        );
        self.state.candidates.insert(
            candidate.to_owned(),
            FilesystemCandidateBinding {
                canonical_path: canonical_path.clone(),
                generation: binding_generation,
            },
        );
        let binding = FilesystemResourceBinding {
            session_id: self.session_id,
            candidate_path: candidate.to_owned(),
            canonical_path: canonical_path.clone(),
            generation: binding_generation,
        };
        Ok(LoadedFilesystemSource {
            source_id,
            source,
            provenance: FilesystemProvenance { canonical_path },
            binding,
        })
    }

    fn release_binding(
        &mut self,
        binding: &FilesystemResourceBinding,
    ) -> Result<FilesystemReleaseOutcome, FilesystemDraftError> {
        if binding.session_id != self.session_id {
            return Err(FilesystemDraftError::ForeignBinding);
        }
        let Some(current) = self.state.candidates.get(&binding.candidate_path) else {
            return Ok(FilesystemReleaseOutcome::Missing);
        };
        if current.generation != binding.generation
            || current.canonical_path != binding.canonical_path
        {
            return Ok(FilesystemReleaseOutcome::Stale);
        }
        self.record_candidate_change()?;
        self.release_path(&binding.candidate_path);
        Ok(FilesystemReleaseOutcome::Released)
    }

    fn release_path(&mut self, path: &Path) {
        let last_canonical = self.state.candidates.remove(path).and_then(|binding| {
            (!self
                .state
                .candidates
                .values()
                .any(|other| other.canonical_path == binding.canonical_path))
            .then_some(binding.canonical_path)
        });
        if let Some(canonical) = &last_canonical
            && let Some(charge) = self.state.charged.remove(canonical)
        {
            self.state.budget.release(charge.bytes);
        }
        if let Ok(index) = self.root_index(path) {
            if let Some(canonical) = &last_canonical {
                self.state.sessions[index].remove_cached_text(canonical);
            }
            self.state.sessions[index].release_candidate(path);
        }
    }

    fn apply_not_found(&mut self, path: &Path, index: usize) -> Result<(), FilesystemDraftError> {
        let Some(binding) = self.state.candidates.get(path).cloned() else {
            return Ok(());
        };
        self.record_candidate_change()?;
        self.state.candidates.remove(path);
        self.state.sessions[index].remove_cached_text_if_unaliased(path, &binding.canonical_path);
        if self
            .state
            .candidates
            .values()
            .any(|other| other.canonical_path == binding.canonical_path)
        {
            return Ok(());
        }
        if let Some(charge) = self.state.charged.remove(&binding.canonical_path) {
            self.state.budget.release(charge.bytes);
        }
        Ok(())
    }

    fn reserve_binding_generation(&self) -> Result<u64, FilesystemDraftError> {
        self.binding_generations
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |generation| {
                generation.checked_add(1)
            })
            .map_err(|_| FilesystemDraftError::BindingGenerationExhausted)
    }
}

impl LocalFilesystemDraft {
    fn candidate(&self) -> &LocalFilesystemState {
        &self.candidate
    }

    fn mutation_cursor(&mut self) -> LocalFilesystemMutationCursor<'_> {
        LocalFilesystemMutationCursor {
            session_id: self.session_id,
            binding_generations: &self.binding_generations,
            job: Some(&self.job),
            state: &mut self.candidate,
        }
    }

    /// Refuses to start work once a failure has made this draft uncommittable.
    ///
    /// A poisoned draft can never be installed, so any further filesystem work it
    /// performs is spent on a result nobody can use. Refusing before the work
    /// starts keeps that waste out of the counters, and keeps the draft from
    /// taking binding generations that no commit will ever justify.
    fn ensure_operation_can_start(&self) -> Result<(), FilesystemDraftError> {
        if self.poisoned {
            return Err(FilesystemDraftError::PoisonedDraft);
        }
        Ok(())
    }

    fn record<T>(
        &mut self,
        result: Result<T, FilesystemDraftError>,
    ) -> Result<T, FilesystemDraftError> {
        if result.is_err() {
            self.poisoned = true;
        }
        result
    }

    pub const fn session_id(&self) -> LocalFilesystemSessionId {
        self.session_id
    }

    pub fn roots(&self) -> &[PathBuf] {
        &self.candidate().roots
    }

    pub fn limits(&self) -> FilesystemReadLimits {
        self.candidate().limits
    }

    /// Lists the AsciiDoc files below the roots as this draft would see them.
    ///
    /// Returns [`FilesystemDraftError::PoisonedDraft`] once an earlier operation
    /// has failed, because the listing could only feed a draft that can no longer
    /// be committed.
    pub fn discover_adoc_paths_with_control(
        &self,
        exclude_directory: impl FnMut(&Path, &Path) -> bool,
        is_cancelled: impl FnMut() -> bool,
    ) -> Result<Vec<PathBuf>, FilesystemDraftError> {
        self.ensure_operation_can_start()?;
        LocalFilesystemView {
            state: self.candidate(),
            job: Some((self.session_id, &self.job)),
        }
        .discover_adoc_paths_with_control(
            LocalFilesystemSession::MAX_SCAN_ENTRIES,
            exclude_directory,
            is_cancelled,
        )
        .map_err(FilesystemDraftError::from)
    }

    pub fn scan_utf8(
        &mut self,
        source_id: impl FnMut(&Path) -> Result<LogicalSourceId, ResourceError>,
    ) -> Result<Vec<LoadedFilesystemSource>, FilesystemDraftError> {
        self.ensure_operation_can_start()?;
        let result = self.mutation_cursor().scan_utf8(source_id);
        self.record(result)
    }

    pub fn read_utf8(
        &mut self,
        source_id: LogicalSourceId,
        path: &Path,
    ) -> Result<LoadedFilesystemSource, FilesystemDraftError> {
        self.ensure_operation_can_start()?;
        let result = self
            .mutation_cursor()
            .read_utf8_preserving_missing(source_id, path)
            .and_then(|outcome| outcome.into_loaded().map_err(FilesystemDraftError::from));
        self.record(result)
    }

    /// Reads one absolute path without poisoning this draft when it is absent.
    ///
    /// `NotFound` changes only this draft's candidate state. Dropping the draft
    /// preserves live state; committing it applies the absence.
    pub fn read_utf8_outcome(
        &mut self,
        source_id: LogicalSourceId,
        path: &Path,
    ) -> Result<FilesystemReadOutcome, FilesystemDraftError> {
        self.ensure_operation_can_start()?;
        let result = self.mutation_cursor().read_utf8(source_id, path);
        self.record(result)
    }

    /// Reads one absolute path while rejecting every symbolic link.
    ///
    /// This form is intended for policy-bearing files. `NotFound` keeps the
    /// draft usable, while a symbolic link or another read failure poisons it.
    pub fn read_utf8_no_symlinks_outcome(
        &mut self,
        source_id: LogicalSourceId,
        path: &Path,
    ) -> Result<FilesystemReadOutcome, FilesystemDraftError> {
        self.ensure_operation_can_start()?;
        let result = self
            .mutation_cursor()
            .read_utf8_no_symlinks(source_id, path);
        self.record(result)
    }

    pub fn reread_utf8(
        &mut self,
        source_id: LogicalSourceId,
        path: &Path,
    ) -> Result<LoadedFilesystemSource, FilesystemDraftError> {
        self.ensure_operation_can_start()?;
        let result = self
            .mutation_cursor()
            .reread_utf8_preserving_missing(source_id, path)
            .and_then(|outcome| outcome.into_loaded().map_err(FilesystemDraftError::from));
        self.record(result)
    }

    /// Reopens one absolute path without poisoning this draft when it is absent.
    ///
    /// `NotFound` changes only this draft's candidate state. Dropping the draft
    /// preserves live state; committing it applies the absence.
    pub fn reread_utf8_outcome(
        &mut self,
        source_id: LogicalSourceId,
        path: &Path,
    ) -> Result<FilesystemReadOutcome, FilesystemDraftError> {
        self.ensure_operation_can_start()?;
        let result = self.mutation_cursor().reread_utf8(source_id, path);
        self.record(result)
    }

    pub fn read_target_utf8(
        &mut self,
        source_id: LogicalSourceId,
        base: &Path,
        target: &str,
    ) -> Result<LoadedFilesystemSource, FilesystemDraftError> {
        self.ensure_operation_can_start()?;
        let result = self
            .mutation_cursor()
            .read_target_utf8_preserving_missing(source_id, base, target)
            .and_then(|outcome| outcome.into_loaded().map_err(FilesystemDraftError::from));
        self.record(result)
    }

    /// Resolves one authored target without poisoning this draft when it is absent.
    ///
    /// `NotFound` changes only this draft's candidate state. Dropping the draft
    /// preserves live state; committing it applies the absence.
    pub fn read_target_utf8_outcome(
        &mut self,
        source_id: LogicalSourceId,
        base: &Path,
        target: &str,
    ) -> Result<FilesystemReadOutcome, FilesystemDraftError> {
        self.ensure_operation_can_start()?;
        let result = self
            .mutation_cursor()
            .read_target_utf8(source_id, base, target);
        self.record(result)
    }

    /// Gives up this draft's claim on a resource it acquired earlier.
    ///
    /// Releasing performs no filesystem work, but it is still refused on a
    /// poisoned draft: the candidate state it would edit is already unusable.
    pub fn release_binding(
        &mut self,
        binding: &FilesystemResourceBinding,
    ) -> Result<FilesystemReleaseOutcome, FilesystemDraftError> {
        self.ensure_operation_can_start()?;
        let result = self.mutation_cursor().release_binding(binding);
        self.record(result)
    }

    pub fn budget(&self) -> ResourceBudget {
        self.candidate().budget
    }

    /// Verifies that this draft can be installed into `live` without mutation.
    fn validate(&self, live: &LocalFilesystemSession) -> Result<(), FilesystemDraftError> {
        self.job.ensure_active_job()?;
        if self.poisoned {
            return Err(FilesystemDraftError::PoisonedDraft);
        }
        if self.session_id != live.session_id
            || !Arc::ptr_eq(&self.lease.active, &live.active_draft)
            || !Arc::ptr_eq(&self.binding_generations, &live.next_binding_generation)
            || self.base_revision != live.revision
            || self.lease.active.load(Ordering::Acquire) != self.lease.token
        {
            return Err(FilesystemDraftError::InvalidDraft);
        }
        Ok(())
    }

    /// Validates the draft-local conditions required before state replacement.
    ///
    /// [`PreparedFilesystemCommit::commit`] separately checks the job lifecycle
    /// under the job lock because cancellation can occur after this method.
    pub fn prepare_commit(
        self,
        live: &mut LocalFilesystemSession,
    ) -> Result<PreparedFilesystemCommit<'_>, FilesystemDraftError> {
        self.validate(live)?;
        let next_revision = live
            .revision
            .checked_add(1)
            .ok_or(FilesystemDraftError::SessionRevisionExhausted)?;
        Ok(PreparedFilesystemCommit {
            live,
            candidate: self.candidate,
            next_revision,
            job: self.job,
            _lease: self.lease,
        })
    }
}

impl Drop for FilesystemDraftLease {
    fn drop(&mut self) {
        let _ = self
            .active
            .compare_exchange(self.token, 0, Ordering::AcqRel, Ordering::Acquire);
    }
}

impl PreparedFilesystemCommit<'_> {
    /// Installs the state while the originating job remains active.
    pub fn commit(self) -> Result<(), FilesystemDraftError> {
        self.job
            .with_active_commit(|| {
                self.live.state = self.candidate;
                self.live.revision = self.next_revision;
            })
            .map_err(FilesystemDraftError::from)
    }
}

fn read_candidate_with_optional_job(
    session: &mut LocalTargetSession,
    candidate: &Path,
    reuse_cached_text: bool,
    follow_symlinks: bool,
    after_open: impl FnOnce(),
    capacity: impl FnOnce(&Path) -> crate::local_target::CandidateReadCapacity,
    permit: Option<&mut FilesystemReadPermit>,
) -> Result<crate::local_target::LoadedLocalTarget, CoordinatedLocalTargetError> {
    match permit {
        Some(permit) => session.read_candidate_utf8_with_job_capacity(
            candidate,
            reuse_cached_text,
            follow_symlinks,
            after_open,
            capacity,
            permit,
        ),
        None => session
            .read_candidate_utf8_with_capacity(
                candidate,
                reuse_cached_text,
                follow_symlinks,
                after_open,
                capacity,
            )
            .map_err(CoordinatedLocalTargetError::Target),
    }
}

fn reread_candidate_with_optional_job(
    session: &mut LocalTargetSession,
    candidate: &Path,
    capacity: impl FnOnce(&Path) -> crate::local_target::CandidateReadCapacity,
    permit: Option<&mut FilesystemReadPermit>,
) -> Result<
    (
        crate::local_target::LoadedLocalTarget,
        LocalTargetTextRollback,
    ),
    CoordinatedLocalTargetError,
> {
    match permit {
        Some(permit) => {
            session.reread_candidate_utf8_with_job_capacity(candidate, permit, capacity)
        }
        None => session
            .reread_candidate_utf8_with_capacity(candidate, capacity)
            .map_err(CoordinatedLocalTargetError::Target),
    }
}

fn map_coordinated_read_error(
    error: CoordinatedLocalTargetError,
    limits: FilesystemReadLimits,
    file_limit_denied: bool,
) -> FilesystemDraftError {
    match error {
        CoordinatedLocalTargetError::Target(source) => {
            map_shared_read_error(source, limits, file_limit_denied).into()
        }
        CoordinatedLocalTargetError::Job(source) => source.into(),
    }
}

fn shared_read_capacity(
    mut budget: ResourceBudget,
    charged: &BTreeMap<PathBuf, FilesystemCharge>,
    candidates: &BTreeMap<PathBuf, FilesystemCandidateBinding>,
    limits: FilesystemReadLimits,
    candidate: &Path,
    canonical: &Path,
    file_limit_denied: &std::cell::Cell<bool>,
) -> crate::local_target::CandidateReadCapacity {
    if let Some(previous) = candidates.get(candidate)
        && previous.canonical_path != canonical
        && !candidates.iter().any(|(other, resolved)| {
            other.as_path() != candidate && resolved.canonical_path == previous.canonical_path
        })
        && let Some(charge) = charged.get(&previous.canonical_path)
    {
        budget.release(charge.bytes);
    }
    let previous = charged.get(canonical).copied();
    let allow_file = previous.is_some() || budget.files() < limits.max_files;
    file_limit_denied.set(!allow_file);
    let retained = previous
        .and_then(|charge| budget.bytes().checked_sub(charge.bytes))
        .unwrap_or(budget.bytes());
    crate::local_target::CandidateReadCapacity {
        allow_file,
        max_total_bytes: limits.max_total_bytes.saturating_sub(retained),
        max_resource_bytes: limits.max_resource_bytes,
    }
}

fn map_shared_read_error(
    error: LocalTargetError,
    limits: FilesystemReadLimits,
    file_limit_denied: bool,
) -> ResourceError {
    if file_limit_denied && matches!(error, LocalTargetError::ReadLimitExceeded) {
        ResourceError::FileLimit {
            limit: limits.max_files,
        }
    } else {
        ResourceError::from(error)
    }
}

#[cfg(test)]
mod tests;
