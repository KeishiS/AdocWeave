use std::collections::{BTreeMap, VecDeque};
use std::error::Error;
use std::fmt;
#[cfg(not(target_os = "linux"))]
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::io_observation::FilesystemIoMeter;
use crate::local_target::{
    FilesystemRaceResistance, LocalTargetCandidateRollback, LocalTargetError, LocalTargetPolicy,
    LocalTargetSession, LocalTargetTextRollback,
};

/// Maximum number of directory authorities retained by one policy.
///
/// A Linux authority owns one file descriptor per root. This bound is kept
/// separate from the number of files a session may read so configuration alone
/// cannot exhaust the process file-descriptor table before any read begins.
const MAX_FILESYSTEM_POLICY_ROOTS: usize = 128;

/// Bounds applied while the host discovers and reads filesystem resources.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FilesystemReadLimits {
    /// Maximum number of filesystem resources charged to one session.
    pub max_files: usize,
    /// Maximum combined bytes charged to one session.
    pub max_total_bytes: u64,
    /// Maximum bytes read from one filesystem resource.
    pub max_resource_bytes: u64,
}

impl Default for FilesystemReadLimits {
    fn default() -> Self {
        Self {
            max_files: 10_000,
            max_total_bytes: 50 * 1024 * 1024,
            max_resource_bytes: 10 * 1024 * 1024,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LocalFilesystemPolicy {
    roots: Vec<PathBuf>,
    root_policies: Vec<LocalTargetPolicy>,
    limits: FilesystemReadLimits,
}

/// Immutable selection of retained filesystem roots and read limits.
///
/// The selected root handles, their path identities and limits travel as one
/// value, so callers cannot accidentally pair roots from another authority.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LocalFilesystemAccess {
    roots: Vec<PathBuf>,
    root_policies: Vec<LocalTargetPolicy>,
    limits: FilesystemReadLimits,
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
/// validates the identity and revision before a
/// separate infallible commit installs the live resource state.
#[must_use = "a filesystem draft must be committed or dropped"]
#[derive(Debug)]
pub struct LocalFilesystemDraft {
    session_id: LocalFilesystemSessionId,
    base_revision: u64,
    candidate: LocalFilesystemState,
    lease: FilesystemDraftLease,
    binding_generations: Arc<AtomicU64>,
    poisoned: bool,
}

#[derive(Debug)]
struct FilesystemDraftLease {
    active: Arc<AtomicU64>,
    token: u64,
}

/// A filesystem state replacement whose commit path cannot fail.
#[must_use = "a prepared filesystem commit must be committed or dropped"]
pub struct PreparedFilesystemCommit<'a> {
    live: &'a mut LocalFilesystemSession,
    candidate: LocalFilesystemState,
    next_revision: u64,
    _lease: FilesystemDraftLease,
}

/// Opaque state used to undo one filesystem reread and its command snapshot.
#[derive(Clone, Debug)]
pub struct FilesystemReadRollback {
    session_id: LocalFilesystemSessionId,
    applied_generation: u64,
    canonical_path: PathBuf,
    candidate_path: PathBuf,
    session_index: usize,
    candidate_rollback: LocalTargetCandidateRollback,
    accounting: FilesystemAccountingRollback,
    text_rollback: LocalTargetTextRollback,
}

#[derive(Clone, Debug)]
struct FilesystemAccountingRollback {
    previous_candidate: Option<FilesystemCandidateBinding>,
    previous_charge: Option<FilesystemCharge>,
    displaced_charge: Option<(PathBuf, FilesystemCharge)>,
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
    meter: FilesystemIoMeter,
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
            meter: self.meter.clone(),
            #[cfg(test)]
            clone_count: Arc::clone(&self.clone_count),
        }
    }
}

struct LocalFilesystemView<'a> {
    state: &'a LocalFilesystemState,
}

struct LocalFilesystemMutationCursor<'a> {
    session_id: LocalFilesystemSessionId,
    binding_generations: &'a Arc<AtomicU64>,
    state: &'a mut LocalFilesystemState,
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

    /// Selects an immutable access set from roots this policy already retains.
    pub fn access_existing(
        &self,
        roots: impl IntoIterator<Item = PathBuf>,
        limits: FilesystemReadLimits,
    ) -> Result<LocalFilesystemAccess, ResourceError> {
        validate_derived_limits(self.limits, limits)?;
        let mut policies = roots
            .into_iter()
            .map(|root| {
                self.root_policy(&root)
                    .cloned()
                    .ok_or(ResourceError::OutsideRoots(root))
            })
            .collect::<Result<Vec<_>, _>>()?;
        policies.sort_by(|left, right| left.root().cmp(right.root()));
        policies.dedup_by(|left, right| left.root() == right.root());
        LocalFilesystemAccess::from_policies(policies, limits)
    }

    /// Extends the retained authority transactionally and returns one opaque
    /// access set for the requested roots.
    pub fn access_derived(
        &mut self,
        anchor: &Path,
        roots: DerivedFilesystemRoots,
        limits: FilesystemReadLimits,
    ) -> Result<LocalFilesystemAccess, ResourceError> {
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

    pub fn session(&self) -> Result<LocalFilesystemSession, ResourceError> {
        LocalFilesystemAccess::from_policies(self.root_policies.clone(), self.limits)?.session()
    }
}

impl LocalFilesystemAccess {
    fn from_policies(
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

    /// Returns the logical paths paired with the retained root authorities.
    pub fn roots(&self) -> &[PathBuf] {
        &self.roots
    }

    /// Returns the limits applied independently to each new session.
    pub const fn limits(&self) -> FilesystemReadLimits {
        self.limits
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
        let meter = FilesystemIoMeter::detached();
        let sessions = self
            .root_policies
            .iter()
            .cloned()
            .map(|policy| {
                LocalTargetSession::metered(
                    policy,
                    self.limits.max_files,
                    FilesystemReadLimits {
                        max_files: usize::MAX,
                        max_total_bytes: u64::MAX,
                        max_resource_bytes: self.limits.max_resource_bytes,
                    },
                    meter.clone(),
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
                meter,
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
    const MAX_SCAN_ENTRIES: usize = 100_000;

    pub const fn id(&self) -> LocalFilesystemSessionId {
        self.session_id
    }

    /// Creates an isolated candidate state without changing this live session.
    pub fn draft(&self) -> Result<LocalFilesystemDraft, FilesystemDraftError> {
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
        LocalFilesystemView { state: &self.state }.discover_adoc_paths_with_control(
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
        LocalFilesystemView { state: &self.state }.discover_adoc_paths_with_control(
            scan_entry_limit,
            exclude_directory,
            is_cancelled,
        )
    }
}

impl LocalFilesystemView<'_> {
    fn discover_adoc_paths_with_control(
        &self,
        scan_entry_limit: usize,
        exclude_directory: impl FnMut(&Path, &Path) -> bool,
        is_cancelled: impl FnMut() -> bool,
    ) -> Result<Vec<PathBuf>, ResourceError> {
        #[cfg(target_os = "linux")]
        {
            self.discover_adoc_paths_with_limit_handle_relative(
                scan_entry_limit,
                exclude_directory,
                is_cancelled,
            )
        }
        #[cfg(not(target_os = "linux"))]
        {
            let mut exclude_directory = exclude_directory;
            let mut is_cancelled = is_cancelled;
            let mut paths = Vec::new();
            let mut scanned_entries = 0_usize;
            for root in &self.state.roots {
                let mut pending = VecDeque::from([root.clone()]);
                while let Some(path) = pending.pop_front() {
                    if is_cancelled() {
                        return Err(ResourceError::Unverifiable(
                            "local filesystem scan was cancelled".to_owned(),
                        ));
                    }
                    let metadata =
                        fs::symlink_metadata(&path).map_err(|source| ResourceError::Inspect {
                            path: path.clone(),
                            source: source.to_string(),
                        })?;
                    if metadata.file_type().is_symlink() {
                        continue;
                    }
                    if metadata.is_dir() {
                        if path != *root
                            && let Ok(relative) = path.strip_prefix(root)
                            && exclude_directory(root, relative)
                        {
                            continue;
                        }
                        self.state.meter.observe_directory_read();
                        let mut children = Vec::new();
                        let directory =
                            fs::read_dir(&path).map_err(|source| ResourceError::Inspect {
                                path: path.clone(),
                                source: source.to_string(),
                            })?;
                        for child in directory {
                            self.state.meter.observe_directory_entry();
                            let child = child.map_err(|source| ResourceError::Inspect {
                                path: path.clone(),
                                source: source.to_string(),
                            })?;
                            if is_cancelled() {
                                return Err(ResourceError::Unverifiable(
                                    "local filesystem scan was cancelled".to_owned(),
                                ));
                            }
                            children.push(child);
                            scanned_entries += 1;
                            if scanned_entries > scan_entry_limit {
                                return Err(ResourceError::ScanEntryLimit {
                                    limit: scan_entry_limit,
                                });
                            }
                        }
                        children.sort_by_key(fs::DirEntry::file_name);
                        pending.extend(children.into_iter().map(|entry| entry.path()));
                    } else if path.extension().and_then(|value| value.to_str()) == Some("adoc") {
                        paths.push(path);
                    }
                }
            }
            paths.sort();
            paths.dedup();
            Ok(paths)
        }
    }

    #[cfg(target_os = "linux")]
    fn discover_adoc_paths_with_limit_handle_relative(
        &self,
        scan_entry_limit: usize,
        mut exclude_directory: impl FnMut(&Path, &Path) -> bool,
        mut is_cancelled: impl FnMut() -> bool,
    ) -> Result<Vec<PathBuf>, ResourceError> {
        use std::ffi::OsString;
        use std::os::unix::ffi::OsStringExt;

        use rustix::fs::{AtFlags, Dir, FileType, statat};

        let mut paths = Vec::new();
        let mut scanned_entries = 0_usize;
        for (root, session) in self.state.roots.iter().zip(&self.state.sessions) {
            let policy = session.policy();
            let mut pending = VecDeque::from([root.clone()]);
            while let Some(path) = pending.pop_front() {
                if is_cancelled() {
                    return Err(ResourceError::Unverifiable(
                        "local filesystem scan was cancelled".to_owned(),
                    ));
                }
                if path != *root
                    && let Ok(relative) = path.strip_prefix(root)
                    && exclude_directory(root, relative)
                {
                    continue;
                }
                let directory = policy
                    .open_directory_no_symlinks(&path)
                    .map_err(ResourceError::from)?;
                self.state.meter.observe_directory_read();
                let mut entries =
                    Dir::read_from(&directory).map_err(|source| ResourceError::Inspect {
                        path: path.clone(),
                        source: source.to_string(),
                    })?;
                let mut children = Vec::<(OsString, FileType)>::new();
                for child in &mut entries {
                    self.state.meter.observe_directory_entry();
                    let child = child.map_err(|source| ResourceError::Inspect {
                        path: path.clone(),
                        source: source.to_string(),
                    })?;
                    if is_cancelled() {
                        return Err(ResourceError::Unverifiable(
                            "local filesystem scan was cancelled".to_owned(),
                        ));
                    }
                    let name = child.file_name();
                    if name.to_bytes() == b"." || name.to_bytes() == b".." {
                        continue;
                    }
                    let name = OsString::from_vec(name.to_bytes().to_vec());
                    let child_path = path.join(&name);
                    let metadata =
                        statat(&directory, &name, AtFlags::SYMLINK_NOFOLLOW).map_err(|source| {
                            ResourceError::Inspect {
                                path: child_path,
                                source: source.to_string(),
                            }
                        })?;
                    let file_type = FileType::from_raw_mode(metadata.st_mode);
                    children.push((name, file_type));
                    scanned_entries = scanned_entries.saturating_add(1);
                    if scanned_entries > scan_entry_limit {
                        return Err(ResourceError::ScanEntryLimit {
                            limit: scan_entry_limit,
                        });
                    }
                }
                children.sort_by(|left, right| left.0.cmp(&right.0));
                for (name, file_type) in children {
                    let child = path.join(name);
                    if file_type == FileType::Symlink {
                        continue;
                    }
                    if file_type == FileType::Directory {
                        pending.push_back(child);
                    } else if file_type == FileType::RegularFile
                        && child.extension().and_then(|value| value.to_str()) == Some("adoc")
                    {
                        paths.push(child);
                    }
                }
            }
        }
        paths.sort();
        paths.dedup();
        Ok(paths)
    }
}

impl LocalFilesystemSession {
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
            .reread_utf8(source_id, path)
            .map_err(ResourceError::from)
    }
    /// Reopens a path and returns the state needed to undo its budget charge
    /// and cached command snapshot.
    ///
    /// Callers which update another state store after reading must retain the
    /// rollback value until that update commits.
    pub fn reread_utf8_with_rollback(
        &mut self,
        source_id: LogicalSourceId,
        path: &Path,
    ) -> Result<(LoadedFilesystemSource, FilesystemReadRollback), ResourceError> {
        self.invalidate_active_draft();
        self.reread_utf8_with_rollback_in_place(source_id, path)
    }

    fn reread_utf8_with_rollback_in_place(
        &mut self,
        source_id: LogicalSourceId,
        path: &Path,
    ) -> Result<(LoadedFilesystemSource, FilesystemReadRollback), ResourceError> {
        let index = self.root_index(path)?;
        let candidate_rollback = self.state.sessions[index].candidate_rollback(path);
        let binding_generation = self.reserve_binding_generation()?;
        let state = &mut self.state;
        let budget = state.budget;
        let charged = &state.charged;
        let candidates = &state.candidates;
        let limits = state.limits;
        let file_limit_denied = std::cell::Cell::new(false);
        let (loaded, text_rollback) = match state.sessions[index]
            .reread_candidate_utf8_with_capacity(path, |canonical| {
                shared_read_capacity(
                    budget,
                    charged,
                    candidates,
                    limits,
                    path,
                    canonical,
                    &file_limit_denied,
                )
            }) {
            Ok(loaded) => loaded,
            Err(error) => {
                state.sessions[index].rollback_candidate(candidate_rollback);
                return Err(map_shared_read_error(
                    error,
                    limits,
                    file_limit_denied.get(),
                ));
            }
        };
        let canonical_path = loaded.canonical_path().to_owned();
        match self.finish_read(self.session_id, binding_generation, source_id, path, loaded) {
            Ok((loaded, accounting)) => {
                let applied_generation = self
                    .state
                    .charged
                    .get(&canonical_path)
                    .expect("successful read records its charge")
                    .generation;
                Ok((
                    loaded,
                    FilesystemReadRollback {
                        session_id: self.session_id,
                        applied_generation,
                        canonical_path,
                        candidate_path: path.to_owned(),
                        session_index: index,
                        candidate_rollback,
                        accounting,
                        text_rollback,
                    },
                ))
            }
            Err(error) => {
                self.state.sessions[index].rollback_cached_text(text_rollback);
                self.state.sessions[index].rollback_candidate(candidate_rollback);
                Err(error)
            }
        }
    }

    /// Restores the charge and cached command snapshot replaced by
    /// [`Self::reread_utf8_with_rollback`].
    pub fn rollback_reread(
        &mut self,
        rollback: FilesystemReadRollback,
    ) -> Result<(), ResourceError> {
        self.invalidate_active_draft();
        if rollback.session_id != self.session_id {
            return Err(ResourceError::InvalidRollback);
        }
        let Some(current) = self.state.charged.get(&rollback.canonical_path).copied() else {
            return Err(ResourceError::InvalidRollback);
        };
        if current.generation != rollback.applied_generation {
            return Err(ResourceError::InvalidRollback);
        }
        if !self
            .state
            .candidates
            .get(&rollback.candidate_path)
            .is_some_and(|binding| binding.canonical_path == rollback.canonical_path)
        {
            return Err(ResourceError::InvalidRollback);
        }
        if let Some((path, _)) = &rollback.accounting.displaced_charge
            && self.state.charged.contains_key(path)
        {
            return Err(ResourceError::InvalidRollback);
        }
        match rollback.accounting.previous_charge {
            Some(previous) => {
                self.state
                    .budget
                    .restore_replacement(current.bytes, previous.bytes);
                self.state
                    .charged
                    .insert(rollback.canonical_path.clone(), previous);
            }
            None => {
                self.state.budget.release(current.bytes);
                self.state.charged.remove(&rollback.canonical_path);
            }
        }
        if let Some((path, charge)) = rollback.accounting.displaced_charge {
            self.state.budget.restore_charge(charge.bytes);
            self.state.charged.insert(path, charge);
        }
        match rollback.accounting.previous_candidate {
            Some(previous) => {
                self.state
                    .candidates
                    .insert(rollback.candidate_path.clone(), previous);
            }
            None => {
                self.state.candidates.remove(&rollback.candidate_path);
            }
        }
        self.state.sessions[rollback.session_index].rollback_cached_text(rollback.text_rollback);
        self.state.sessions[rollback.session_index].rollback_candidate(rollback.candidate_rollback);
        Ok(())
    }

    /// Releases a path through the migration-only compatibility API.
    ///
    /// This operation has no binding-generation protection. Do not mix it with
    /// [`LocalFilesystemDraft::release_binding`]; callers adopting bindings
    /// must release resources through that generation-checked API instead.
    pub fn release(&mut self, path: &Path) {
        self.invalidate_active_draft();
        self.mutation_cursor().release_path(path);
    }

    fn mutation_cursor(&mut self) -> LocalFilesystemMutationCursor<'_> {
        LocalFilesystemMutationCursor {
            session_id: self.session_id,
            binding_generations: &self.next_binding_generation,
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
        let mut draft = self.draft()?;
        let loaded = draft
            .mutation_cursor()
            .read_utf8_with(source_id, path, false, after_open)?;
        draft.prepare_commit(self)?.commit();
        Ok(loaded)
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
        .map(|(loaded, _)| loaded)
    }

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

    fn finish_read(
        &mut self,
        session_id: LocalFilesystemSessionId,
        binding_generation: u64,
        source_id: LogicalSourceId,
        candidate: &Path,
        loaded: crate::local_target::LoadedLocalTarget,
    ) -> Result<(LoadedFilesystemSource, FilesystemAccountingRollback), ResourceError> {
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
        Ok((
            LoadedFilesystemSource {
                source_id,
                source,
                provenance: FilesystemProvenance { canonical_path },
                binding,
            },
            FilesystemAccountingRollback {
                previous_candidate,
                previous_charge,
                displaced_charge,
            },
        ))
    }

    pub const fn budget(&self) -> ResourceBudget {
        self.state.budget
    }

    fn reserve_binding_generation(&mut self) -> Result<u64, FilesystemDraftError> {
        self.next_binding_generation
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |generation| {
                generation.checked_add(1)
            })
            .map_err(|_| FilesystemDraftError::BindingGenerationExhausted)
    }
}

impl LocalFilesystemMutationCursor<'_> {
    fn scan_utf8(
        &mut self,
        mut source_id: impl FnMut(&Path) -> Result<LogicalSourceId, ResourceError>,
    ) -> Result<Vec<LoadedFilesystemSource>, FilesystemDraftError> {
        let paths = LocalFilesystemView { state: self.state }.discover_adoc_paths_with_control(
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
        paths
            .into_iter()
            .map(|path| {
                let source_id = source_id(&path)?;
                self.read_utf8(source_id, &path)
            })
            .collect()
    }

    fn read_utf8(
        &mut self,
        source_id: LogicalSourceId,
        path: &Path,
    ) -> Result<LoadedFilesystemSource, FilesystemDraftError> {
        self.read_utf8_with(source_id, path, false, || {})
    }

    fn read_target_utf8(
        &mut self,
        source_id: LogicalSourceId,
        base: &Path,
        target: &str,
    ) -> Result<LoadedFilesystemSource, FilesystemDraftError> {
        self.state.meter.observe_read_operation();
        let index = self.root_index(base)?;
        let candidate = self.state.sessions[index]
            .candidate(base, target)
            .map_err(ResourceError::from)?;
        let binding_generation = self.reserve_binding_generation()?;
        let budget = self.state.budget;
        let charged = &self.state.charged;
        let candidates = &self.state.candidates;
        let limits = self.state.limits;
        let file_limit_denied = std::cell::Cell::new(false);
        let loaded = self.state.sessions[index]
            .read_candidate_utf8_with_capacity(
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
            )
            .map_err(|error| map_shared_read_error(error, limits, file_limit_denied.get()))?;
        self.finish_read(binding_generation, source_id, &candidate, loaded)
            .map(|(loaded, _)| loaded)
    }

    fn reread_utf8(
        &mut self,
        source_id: LogicalSourceId,
        path: &Path,
    ) -> Result<LoadedFilesystemSource, FilesystemDraftError> {
        self.state.meter.observe_read_operation();
        let index = self.root_index(path)?;
        let candidate_rollback = self.state.sessions[index].candidate_rollback(path);
        let binding_generation = self.reserve_binding_generation()?;
        let budget = self.state.budget;
        let charged = &self.state.charged;
        let candidates = &self.state.candidates;
        let limits = self.state.limits;
        let file_limit_denied = std::cell::Cell::new(false);
        let (loaded, text_rollback) = match self.state.sessions[index]
            .reread_candidate_utf8_with_capacity(path, |canonical| {
                shared_read_capacity(
                    budget,
                    charged,
                    candidates,
                    limits,
                    path,
                    canonical,
                    &file_limit_denied,
                )
            }) {
            Ok(loaded) => loaded,
            Err(error) => {
                self.state.sessions[index].rollback_candidate(candidate_rollback);
                return Err(map_shared_read_error(error, limits, file_limit_denied.get()).into());
            }
        };
        match self.finish_read(binding_generation, source_id, path, loaded) {
            Ok((loaded, _)) => Ok(loaded),
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
    ) -> Result<LoadedFilesystemSource, FilesystemDraftError> {
        self.state.meter.observe_read_operation();
        if !path.is_absolute() {
            return Err(ResourceError::PathNotAbsolute(path.to_owned()).into());
        }
        let index = self.root_index(path)?;
        let candidate = path.to_owned();
        if candidate == self.state.roots[index] {
            return Err(ResourceError::NotRegularFile(candidate).into());
        }
        let binding_generation = self.reserve_binding_generation()?;
        let budget = self.state.budget;
        let charged = &self.state.charged;
        let candidates = &self.state.candidates;
        let limits = self.state.limits;
        let file_limit_denied = std::cell::Cell::new(false);
        let loaded = self.state.sessions[index]
            .read_candidate_utf8_with_capacity(
                &candidate,
                reuse_cached_text,
                true,
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
            )
            .map_err(|error| map_shared_read_error(error, limits, file_limit_denied.get()))?;
        self.finish_read(binding_generation, source_id, &candidate, loaded)
            .map(|(loaded, _)| loaded)
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
    ) -> Result<(LoadedFilesystemSource, FilesystemAccountingRollback), FilesystemDraftError> {
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
            session_id: self.session_id,
            candidate_path: candidate.to_owned(),
            canonical_path: canonical_path.clone(),
            generation: binding_generation,
        };
        Ok((
            LoadedFilesystemSource {
                source_id,
                source,
                provenance: FilesystemProvenance { canonical_path },
                binding,
            },
            FilesystemAccountingRollback {
                previous_candidate,
                previous_charge,
                displaced_charge,
            },
        ))
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
        self.release_path(&binding.candidate_path);
        Ok(FilesystemReleaseOutcome::Released)
    }

    fn release_path(&mut self, path: &Path) {
        if let Some(binding) = self.state.candidates.remove(path)
            && !self
                .state
                .candidates
                .values()
                .any(|other| other.canonical_path == binding.canonical_path)
            && let Some(charge) = self.state.charged.remove(&binding.canonical_path)
        {
            self.state.budget.release(charge.bytes);
        }
        if let Ok(index) = self.root_index(path) {
            self.state.sessions[index].release_candidate(path);
        }
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
        let result = self.mutation_cursor().read_utf8(source_id, path);
        self.record(result)
    }

    pub fn reread_utf8(
        &mut self,
        source_id: LogicalSourceId,
        path: &Path,
    ) -> Result<LoadedFilesystemSource, FilesystemDraftError> {
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

    /// Validates every condition which could prevent a state replacement.
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
    /// Installs the state validated by [`LocalFilesystemDraft::prepare_commit`].
    pub fn commit(self) {
        self.live.state = self.candidate;
        self.live.revision = self.next_revision;
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
    let allow_file = previous.is_some() || budget.files < limits.max_files;
    file_limit_denied.set(!allow_file);
    let retained = previous
        .and_then(|charge| budget.bytes.checked_sub(charge.bytes))
        .unwrap_or(budget.bytes);
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

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ResourceBudget {
    files: usize,
    bytes: u64,
}

impl ResourceBudget {
    pub fn charge(
        &mut self,
        path: &Path,
        bytes: u64,
        limits: FilesystemReadLimits,
    ) -> Result<(), ResourceError> {
        if bytes > limits.max_resource_bytes {
            return Err(ResourceError::ResourceTooLarge(path.to_owned()));
        }
        let files = self.files.checked_add(1).ok_or(ResourceError::FileLimit {
            limit: limits.max_files,
        })?;
        if files > limits.max_files {
            return Err(ResourceError::FileLimit {
                limit: limits.max_files,
            });
        }
        let total = self
            .bytes
            .checked_add(bytes)
            .ok_or(ResourceError::ByteLimit)?;
        if total > limits.max_total_bytes {
            return Err(ResourceError::ByteLimit);
        }
        self.files = files;
        self.bytes = total;
        Ok(())
    }

    fn replace(
        &mut self,
        path: &Path,
        previous: Option<u64>,
        bytes: u64,
        limits: FilesystemReadLimits,
    ) -> Result<(), ResourceError> {
        let Some(previous) = previous else {
            return self.charge(path, bytes, limits);
        };
        if bytes > limits.max_resource_bytes {
            return Err(ResourceError::ResourceTooLarge(path.to_owned()));
        }
        let retained = self
            .bytes
            .checked_sub(previous)
            .expect("charged bytes are part of the total");
        let total = retained
            .checked_add(bytes)
            .ok_or(ResourceError::ByteLimit)?;
        if total > limits.max_total_bytes {
            return Err(ResourceError::ByteLimit);
        }
        self.bytes = total;
        Ok(())
    }

    fn restore_replacement(&mut self, current: u64, previous: u64) {
        self.bytes = self
            .bytes
            .checked_sub(current)
            .and_then(|bytes| bytes.checked_add(previous))
            .expect("replacement charge is part of the budget");
    }

    fn restore_charge(&mut self, bytes: u64) {
        self.files = self
            .files
            .checked_add(1)
            .expect("restored file count fits the original budget");
        self.bytes = self
            .bytes
            .checked_add(bytes)
            .expect("restored bytes fit the original budget");
    }

    fn release(&mut self, bytes: u64) {
        self.files = self
            .files
            .checked_sub(1)
            .expect("released file was charged");
        self.bytes = self
            .bytes
            .checked_sub(bytes)
            .expect("released bytes were charged");
    }

    pub const fn files(self) -> usize {
        self.files
    }

    pub const fn bytes(self) -> u64 {
        self.bytes
    }
}

/// An error raised while creating, mutating, or committing a filesystem draft.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum FilesystemDraftError {
    SessionRevisionExhausted,
    BindingGenerationExhausted,
    DraftBusy,
    InvalidDraft,
    PoisonedDraft,
    ForeignBinding,
    Resource(ResourceError),
}

impl fmt::Display for FilesystemDraftError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::SessionRevisionExhausted => {
                formatter.write_str("filesystem session revision space is exhausted")
            }
            Self::BindingGenerationExhausted => {
                formatter.write_str("filesystem binding generation space is exhausted")
            }
            Self::DraftBusy => {
                formatter.write_str("filesystem session already has an active draft")
            }
            Self::InvalidDraft => {
                formatter.write_str("filesystem draft is stale or belongs to another session")
            }
            Self::PoisonedDraft => {
                formatter.write_str("filesystem draft contains a failed operation")
            }
            Self::ForeignBinding => {
                formatter.write_str("filesystem binding belongs to another session")
            }
            Self::Resource(source) => source.fmt(formatter),
        }
    }
}

impl Error for FilesystemDraftError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Resource(source) => Some(source),
            _ => None,
        }
    }
}

impl From<ResourceError> for FilesystemDraftError {
    fn from(source: ResourceError) -> Self {
        Self::Resource(source)
    }
}

impl From<FilesystemDraftError> for ResourceError {
    fn from(error: FilesystemDraftError) -> Self {
        match error {
            FilesystemDraftError::Resource(source) => source,
            lifecycle => Self::Unverifiable(lifecycle.to_string()),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ResourceError {
    NoRoots,
    InvalidRoot,
    InvalidSourceId,
    SessionIdentityExhausted,
    InvalidRollback,
    Missing(PathBuf),
    PermissionDenied(PathBuf),
    PathNotAbsolute(PathBuf),
    OutsideRoots(PathBuf),
    NotRegularFile(PathBuf),
    Inspect { path: PathBuf, source: String },
    Read { path: PathBuf, source: String },
    InvalidUtf8 { path: PathBuf, source: String },
    ResourceTooLarge(PathBuf),
    RootLimit { limit: usize },
    FileLimit { limit: usize },
    ScanEntryLimit { limit: usize },
    ByteLimit,
    Unverifiable(String),
}

impl fmt::Display for ResourceError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoRoots => formatter.write_str("no local resource roots were configured"),
            Self::InvalidRoot => formatter.write_str("local resource root is not a directory"),
            Self::InvalidSourceId => formatter.write_str("local source ID is invalid"),
            Self::SessionIdentityExhausted => {
                formatter.write_str("filesystem session identity space is exhausted")
            }
            Self::InvalidRollback => formatter
                .write_str("filesystem reread rollback is stale or belongs to another session"),
            Self::Missing(path) => {
                write!(formatter, "local resource is missing: {}", path.display())
            }
            Self::PermissionDenied(path) => {
                write!(formatter, "permission denied reading {}", path.display())
            }
            Self::PathNotAbsolute(path) => write!(
                formatter,
                "local resource path is not absolute: {}",
                path.display()
            ),
            Self::OutsideRoots(path) => write!(
                formatter,
                "local resource is outside configured roots: {}",
                path.display()
            ),
            Self::NotRegularFile(path) => write!(
                formatter,
                "local resource is not a regular file: {}",
                path.display()
            ),
            Self::Inspect { path, source } => {
                write!(formatter, "cannot inspect {}: {source}", path.display())
            }
            Self::Read { path, source } => {
                write!(formatter, "cannot read {}: {source}", path.display())
            }
            Self::InvalidUtf8 { path, source } => write!(
                formatter,
                "cannot read {} as UTF-8: {source}",
                path.display()
            ),
            Self::ResourceTooLarge(path) => {
                write!(formatter, "local resource is too large: {}", path.display())
            }
            Self::RootLimit { limit } => {
                write!(formatter, "local resource root limit exceeded: {limit}")
            }
            Self::FileLimit { limit } => {
                write!(formatter, "local resource file limit exceeded: {limit}")
            }
            Self::ScanEntryLimit { limit } => {
                write!(
                    formatter,
                    "local filesystem scan entry limit exceeded: {limit}"
                )
            }
            Self::ByteLimit => formatter.write_str("local resource byte limit exceeded"),
            Self::Unverifiable(reason) => {
                write!(formatter, "local resource cannot be verified: {reason}")
            }
        }
    }
}

impl Error for ResourceError {}

impl From<LocalTargetError> for ResourceError {
    fn from(error: LocalTargetError) -> Self {
        match error {
            LocalTargetError::Missing(path) => Self::Missing(path),
            LocalTargetError::OutsideRoot(path) => Self::OutsideRoots(path),
            LocalTargetError::NotFile(path) | LocalTargetError::NotDirectory(path) => {
                Self::NotRegularFile(path)
            }
            LocalTargetError::PermissionDenied(path) => Self::PermissionDenied(path),
            LocalTargetError::InvalidUtf8(path) => Self::InvalidUtf8 {
                path,
                source: "input is not valid UTF-8".to_owned(),
            },
            LocalTargetError::Unverifiable(source) => Self::Unverifiable(source),
            LocalTargetError::LimitExceeded { limit } => Self::FileLimit { limit },
            LocalTargetError::ResourceTooLarge(path) => Self::ResourceTooLarge(path),
            LocalTargetError::ReadLimitExceeded => Self::ByteLimit,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::io_observation::FilesystemIoUsage;
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    struct TestDir(PathBuf);

    impl TestDir {
        fn new(label: &str) -> Self {
            let nonce = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("clock after epoch")
                .as_nanos();
            let path = std::env::temp_dir().join(format!(
                "adocweave-host-{label}-{}-{nonce}",
                std::process::id()
            ));
            fs::create_dir(&path).expect("create test directory");
            Self(path)
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TestDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn policy(root: &Path, max_resource_bytes: u64) -> LocalFilesystemPolicy {
        LocalFilesystemPolicy::new(
            [root.to_owned()],
            FilesystemReadLimits {
                max_files: 10,
                max_total_bytes: 100,
                max_resource_bytes,
            },
        )
        .expect("valid policy")
    }

    fn source_id() -> LogicalSourceId {
        LogicalSourceId::new("test-source").expect("source ID")
    }

    /// The draft shares the meter of the session it was cloned from, so reading
    /// it after the draft is gone still reports the draft's work.
    fn draft_meter(draft: &LocalFilesystemDraft) -> FilesystemIoMeter {
        draft.candidate().meter.clone()
    }

    #[test]
    fn filesystem_draft_is_isolated_until_commit_and_drop_discards_it() {
        let root = TestDir::new("filesystem-draft-isolation");
        let path = root.path().join("source.adoc");
        fs::write(&path, "a").expect("source");
        let mut session = policy(root.path(), 100).session().expect("session");
        session.read_utf8(source_id(), &path).expect("initial read");

        fs::write(&path, "bb").expect("replacement");
        let mut discarded = session.draft().expect("discarded draft");
        discarded
            .reread_utf8(source_id(), &path)
            .expect("draft reread");
        assert_eq!(discarded.budget().bytes(), 2);
        assert_eq!(session.budget().bytes(), 1);
        drop(discarded);
        assert_eq!(session.budget().bytes(), 1);

        let mut committed = session.draft().expect("committed draft");
        let loaded = committed
            .reread_utf8(source_id(), &path)
            .expect("replacement reread");
        let binding = loaded.binding().clone();
        committed
            .prepare_commit(&mut session)
            .expect("prepare commit draft")
            .commit();
        assert_eq!(session.budget().bytes(), 2);

        let mut released = session.draft().expect("release draft");
        assert_eq!(
            released.release_binding(&binding).expect("release binding"),
            FilesystemReleaseOutcome::Released
        );
        assert_eq!(session.budget().bytes(), 2);
        released
            .prepare_commit(&mut session)
            .expect("prepare commit release")
            .commit();
        assert_eq!(session.budget(), ResourceBudget::default());
    }

    #[test]
    fn draft_operations_reuse_one_candidate_state_clone() {
        let root = TestDir::new("draft-state-clone-count");
        let path = root.path().join("source.adoc");
        fs::write(&path, "text").expect("source");
        let session = policy(root.path(), 100).session().expect("session");
        let clone_count = Arc::clone(&session.state.clone_count);
        let mut draft = session.draft().expect("draft");
        assert_eq!(clone_count.load(Ordering::Relaxed), 1);

        let first = draft.read_utf8(source_id(), &path).expect("read");
        draft.reread_utf8(source_id(), &path).expect("reread");
        draft
            .discover_adoc_paths_with_control(|_, _| false, || false)
            .expect("discover");
        draft.scan_utf8(|_| Ok(source_id())).expect("scan");
        assert_eq!(
            draft
                .release_binding(first.binding())
                .expect("stale release"),
            FilesystemReleaseOutcome::Stale
        );

        assert_eq!(clone_count.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn draft_clone_unwind_releases_the_lease() {
        let root = TestDir::new("draft-clone-unwind");
        let session = policy(root.path(), 100).session().expect("session");
        FORCE_DRAFT_STATE_CLONE_PANIC.with(|forced| forced.set(true));

        let unwind = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _ = session.draft();
        }));
        FORCE_DRAFT_STATE_CLONE_PANIC.with(|forced| forced.set(false));

        assert!(unwind.is_err());
        drop(session.draft().expect("unwind released draft lease"));
    }

    #[test]
    fn draft_resource_error_preserves_its_typed_source() {
        let root = TestDir::new("draft-resource-source");
        let missing = root.path().join("missing.adoc");
        let session = policy(root.path(), 100).session().expect("session");
        let mut draft = session.draft().expect("draft");

        let error = draft
            .read_utf8(source_id(), &missing)
            .expect_err("missing resource");

        assert_eq!(
            error,
            FilesystemDraftError::Resource(ResourceError::Missing(missing.clone()))
        );
        assert_eq!(
            Error::source(&error).and_then(|source| source.downcast_ref::<ResourceError>()),
            Some(&ResourceError::Missing(missing))
        );
        assert_eq!(
            FilesystemDraftError::DraftBusy.to_string(),
            "filesystem session already has an active draft"
        );
        assert!(Error::source(&FilesystemDraftError::DraftBusy).is_none());
    }

    #[test]
    fn draft_release_rejects_a_foreign_binding_with_a_typed_error() {
        let root = TestDir::new("foreign-binding");
        let path = root.path().join("source.adoc");
        fs::write(&path, "text").expect("source");
        let mut first = policy(root.path(), 100).session().expect("first session");
        let loaded = first.read_utf8(source_id(), &path).expect("first read");
        let second = policy(root.path(), 100).session().expect("second session");
        let mut draft = second.draft().expect("second draft");

        assert_eq!(
            draft.release_binding(loaded.binding()),
            Err(FilesystemDraftError::ForeignBinding)
        );
    }

    #[test]
    fn filesystem_draft_is_exclusive_and_failed_operations_poison_commit() {
        let root = TestDir::new("filesystem-draft-exclusive");
        let missing = root.path().join("missing.adoc");
        let mut session = policy(root.path(), 100).session().expect("session");
        let mut draft = session.draft().expect("first draft");
        assert!(matches!(
            session.draft(),
            Err(FilesystemDraftError::DraftBusy)
        ));
        assert!(draft.read_utf8(source_id(), &missing).is_err());
        assert!(matches!(
            draft.prepare_commit(&mut session),
            Err(FilesystemDraftError::PoisonedDraft)
        ));
        drop(session.draft().expect("poisoned draft released its lease"));
    }

    #[test]
    fn legacy_live_mutations_invalidate_an_active_draft() {
        let root = TestDir::new("legacy-mutation-invalidates-draft");
        let path = root.path().join("source.adoc");
        fs::write(&path, "a").expect("source");

        let mut released = policy(root.path(), 100).session().expect("session");
        released
            .read_utf8(source_id(), &path)
            .expect("initial read");
        let draft = released.draft().expect("draft before release");
        released.release(&path);
        assert!(matches!(
            released.draft(),
            Err(FilesystemDraftError::DraftBusy)
        ));
        assert!(matches!(
            draft.prepare_commit(&mut released),
            Err(FilesystemDraftError::InvalidDraft)
        ));
        drop(released.draft().expect("invalid draft released its lease"));
        assert_eq!(released.budget(), ResourceBudget::default());

        let mut reread = policy(root.path(), 100).session().expect("session");
        reread.read_utf8(source_id(), &path).expect("initial read");
        let draft = reread.draft().expect("draft before reread");
        fs::write(&path, "bb").expect("replacement");
        reread
            .reread_utf8_with_rollback(source_id(), &path)
            .expect("legacy reread");
        assert!(matches!(
            draft.prepare_commit(&mut reread),
            Err(FilesystemDraftError::InvalidDraft)
        ));
        assert_eq!(reread.budget().bytes(), 2);

        let mut rolled_back = policy(root.path(), 100).session().expect("session");
        rolled_back
            .read_utf8(source_id(), &path)
            .expect("initial read");
        fs::write(&path, "ccc").expect("replacement");
        let (_, rollback) = rolled_back
            .reread_utf8_with_rollback(source_id(), &path)
            .expect("legacy reread");
        let draft = rolled_back.draft().expect("draft before rollback");
        rolled_back
            .rollback_reread(rollback)
            .expect("legacy rollback");
        assert!(matches!(
            draft.prepare_commit(&mut rolled_back),
            Err(FilesystemDraftError::InvalidDraft)
        ));
        assert_eq!(rolled_back.budget().bytes(), 2);
    }

    #[test]
    fn draft_rejects_a_foreign_session_and_exhausted_revision() {
        let first_root = TestDir::new("draft-first-session");
        let second_root = TestDir::new("draft-second-session");
        let mut first = policy(first_root.path(), 100)
            .session()
            .expect("first session");
        let mut second = policy(second_root.path(), 100)
            .session()
            .expect("second session");

        let draft = first.draft().expect("draft");
        assert!(matches!(
            draft.prepare_commit(&mut second),
            Err(FilesystemDraftError::InvalidDraft)
        ));
        drop(first.draft().expect("foreign prepare released the lease"));

        first.revision = u64::MAX;
        assert!(matches!(
            first.draft(),
            Err(FilesystemDraftError::SessionRevisionExhausted)
        ));
        assert_eq!(first.active_draft.load(Ordering::Acquire), 0);
    }

    #[test]
    fn dropping_a_prepared_commit_preserves_live_state_and_releases_lease() {
        let root = TestDir::new("prepared-commit-drop");
        let path = root.path().join("source.adoc");
        fs::write(&path, "a").expect("source");
        let mut session = policy(root.path(), 100).session().expect("session");
        session.read_utf8(source_id(), &path).expect("initial read");
        fs::write(&path, "bb").expect("replacement");

        let mut draft = session.draft().expect("draft");
        draft.reread_utf8(source_id(), &path).expect("draft read");
        let prepared = draft.prepare_commit(&mut session).expect("prepare");
        drop(prepared);

        assert_eq!(session.budget().bytes(), 1);
        drop(session.draft().expect("prepared drop released lease"));
    }

    #[test]
    fn stale_binding_from_an_older_committed_generation_cannot_release_replacement() {
        let root = TestDir::new("filesystem-draft-stale-binding");
        let path = root.path().join("source.adoc");
        fs::write(&path, "a").expect("source");
        let mut session = policy(root.path(), 100).session().expect("session");
        let mut first = session.draft().expect("first draft");
        let first_binding = first
            .read_utf8(source_id(), &path)
            .expect("first read")
            .binding()
            .clone();
        first
            .prepare_commit(&mut session)
            .expect("prepare first commit")
            .commit();

        fs::write(&path, "bb").expect("replacement");
        let mut second = session.draft().expect("second draft");
        let second_binding = second
            .reread_utf8(source_id(), &path)
            .expect("second read")
            .binding()
            .clone();
        second
            .prepare_commit(&mut session)
            .expect("prepare second commit")
            .commit();

        let mut release = session.draft().expect("release draft");
        assert_eq!(
            release
                .release_binding(&first_binding)
                .expect("stale release"),
            FilesystemReleaseOutcome::Stale
        );
        assert_eq!(
            release
                .release_binding(&second_binding)
                .expect("current release"),
            FilesystemReleaseOutcome::Released
        );
        release
            .prepare_commit(&mut session)
            .expect("prepare release commit")
            .commit();
        assert_eq!(session.budget(), ResourceBudget::default());
    }

    #[test]
    fn binding_from_a_dropped_draft_cannot_release_a_later_commit() {
        let root = TestDir::new("dropped-draft-binding");
        let path = root.path().join("source.adoc");
        fs::write(&path, "a").expect("source");
        let mut session = policy(root.path(), 100).session().expect("session");

        let mut discarded = session.draft().expect("discarded draft");
        let stale = discarded
            .read_utf8(source_id(), &path)
            .expect("discarded read")
            .binding()
            .clone();
        drop(discarded);

        let mut committed = session.draft().expect("committed draft");
        committed
            .read_utf8(source_id(), &path)
            .expect("committed read");
        committed
            .prepare_commit(&mut session)
            .expect("prepare commit")
            .commit();

        let mut release = session.draft().expect("release draft");
        assert_eq!(
            release.release_binding(&stale).expect("stale release"),
            FilesystemReleaseOutcome::Stale
        );
    }

    #[test]
    fn exhausted_binding_generation_rejects_read_before_io() {
        let root = TestDir::new("binding-generation-exhausted");
        let path = root.path().join("source.adoc");
        fs::write(&path, "text").expect("source");
        let session = policy(root.path(), 100).session().expect("session");
        session
            .next_binding_generation
            .store(u64::MAX, Ordering::Relaxed);
        let mut draft = session.draft().expect("draft");
        let before_reads = draft.candidate().sessions[0].read_files();
        let before_inspections = draft.candidate().sessions[0].inspected_paths();

        assert_eq!(
            draft.read_utf8(source_id(), &path),
            Err(FilesystemDraftError::BindingGenerationExhausted)
        );
        assert_eq!(draft.candidate().sessions[0].read_files(), before_reads);
        assert_eq!(
            draft.candidate().sessions[0].inspected_paths(),
            before_inspections
        );
    }

    #[test]
    fn a_failed_read_keeps_the_bytes_it_already_obtained() {
        let root = TestDir::new("meter-invalid-utf8");
        let path = root.path().join("source.adoc");
        fs::write(&path, [0xff, 0xfe, 0xfd]).expect("invalid UTF-8 source");
        let session = policy(root.path(), 100).session().expect("session");
        let mut draft = session.draft().expect("draft");
        let meter = draft_meter(&draft);

        let result = draft.read_utf8(source_id(), &path);

        assert_eq!(
            result,
            Err(FilesystemDraftError::Resource(ResourceError::InvalidUtf8 {
                path,
                source: "input is not valid UTF-8".to_owned(),
            }))
        );
        assert_eq!(
            meter.usage(),
            FilesystemIoUsage {
                read_operations: 1,
                read_bytes: 3,
                directory_read_operations: 0,
                directory_entries: 0,
            }
        );
    }

    #[test]
    fn a_missing_resource_counts_an_attempt_and_stops_the_poisoned_draft() {
        let root = TestDir::new("meter-missing-then-poisoned");
        let missing = root.path().join("missing.adoc");
        let existing = root.path().join("existing.adoc");
        fs::write(&existing, "text").expect("existing source");
        let session = policy(root.path(), 100).session().expect("session");
        let mut draft = session.draft().expect("draft");
        let meter = draft_meter(&draft);

        assert_eq!(
            draft.read_utf8(source_id(), &missing),
            Err(FilesystemDraftError::Resource(ResourceError::Missing(
                missing
            )))
        );
        let after_failure = meter.usage();
        assert_eq!(after_failure.read_operations, 1);
        assert_eq!(after_failure.read_bytes, 0);

        assert_eq!(
            draft.read_utf8(source_id(), &existing),
            Err(FilesystemDraftError::PoisonedDraft)
        );
        assert_eq!(meter.usage(), after_failure);
    }

    #[test]
    fn a_poisoned_draft_starts_no_filesystem_work_at_all() {
        let root = TestDir::new("meter-poisoned-draft-entry-points");
        let missing = root.path().join("missing.adoc");
        let existing = root.path().join("existing.adoc");
        fs::write(&existing, "text").expect("existing source");
        let session = policy(root.path(), 100).session().expect("session");
        let mut draft = session.draft().expect("draft");
        let meter = draft_meter(&draft);
        assert!(draft.read_utf8(source_id(), &missing).is_err());
        let after_failure = meter.usage();

        assert_eq!(
            draft.scan_utf8(path_source_id),
            Err(FilesystemDraftError::PoisonedDraft)
        );
        assert_eq!(
            draft.reread_utf8(source_id(), &existing),
            Err(FilesystemDraftError::PoisonedDraft)
        );
        assert_eq!(
            draft.read_target_utf8(source_id(), root.path(), "existing.adoc"),
            Err(FilesystemDraftError::PoisonedDraft)
        );
        assert_eq!(
            draft
                .discover_adoc_paths_with_control(|_, _| false, || false)
                .err(),
            Some(FilesystemDraftError::PoisonedDraft)
        );
        assert_eq!(meter.usage(), after_failure);
    }

    #[test]
    fn a_capacity_rejection_counts_an_attempt_without_bytes() {
        let root = TestDir::new("meter-capacity-rejection");
        let path = root.path().join("source.adoc");
        fs::write(&path, "text").expect("source");
        let session = LocalFilesystemPolicy::new(
            [root.path().to_owned()],
            FilesystemReadLimits {
                max_files: 0,
                max_total_bytes: 100,
                max_resource_bytes: 100,
            },
        )
        .expect("policy")
        .session()
        .expect("session");
        let mut draft = session.draft().expect("draft");
        let meter = draft_meter(&draft);

        assert_eq!(
            draft.read_utf8(source_id(), &path),
            Err(FilesystemDraftError::Resource(ResourceError::FileLimit {
                limit: 0
            }))
        );
        assert_eq!(meter.usage().read_operations, 1);
        assert_eq!(meter.usage().read_bytes, 0);
    }

    #[test]
    fn a_cached_read_counts_the_request_without_reading_bytes() {
        let root = TestDir::new("meter-cache-hit");
        let path = root.path().join("source.adoc");
        fs::write(&path, "text").expect("source");
        let session = policy(root.path(), 100).session().expect("session");
        let mut draft = session.draft().expect("draft");
        let meter = draft_meter(&draft);
        draft
            .mutation_cursor()
            .read_utf8_with(source_id(), &path, true, || {})
            .expect("cache miss");
        let after_miss = meter.usage();

        draft
            .mutation_cursor()
            .read_utf8_with(source_id(), &path, true, || {})
            .expect("cache hit");

        assert_eq!(after_miss.read_operations, 1);
        assert_eq!(after_miss.read_bytes, 4);
        let hit = meter.usage().since(after_miss);
        assert_eq!(hit.read_operations, 1);
        assert_eq!(hit.read_bytes, 0);
    }

    #[test]
    fn a_discarded_draft_leaves_its_work_counted_in_the_session() {
        let root = TestDir::new("meter-shared-with-session");
        let path = root.path().join("source.adoc");
        fs::write(&path, "abc").expect("source");
        let session = policy(root.path(), 100).session().expect("session");
        let session_meter = session.state.meter.clone();
        let mut draft = session.draft().expect("draft");

        draft.read_utf8(source_id(), &path).expect("read");
        drop(draft);

        assert_eq!(session_meter.usage().read_operations, 1);
        assert_eq!(session_meter.usage().read_bytes, 3);
    }

    #[test]
    fn legacy_read_maps_binding_exhaustion_without_starting_io() {
        let root = TestDir::new("legacy-binding-generation-exhausted");
        let path = root.path().join("source.adoc");
        fs::write(&path, "text").expect("source");
        let mut session = policy(root.path(), 100).session().expect("session");
        session
            .next_binding_generation
            .store(u64::MAX, Ordering::Relaxed);
        let before_reads = session.state.sessions[0].read_files();
        let before_inspections = session.state.sessions[0].inspected_paths();

        assert_eq!(
            session.read_utf8(source_id(), &path),
            Err(ResourceError::Unverifiable(
                "filesystem binding generation space is exhausted".to_owned()
            ))
        );
        assert_eq!(session.state.sessions[0].read_files(), before_reads);
        assert_eq!(
            session.state.sessions[0].inspected_paths(),
            before_inspections
        );
    }

    #[test]
    fn rollback_restores_the_original_binding_generation() {
        let root = TestDir::new("rollback-binding-generation");
        let path = root.path().join("source.adoc");
        fs::write(&path, "a").expect("source");
        let mut session = policy(root.path(), 100).session().expect("session");
        let original = session.read_utf8(source_id(), &path).expect("initial read");
        fs::write(&path, "bb").expect("replacement");
        let (_, rollback) = session
            .reread_utf8_with_rollback(source_id(), &path)
            .expect("reread");
        session.rollback_reread(rollback).expect("rollback");
        let mut release = session.draft().expect("release draft");

        assert_eq!(
            release
                .release_binding(original.binding())
                .expect("original binding remains current"),
            FilesystemReleaseOutcome::Released
        );
        release
            .prepare_commit(&mut session)
            .expect("prepare release")
            .commit();
        assert_eq!(session.budget(), ResourceBudget::default());
    }

    #[test]
    fn loaded_source_value_equality_ignores_lifecycle_bindings() {
        let root = TestDir::new("loaded-source-value-equality");
        let path = root.path().join("source.adoc");
        fs::write(&path, "text").expect("source");
        let mut first = policy(root.path(), 100).session().expect("first session");
        let mut second = policy(root.path(), 100).session().expect("second session");

        let first_loaded = first.read_utf8(source_id(), &path).expect("first read");
        let second_loaded = second.read_utf8(source_id(), &path).expect("second read");

        assert_ne!(first_loaded.binding(), second_loaded.binding());
        assert_eq!(first_loaded, second_loaded);
        let expected_binding = first_loaded.binding().clone();
        let (logical_id, source, binding) = first_loaded.into_parts_with_binding();
        assert_eq!(logical_id, source_id());
        assert_eq!(source.as_ref(), "text");
        assert_eq!(binding, expected_binding);
    }

    fn path_source_id(path: &Path) -> Result<LogicalSourceId, ResourceError> {
        LogicalSourceId::new(format!(
            "logical:{}",
            path.file_name()
                .and_then(|name| name.to_str())
                .ok_or_else(|| ResourceError::Unverifiable(
                    "test path has no UTF-8 file name".to_owned()
                ))?
        ))
    }

    #[test]
    fn scan_is_deterministic_and_keeps_paths_out_of_logical_ids() {
        let root = TestDir::new("scan");
        let nested = root.path().join("nested");
        fs::create_dir(&nested).expect("nested directory");
        fs::write(root.path().join("b.adoc"), "second\n").expect("second source");
        fs::write(nested.join("a.adoc"), "first\n").expect("first source");
        fs::write(root.path().join("ignored.txt"), "ignored\n").expect("ignored source");
        let policy = LocalFilesystemPolicy::new(
            [root.path().to_owned(), root.path().to_owned()],
            FilesystemReadLimits::default(),
        )
        .expect("policy");
        let mut session = policy.session().expect("session");

        let loaded = session.scan_utf8(path_source_id).expect("scan");

        assert_eq!(loaded.len(), 2);
        assert_eq!(
            loaded
                .iter()
                .map(|source| source.source_id().as_str())
                .collect::<Vec<_>>(),
            ["logical:b.adoc", "logical:a.adoc"]
        );
        assert_eq!(
            loaded
                .iter()
                .map(LoadedFilesystemSource::source)
                .collect::<Vec<_>>(),
            ["second\n", "first\n"]
        );
        assert!(loaded.iter().all(|source| {
            !source
                .source_id()
                .as_str()
                .contains(root.path().to_string_lossy().as_ref())
        }));
        assert_eq!(
            (session.budget().files(), session.budget().bytes()),
            (2, 13)
        );
    }

    #[test]
    fn adding_roots_over_the_policy_limit_is_transactional() {
        let parent = TestDir::new("root-policy-limit");
        let initial = parent.path().join("root-000");
        fs::create_dir(&initial).expect("initial root");
        let mut policy = LocalFilesystemPolicy::new([initial], FilesystemReadLimits::default())
            .expect("initial policy");
        let anchor = policy.roots()[0].clone();
        let mut additions = Vec::new();
        for index in 1..MAX_FILESYSTEM_POLICY_ROOTS {
            let root = parent.path().join(format!("root-{index:03}"));
            fs::create_dir(&root).expect("additional root");
            additions.push(root);
        }
        policy
            .access_derived(
                &anchor,
                DerivedFilesystemRoots {
                    confined: Vec::new(),
                    independent: additions,
                },
                FilesystemReadLimits::default(),
            )
            .expect("fill policy root limit");
        let before = policy.roots().to_vec();
        let rejected = parent.path().join("root-over-limit");
        fs::create_dir(&rejected).expect("rejected root");

        assert_eq!(
            policy
                .access_derived(
                    &anchor,
                    DerivedFilesystemRoots {
                        confined: Vec::new(),
                        independent: vec![rejected.clone()],
                    },
                    FilesystemReadLimits::default(),
                )
                .expect_err("root limit"),
            ResourceError::RootLimit {
                limit: MAX_FILESYSTEM_POLICY_ROOTS,
            }
        );
        assert_eq!(policy.roots(), before);
        assert!(before.iter().all(|root| policy.root_policy(root).is_some()));
        let duplicate = policy
            .access_derived(
                &anchor,
                DerivedFilesystemRoots {
                    confined: Vec::new(),
                    independent: vec![before[0].clone()],
                },
                FilesystemReadLimits::default(),
            )
            .expect("duplicate root at the limit");
        assert_eq!(duplicate.roots(), [before[0].clone()]);
        assert_eq!(policy.roots(), before);
        drop(policy);

        let mut staged = LocalFilesystemPolicy::new(
            before[..MAX_FILESYSTEM_POLICY_ROOTS - 1].iter().cloned(),
            FilesystemReadLimits::default(),
        )
        .expect("policy below the limit");
        let staged_before = staged.roots().to_vec();
        let staged_anchor = staged_before[0].clone();
        assert_eq!(
            staged
                .access_derived(
                    &staged_anchor,
                    DerivedFilesystemRoots {
                        confined: Vec::new(),
                        independent: vec![
                            before[MAX_FILESYSTEM_POLICY_ROOTS - 1].clone(),
                            rejected.clone(),
                        ],
                    },
                    FilesystemReadLimits::default(),
                )
                .expect_err("staged roots exceed the limit"),
            ResourceError::RootLimit {
                limit: MAX_FILESYSTEM_POLICY_ROOTS,
            }
        );
        assert_eq!(staged.roots(), staged_before);
        assert!(
            staged
                .root_policy(&before[MAX_FILESYSTEM_POLICY_ROOTS - 1])
                .is_none()
        );
        drop(staged);

        assert_eq!(
            LocalFilesystemPolicy::new(
                before.into_iter().chain([rejected]),
                FilesystemReadLimits::default(),
            )
            .expect_err("constructor root limit"),
            ResourceError::RootLimit {
                limit: MAX_FILESYSTEM_POLICY_ROOTS,
            }
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn policy_session_keeps_the_root_opened_at_policy_construction() {
        use std::os::unix::fs::symlink;

        let root = TestDir::new("policy-root-swap");
        let outside = TestDir::new("policy-root-swap-outside");
        let candidate = root.path().join("root.adoc");
        fs::write(&candidate, "inside").expect("inside source");
        fs::write(outside.path().join("root.adoc"), "outside").expect("outside source");
        let policy = policy(root.path(), 100);
        let displaced = root.path().with_extension("anchored");
        fs::rename(root.path(), &displaced).expect("displace trusted root");
        symlink(outside.path(), root.path()).expect("replace root path");

        let loaded = policy
            .session()
            .expect("session")
            .read_utf8(source_id(), &candidate)
            .expect("read from retained policy root");

        assert_eq!(loaded.source(), "inside");
        assert_ne!(loaded.source(), "outside");
        fs::remove_file(root.path()).expect("remove replacement symlink");
        fs::rename(displaced, root.path()).expect("restore trusted root");
    }

    #[test]
    fn derived_session_cannot_expand_policy_limits() {
        let root = TestDir::new("derived-session-limits");
        let policy = policy(root.path(), 10);
        let root_path = policy.roots()[0].clone();

        for limits in [
            FilesystemReadLimits {
                max_files: 11,
                max_total_bytes: 100,
                max_resource_bytes: 10,
            },
            FilesystemReadLimits {
                max_files: 10,
                max_total_bytes: 101,
                max_resource_bytes: 10,
            },
            FilesystemReadLimits {
                max_files: 10,
                max_total_bytes: 100,
                max_resource_bytes: 11,
            },
        ] {
            assert!(matches!(
                policy.access_existing([root_path.clone()], limits),
                Err(ResourceError::Unverifiable(reason))
                    if reason == "filesystem access limits exceed the authority limits"
            ));
        }
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn confined_root_derivation_keeps_the_anchor_namespace() {
        let directory = TestDir::new("derived-root-authority");
        let root = directory.path().join("workspace");
        let nested = root.join("docs");
        fs::create_dir_all(&nested).expect("trusted nested root");
        fs::write(nested.join("document.adoc"), "trusted").expect("trusted document");
        let mut policy =
            LocalFilesystemPolicy::new([root.clone()], FilesystemReadLimits::default())
                .expect("filesystem policy");
        let anchor = policy.roots()[0].clone();

        let moved = directory.path().join("moved-workspace");
        fs::rename(&root, &moved).expect("move trusted workspace");
        fs::create_dir_all(root.join("docs")).expect("replacement nested root");
        fs::write(root.join("docs/document.adoc"), "replacement").expect("replacement document");

        let access = policy
            .access_derived(
                &anchor,
                DerivedFilesystemRoots {
                    confined: vec![nested.clone()],
                    independent: Vec::new(),
                },
                FilesystemReadLimits::default(),
            )
            .expect("derive nested authority");
        let mut session = access.session().expect("derived session");
        let loaded = session
            .read_utf8(
                LogicalSourceId::new("document").expect("source id"),
                &nested.join("document.adoc"),
            )
            .expect("read through retained namespace");

        assert_eq!(loaded.source(), "trusted");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn scan_enumerates_the_retained_root_after_namespace_replacement() {
        use std::os::unix::fs::symlink;

        let root = TestDir::new("scan-root-swap");
        let outside = TestDir::new("scan-root-swap-outside");
        fs::write(root.path().join("inside.adoc"), "inside").expect("inside source");
        fs::write(outside.path().join("outside.adoc"), "outside").expect("outside source");
        let policy = policy(root.path(), 100);
        let displaced = root.path().with_extension("anchored");
        fs::rename(root.path(), &displaced).expect("displace trusted root");
        symlink(outside.path(), root.path()).expect("replace root path");

        let loaded = policy
            .session()
            .expect("session")
            .scan_utf8(path_source_id)
            .expect("scan retained root");

        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].source(), "inside");
        assert_eq!(loaded[0].source_id().as_str(), "logical:inside.adoc");
        fs::remove_file(root.path()).expect("remove replacement symlink");
        fs::rename(displaced, root.path()).expect("restore trusted root");
    }

    #[cfg(unix)]
    #[test]
    fn scan_does_not_follow_symlinked_files_or_directories() {
        use std::os::unix::fs::symlink;

        let root = TestDir::new("scan-symlink-root");
        let outside = TestDir::new("scan-symlink-outside");
        let outside_file = outside.path().join("outside.adoc");
        fs::write(&outside_file, "outside\n").expect("outside source");
        symlink(&outside_file, root.path().join("file.adoc")).expect("file symlink");
        symlink(outside.path(), root.path().join("directory")).expect("directory symlink");
        fs::write(root.path().join("inside.adoc"), "inside\n").expect("inside source");
        let mut session = policy(root.path(), 100).session().expect("session");

        let loaded = session.scan_utf8(path_source_id).expect("scan");

        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].source_id().as_str(), "logical:inside.adoc");
        assert_eq!(loaded[0].source(), "inside\n");
    }

    #[test]
    fn scan_applies_candidate_and_shared_byte_budgets_in_the_host_session() {
        let root = TestDir::new("scan-budget");
        fs::write(root.path().join("a.adoc"), "1234").expect("first source");
        fs::write(root.path().join("b.adoc"), "5678").expect("second source");
        let policy = LocalFilesystemPolicy::new(
            [root.path().to_owned()],
            FilesystemReadLimits {
                max_files: 1,
                max_total_bytes: 8,
                max_resource_bytes: 4,
            },
        )
        .expect("policy");
        assert_eq!(
            policy.session().expect("session").scan_utf8(path_source_id),
            Err(ResourceError::FileLimit { limit: 1 })
        );

        let policy = LocalFilesystemPolicy::new(
            [root.path().to_owned()],
            FilesystemReadLimits {
                max_files: 2,
                max_total_bytes: 7,
                max_resource_bytes: 4,
            },
        )
        .expect("policy");
        assert_eq!(
            policy.session().expect("session").scan_utf8(path_source_id),
            Err(ResourceError::ByteLimit)
        );
    }

    #[test]
    fn directory_pruning_happens_before_the_scan_entry_limit() {
        let root = TestDir::new("scan-pruning-limit");
        let excluded = root.path().join("excluded");
        fs::create_dir(&excluded).expect("excluded directory");
        for name in ["one", "two", "three"] {
            fs::write(excluded.join(name), "ignored").expect("excluded entry");
        }
        fs::write(root.path().join("kept.adoc"), "kept\n").expect("kept source");
        let session = policy(root.path(), 100).session().expect("session");

        assert_eq!(
            session.discover_adoc_paths_with_limit(2, |_, _| false, || false),
            Err(ResourceError::ScanEntryLimit { limit: 2 })
        );
        assert_eq!(
            session
                .discover_adoc_paths_with_limit(
                    2,
                    |scan_root, relative| {
                        assert_eq!(scan_root, root.path());
                        relative == Path::new("excluded")
                    },
                    || false,
                )
                .expect("pruned discovery"),
            [root.path().join("kept.adoc")]
        );
    }

    #[test]
    fn pruned_directory_itself_still_counts_toward_the_scan_limit() {
        let root = TestDir::new("scan-pruned-directory-boundary");
        fs::create_dir(root.path().join("excluded")).expect("excluded directory");
        let session = policy(root.path(), 100).session().expect("session");

        assert_eq!(
            session.discover_adoc_paths_with_limit(0, |_, _| true, || false),
            Err(ResourceError::ScanEntryLimit { limit: 0 })
        );
        assert!(
            session
                .discover_adoc_paths_with_limit(
                    1,
                    |_, relative| relative == Path::new("excluded"),
                    || false,
                )
                .expect("boundary discovery")
                .is_empty()
        );
    }

    #[test]
    fn cancelled_discovery_never_returns_a_partial_candidate_set() {
        let root = TestDir::new("scan-cancelled");
        fs::write(root.path().join("first.adoc"), "first\n").expect("first source");
        fs::write(root.path().join("second.adoc"), "second\n").expect("second source");
        let session = policy(root.path(), 100).session().expect("session");
        let checks = std::cell::Cell::new(0_usize);

        let result = session.discover_adoc_paths_with_control(
            |_, _| false,
            || {
                checks.set(checks.get() + 1);
                checks.get() > 2
            },
        );

        assert_eq!(
            result,
            Err(ResourceError::Unverifiable(
                "local filesystem scan was cancelled".to_owned()
            ))
        );
    }

    #[test]
    fn a_cancelled_discovery_keeps_the_directory_work_it_performed() {
        let root = TestDir::new("meter-scan-cancellation");
        fs::write(root.path().join("a.adoc"), "a").expect("first source");
        fs::write(root.path().join("b.adoc"), "b").expect("second source");
        let session = policy(root.path(), 100).session().expect("session");
        let meter = session.state.meter.clone();
        let checks = std::cell::Cell::new(0_usize);

        let result = LocalFilesystemView {
            state: &session.state,
        }
        .discover_adoc_paths_with_control(
            LocalFilesystemSession::MAX_SCAN_ENTRIES,
            |_, _| false,
            || {
                checks.set(checks.get() + 1);
                checks.get() > 1
            },
        );

        assert_eq!(
            result,
            Err(ResourceError::Unverifiable(
                "local filesystem scan was cancelled".to_owned()
            ))
        );
        assert_eq!(
            meter.usage(),
            FilesystemIoUsage {
                read_operations: 0,
                read_bytes: 0,
                directory_read_operations: 1,
                directory_entries: 1,
            }
        );
    }

    /// Entries are counted as the iterator yields them, before anything decides
    /// what they are. On Linux that includes `.` and `..`, which the scan then
    /// skips, so the entry count is higher there than the four visible names.
    #[test]
    fn a_scan_counts_directory_entries_and_the_files_it_reads() {
        let root = TestDir::new("meter-scan-usage");
        let nested = root.path().join("nested");
        fs::create_dir(&nested).expect("nested directory");
        fs::write(root.path().join("a.adoc"), "abc").expect("first source");
        fs::write(root.path().join("ignored.txt"), "ignored").expect("ignored source");
        fs::write(nested.join("b.adoc"), "de").expect("second source");
        let session = policy(root.path(), 100).session().expect("session");
        let mut draft = session.draft().expect("draft");
        let meter = draft_meter(&draft);

        let loaded = draft.scan_utf8(path_source_id).expect("scan");

        assert_eq!(loaded.len(), 2);
        let usage = meter.usage();
        assert_eq!(usage.directory_read_operations, 2);
        #[cfg(target_os = "linux")]
        assert_eq!(usage.directory_entries, 8);
        #[cfg(not(target_os = "linux"))]
        assert_eq!(usage.directory_entries, 4);
        assert_eq!(usage.read_operations, 2);
        assert_eq!(usage.read_bytes, 5);
    }

    #[test]
    fn source_ids_and_platform_capability_are_explicit() {
        assert!(matches!(
            LogicalSourceId::new(""),
            Err(ResourceError::InvalidSourceId)
        ));
        assert!(matches!(
            LogicalSourceId::new("bad\nid"),
            Err(ResourceError::InvalidSourceId)
        ));
        let root = TestDir::new("capability");
        let session = policy(root.path(), 100).session().expect("session");
        #[cfg(target_os = "linux")]
        assert_eq!(
            session.race_resistance(),
            FilesystemRaceResistance::HandleRelative
        );
        #[cfg(not(target_os = "linux"))]
        assert_eq!(
            session.race_resistance(),
            FilesystemRaceResistance::StaticSnapshotOnly
        );
        let mut session = policy(root.path(), 100).session().expect("session");
        assert!(matches!(
            session.read_utf8(source_id(), Path::new("relative.adoc")),
            Err(ResourceError::PathNotAbsolute(_))
        ));
    }

    #[test]
    fn failed_global_budget_charge_is_not_bypassed_by_retrying_the_same_path() {
        let first_root = TestDir::new("file-budget-first");
        let second_root = TestDir::new("file-budget-second");
        let first = first_root.path().join("first.adoc");
        let second = second_root.path().join("second.adoc");
        fs::write(&first, "a").expect("first source");
        fs::write(&second, "b").expect("second source");
        let policy = LocalFilesystemPolicy::new(
            [first_root.path().to_owned(), second_root.path().to_owned()],
            FilesystemReadLimits {
                max_files: 1,
                max_total_bytes: 2,
                max_resource_bytes: 2,
            },
        )
        .expect("policy");
        let mut session = policy.session().expect("session");
        session.read_utf8(source_id(), &first).expect("first read");
        for _ in 0..2 {
            assert_eq!(
                session.read_utf8(source_id(), &second),
                Err(ResourceError::FileLimit { limit: 1 })
            );
        }

        let first_root = TestDir::new("byte-budget-first");
        let second_root = TestDir::new("byte-budget-second");
        let first = first_root.path().join("first.adoc");
        let second = second_root.path().join("second.adoc");
        fs::write(&first, "ab").expect("first source");
        fs::write(&second, "cd").expect("second source");
        let policy = LocalFilesystemPolicy::new(
            [first_root.path().to_owned(), second_root.path().to_owned()],
            FilesystemReadLimits {
                max_files: 2,
                max_total_bytes: 3,
                max_resource_bytes: 2,
            },
        )
        .expect("policy");
        let mut session = policy.session().expect("session");
        session.read_utf8(source_id(), &first).expect("first read");
        for _ in 0..2 {
            assert_eq!(
                session.read_utf8(source_id(), &second),
                Err(ResourceError::ByteLimit)
            );
        }
    }

    #[test]
    fn reread_replaces_and_release_removes_charges_transactionally() {
        let root = TestDir::new("replacement-budget");
        let first = root.path().join("first.adoc");
        let second = root.path().join("second.adoc");
        fs::write(&first, "1234").expect("first source");
        fs::write(&second, "1234").expect("second source");
        let policy = LocalFilesystemPolicy::new(
            [root.path().to_owned()],
            FilesystemReadLimits {
                max_files: 2,
                max_total_bytes: 6,
                max_resource_bytes: 6,
            },
        )
        .expect("policy");
        let mut session = policy.session().expect("session");

        session
            .read_utf8(source_id(), &first)
            .expect("initial read");
        assert_eq!((session.budget().files(), session.budget().bytes()), (1, 4));

        fs::write(&first, "12").expect("shrink first");
        session
            .reread_utf8(source_id(), &first)
            .expect("shrunk reread");
        assert_eq!((session.budget().files(), session.budget().bytes()), (1, 2));

        session
            .read_utf8(source_id(), &second)
            .expect("second read");
        assert_eq!((session.budget().files(), session.budget().bytes()), (2, 6));

        fs::write(&first, "123").expect("grow first");
        assert_eq!(
            session.reread_utf8(source_id(), &first),
            Err(ResourceError::ByteLimit)
        );
        assert_eq!((session.budget().files(), session.budget().bytes()), (2, 6));

        fs::write(&second, "1").expect("shrink second");
        session
            .reread_utf8(source_id(), &second)
            .expect("shrunk second");
        session
            .reread_utf8(source_id(), &first)
            .expect("grown first");
        assert_eq!((session.budget().files(), session.budget().bytes()), (2, 4));

        fs::remove_file(&second).expect("delete second");
        session.release(&second);
        assert_eq!((session.budget().files(), session.budget().bytes()), (1, 3));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn release_uses_the_candidate_after_the_opened_file_is_renamed() {
        let root = TestDir::new("release-renamed-candidate");
        let candidate = root.path().join("source.adoc");
        let renamed = root.path().join("renamed.adoc");
        fs::write(&candidate, "text").expect("source");
        let mut session = policy(root.path(), 100).session().expect("session");

        let loaded = session
            .read_utf8_after_open(source_id(), &candidate, || {
                fs::rename(&candidate, &renamed).expect("rename opened source");
            })
            .expect("read renamed source");
        assert_eq!(loaded.canonical_path(), renamed);
        assert_eq!((session.budget().files(), session.budget().bytes()), (1, 4));

        session.release(&candidate);
        assert_eq!((session.budget().files(), session.budget().bytes()), (0, 0));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn release_reclaims_a_nested_parent_component_alias() {
        let root = TestDir::new("release-parent-alias");
        let nested = root.path().join("nested");
        fs::create_dir(&nested).expect("nested directory");
        let source = root.path().join("source.adoc");
        let alias = nested.join("..").join("source.adoc");
        fs::write(&source, "text").expect("source");
        let mut session = policy(root.path(), 100).session().expect("session");

        session.read_utf8(source_id(), &alias).expect("alias read");
        assert_eq!((session.budget().files(), session.budget().bytes()), (1, 4));
        session.release(&alias);
        assert_eq!((session.budget().files(), session.budget().bytes()), (0, 0));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn release_reclaims_a_symbolic_link_alias() {
        use std::os::unix::fs::symlink;

        let root = TestDir::new("release-symlink-alias");
        let source = root.path().join("source.adoc");
        let alias = root.path().join("alias.adoc");
        fs::write(&source, "text").expect("source");
        symlink("source.adoc", &alias).expect("source alias");
        let mut session = policy(root.path(), 100).session().expect("session");

        session.read_utf8(source_id(), &alias).expect("alias read");
        session.release(&alias);
        assert_eq!((session.budget().files(), session.budget().bytes()), (0, 0));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn the_last_alias_release_reclaims_the_shared_file_limit() {
        use std::os::unix::fs::symlink;

        let root = TestDir::new("release-last-alias");
        let source = root.path().join("source.adoc");
        let first_alias = source.clone();
        let second_alias = root.path().join("alias.adoc");
        let replacement = root.path().join("replacement.adoc");
        fs::write(&source, "text").expect("source");
        fs::write(&replacement, "new").expect("replacement");
        symlink("source.adoc", &second_alias).expect("second alias");
        let policy = LocalFilesystemPolicy::new(
            [root.path().to_owned()],
            FilesystemReadLimits {
                max_files: 2,
                max_total_bytes: 8,
                max_resource_bytes: 4,
            },
        )
        .expect("policy");
        let mut session = policy.session().expect("session");

        let first_loaded = session
            .read_utf8(source_id(), &first_alias)
            .expect("first alias");
        let second_loaded = session
            .read_utf8(source_id(), &second_alias)
            .expect("second alias");
        assert_eq!(
            first_loaded.canonical_path(),
            second_loaded.canonical_path()
        );
        session.release(&first_alias);
        assert_eq!((session.budget().files(), session.budget().bytes()), (1, 4));

        session.release(&second_alias);
        assert_eq!((session.budget().files(), session.budget().bytes()), (0, 0));
        session
            .read_utf8(source_id(), &replacement)
            .expect("released file slot");
        assert_eq!((session.budget().files(), session.budget().bytes()), (1, 3));
    }

    #[test]
    fn reread_rollback_restores_replaced_and_new_charges() {
        let root = TestDir::new("reread-rollback");
        let first = root.path().join("first.adoc");
        let second = root.path().join("second.adoc");
        fs::write(&first, "a").expect("first source");
        fs::write(&second, "bb").expect("second source");
        let policy = LocalFilesystemPolicy::new(
            [root.path().to_owned()],
            FilesystemReadLimits {
                max_files: 2,
                max_total_bytes: 4,
                max_resource_bytes: 4,
            },
        )
        .expect("policy");
        let mut session = policy.session().expect("session");
        session
            .read_utf8(source_id(), &first)
            .expect("initial read");

        fs::write(&first, "aaa").expect("grown first");
        let (_, rollback) = session
            .reread_utf8_with_rollback(source_id(), &first)
            .expect("replacement reread");
        assert_eq!((session.budget().files(), session.budget().bytes()), (1, 3));
        session
            .rollback_reread(rollback)
            .expect("rollback replacement");
        assert_eq!((session.budget().files(), session.budget().bytes()), (1, 1));

        let (_, rollback) = session
            .reread_utf8_with_rollback(source_id(), &second)
            .expect("new reread");
        assert_eq!((session.budget().files(), session.budget().bytes()), (2, 3));
        session
            .rollback_reread(rollback)
            .expect("rollback new read");
        assert_eq!((session.budget().files(), session.budget().bytes()), (1, 1));
    }

    #[test]
    fn reread_commit_replaces_the_command_text_snapshot() {
        let root = TestDir::new("reread-cache-commit");
        let path = root.path().join("source.adoc");
        fs::write(&path, "old").expect("initial source");
        let mut session = policy(root.path(), 100).session().expect("session");
        session.read_utf8(source_id(), &path).expect("initial read");
        session.state.sessions[0]
            .read_candidate_utf8(&path)
            .expect("initial cached read");

        fs::write(&path, "new text").expect("replacement source");
        let reread = session
            .reread_utf8(source_id(), &path)
            .expect("committed reread");
        assert_eq!(reread.source(), "new text");
        assert_eq!((session.budget().files(), session.budget().bytes()), (1, 8));

        fs::write(&path, "disk changed again").expect("later disk source");
        let cached = session.state.sessions[0]
            .read_candidate_utf8(&path)
            .expect("committed command snapshot");
        assert_eq!(cached.source(), "new text");
        assert_eq!((session.budget().files(), session.budget().bytes()), (1, 8));
    }

    #[test]
    fn reread_rollback_restores_the_previous_command_text_snapshot() {
        let root = TestDir::new("reread-cache-rollback");
        let path = root.path().join("source.adoc");
        fs::write(&path, "old").expect("initial source");
        let mut session = policy(root.path(), 100).session().expect("session");
        session.read_utf8(source_id(), &path).expect("initial read");
        session.state.sessions[0]
            .read_candidate_utf8(&path)
            .expect("initial cached read");

        fs::write(&path, "new text").expect("replacement source");
        let (prepared, rollback) = session
            .reread_utf8_with_rollback(source_id(), &path)
            .expect("prepared reread");
        assert_eq!(prepared.source(), "new text");
        assert_eq!((session.budget().files(), session.budget().bytes()), (1, 8));

        session.rollback_reread(rollback).expect("rollback reread");
        assert_eq!(
            session.state.sessions[0]
                .read_candidate_utf8(&path)
                .expect("restored snapshot")
                .source(),
            "old"
        );
        assert_eq!((session.budget().files(), session.budget().bytes()), (1, 3));
    }

    #[test]
    fn reread_rollback_rejects_another_session_and_reuse() {
        let root = TestDir::new("reread-rollback-session");
        let path = root.path().join("source.adoc");
        fs::write(&path, "a").expect("source");
        let policy = LocalFilesystemPolicy::new(
            [root.path().to_owned()],
            FilesystemReadLimits {
                max_files: 1,
                max_total_bytes: 4,
                max_resource_bytes: 4,
            },
        )
        .expect("policy");
        let mut first = policy.session().expect("first session");
        let mut second = policy.session().expect("second session");
        first
            .read_utf8(source_id(), &path)
            .expect("first initial read");
        second
            .read_utf8(source_id(), &path)
            .expect("second initial read");

        fs::write(&path, "bb").expect("replacement");
        let (_, rollback) = first
            .reread_utf8_with_rollback(source_id(), &path)
            .expect("replacement reread");
        assert_eq!(
            second.rollback_reread(rollback.clone()),
            Err(ResourceError::InvalidRollback)
        );
        assert_eq!((second.budget().files(), second.budget().bytes()), (1, 1));

        first
            .rollback_reread(rollback.clone())
            .expect("first rollback");
        assert_eq!((first.budget().files(), first.budget().bytes()), (1, 1));
        assert_eq!(
            first.rollback_reread(rollback),
            Err(ResourceError::InvalidRollback)
        );
        assert_eq!((first.budget().files(), first.budget().bytes()), (1, 1));
    }

    #[test]
    fn reread_rollback_rejects_stale_and_out_of_order_tokens() {
        let root = TestDir::new("reread-rollback-generation");
        let path = root.path().join("source.adoc");
        fs::write(&path, "a").expect("source");
        let policy = LocalFilesystemPolicy::new(
            [root.path().to_owned()],
            FilesystemReadLimits {
                max_files: 1,
                max_total_bytes: 8,
                max_resource_bytes: 8,
            },
        )
        .expect("policy");
        let mut session = policy.session().expect("session");
        session.read_utf8(source_id(), &path).expect("initial read");

        fs::write(&path, "bb").expect("first replacement");
        let (_, first_rollback) = session
            .reread_utf8_with_rollback(source_id(), &path)
            .expect("first reread");
        fs::write(&path, "ccc").expect("second replacement");
        let (_, second_rollback) = session
            .reread_utf8_with_rollback(source_id(), &path)
            .expect("second reread");

        assert_eq!(
            session.rollback_reread(first_rollback),
            Err(ResourceError::InvalidRollback)
        );
        assert_eq!((session.budget().files(), session.budget().bytes()), (1, 3));
        session
            .rollback_reread(second_rollback)
            .expect("latest rollback");
        assert_eq!((session.budget().files(), session.budget().bytes()), (1, 2));
        assert_eq!(
            session.state.sessions[0]
                .read_candidate_utf8(&path)
                .expect("latest rollback restores the preceding cache generation")
                .source(),
            "bb"
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn reread_rollback_preserves_a_preexisting_uncharged_candidate() {
        let root = TestDir::new("reread-rollback-preexisting-candidate");
        let path = root.path().join("source.adoc");
        fs::write(&path, "oversized").expect("oversized source");
        let policy = LocalFilesystemPolicy::new(
            [root.path().to_owned()],
            FilesystemReadLimits {
                max_files: 1,
                max_total_bytes: 8,
                max_resource_bytes: 4,
            },
        )
        .expect("policy");
        let mut session = policy.session().expect("session");
        assert!(matches!(
            session.read_utf8(source_id(), &path),
            Err(ResourceError::ResourceTooLarge(_))
        ));
        assert_eq!(session.state.sessions[0].inspected_paths(), 1);

        fs::write(&path, "ok").expect("accepted source");
        let (_, rollback) = session
            .reread_utf8_with_rollback(source_id(), &path)
            .expect("reread");
        session.rollback_reread(rollback).expect("rollback");

        assert_eq!(session.state.sessions[0].inspected_paths(), 1);
        assert_eq!((session.budget().files(), session.budget().bytes()), (0, 0));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn reread_rollback_releases_a_new_spelling_of_a_charged_canonical_path() {
        let root = TestDir::new("reread-rollback-canonical-alias");
        let nested = root.path().join("nested");
        fs::create_dir(&nested).expect("nested directory");
        let path = root.path().join("source.adoc");
        let alias = nested.join("..").join("source.adoc");
        fs::write(&path, "old").expect("source");
        let policy = LocalFilesystemPolicy::new(
            [root.path().to_owned()],
            FilesystemReadLimits {
                max_files: 2,
                max_total_bytes: 8,
                max_resource_bytes: 8,
            },
        )
        .expect("policy");
        let mut session = policy.session().expect("session");
        session.read_utf8(source_id(), &path).expect("initial read");
        session.state.sessions[0]
            .read_candidate_utf8(&path)
            .expect("initial canonical cache");
        assert_eq!(session.state.sessions[0].inspected_paths(), 1);

        fs::write(&path, "new").expect("replacement");
        let (_, rollback) = session
            .reread_utf8_with_rollback(source_id(), &alias)
            .expect("alias reread");
        assert_eq!(session.state.sessions[0].inspected_paths(), 2);
        session.rollback_reread(rollback).expect("rollback");

        assert_eq!(session.state.sessions[0].inspected_paths(), 1);
        assert_eq!((session.budget().files(), session.budget().bytes()), (1, 3));
        assert_eq!(
            session.state.sessions[0]
                .read_candidate_utf8(&path)
                .expect("alias rollback preserves the canonical cache")
                .source(),
            "old"
        );
    }

    #[test]
    fn rejected_new_reread_releases_only_its_candidate_inspection() {
        let root = TestDir::new("reread-candidate-rollback");
        let first = root.path().join("first.adoc");
        let second = root.path().join("second.adoc");
        let third = root.path().join("third.adoc");
        fs::write(&first, "a").expect("first source");
        fs::write(&second, "bb").expect("second source");
        fs::write(&third, "b").expect("third source");
        let policy = LocalFilesystemPolicy::new(
            [root.path().to_owned()],
            FilesystemReadLimits {
                max_files: 2,
                max_total_bytes: 2,
                max_resource_bytes: 2,
            },
        )
        .expect("policy");
        let mut session = policy.session().expect("session");

        session.read_utf8(source_id(), &first).expect("first read");
        let inspected = session.state.sessions[0].inspected_paths();
        assert_eq!(
            session.reread_utf8(source_id(), &second),
            Err(ResourceError::ByteLimit)
        );
        assert_eq!(session.state.sessions[0].inspected_paths(), inspected);
        session
            .reread_utf8(source_id(), &third)
            .expect("third read after rejected candidate");
        assert_eq!((session.budget().files(), session.budget().bytes()), (2, 2));
    }

    #[test]
    fn shared_byte_capacity_accepts_the_boundary_across_roots() {
        let first_root = TestDir::new("shared-byte-boundary-first");
        let second_root = TestDir::new("shared-byte-boundary-second");
        let first = first_root.path().join("first.adoc");
        let second = second_root.path().join("second.adoc");
        let third = second_root.path().join("third.adoc");
        fs::write(&first, "12").expect("first source");
        fs::write(&second, "34").expect("second source");
        fs::write(&third, "5").expect("third source");
        let policy = LocalFilesystemPolicy::new(
            [first_root.path().to_owned(), second_root.path().to_owned()],
            FilesystemReadLimits {
                max_files: 3,
                max_total_bytes: 4,
                max_resource_bytes: 4,
            },
        )
        .expect("policy");
        let mut session = policy.session().expect("session");

        session.read_utf8(source_id(), &first).expect("first read");
        session
            .read_utf8(source_id(), &second)
            .expect("boundary read");
        assert_eq!(
            session.read_utf8(source_id(), &third),
            Err(ResourceError::ByteLimit)
        );
        assert_eq!((session.budget().files(), session.budget().bytes()), (2, 4));
    }

    #[test]
    fn replacement_receives_its_previous_charge_before_the_bounded_read() {
        let root = TestDir::new("replacement-capacity");
        let path = root.path().join("document.adoc");
        fs::write(&path, "1234").expect("initial source");
        let policy = LocalFilesystemPolicy::new(
            [root.path().to_owned()],
            FilesystemReadLimits {
                max_files: 1,
                max_total_bytes: 6,
                max_resource_bytes: 5,
            },
        )
        .expect("policy");
        let mut session = policy.session().expect("session");
        session.read_utf8(source_id(), &path).expect("initial read");

        fs::write(&path, "12345").expect("replacement source");
        session
            .reread_utf8(source_id(), &path)
            .expect("replacement uses released charge");
        assert_eq!((session.budget().files(), session.budget().bytes()), (1, 5));
        fs::write(&path, "123456").expect("oversized replacement");
        assert_eq!(
            session.reread_utf8(source_id(), &path),
            Err(ResourceError::ResourceTooLarge(path))
        );
        assert_eq!((session.budget().files(), session.budget().bytes()), (1, 5));
    }

    #[test]
    fn budget_rejects_without_partially_charging() {
        let limits = FilesystemReadLimits {
            max_files: 1,
            max_total_bytes: 3,
            max_resource_bytes: 3,
        };
        let mut budget = ResourceBudget::default();
        budget.charge(Path::new("a"), 3, limits).expect("boundary");
        assert_eq!((budget.files(), budget.bytes()), (1, 3));
        assert_eq!(
            budget.charge(Path::new("b"), 1, limits),
            Err(ResourceError::FileLimit { limit: 1 })
        );
        assert_eq!((budget.files(), budget.bytes()), (1, 3));
    }

    #[test]
    fn filesystem_session_identity_never_wraps() {
        assert_eq!(next_session_id(u64::MAX - 1), Some(u64::MAX));
        assert_eq!(next_session_id(u64::MAX), None);
    }

    #[test]
    fn policy_rejects_files_outside_roots_and_directories() {
        let root = TestDir::new("root");
        let outside = TestDir::new("outside");
        let outside_file = outside.path().join("outside.adoc");
        fs::write(&outside_file, "outside").expect("write outside file");
        let mut session = policy(root.path(), 100).session().expect("session");

        assert!(matches!(
            session.read_utf8(source_id(), &outside_file),
            Err(ResourceError::OutsideRoots(_))
        ));
        assert!(matches!(
            session.read_utf8(source_id(), root.path()),
            Err(ResourceError::NotRegularFile(_))
        ));
    }

    #[cfg(unix)]
    #[test]
    fn policy_rejects_symlinks_that_escape_roots() {
        use std::os::unix::fs::symlink;

        let root = TestDir::new("symlink-root");
        let outside = TestDir::new("symlink-outside");
        let outside_file = outside.path().join("outside.adoc");
        fs::write(&outside_file, "outside").expect("write outside file");
        let link = root.path().join("escape.adoc");
        symlink(&outside_file, &link).expect("create symlink");

        assert!(matches!(
            policy(root.path(), 100)
                .session()
                .expect("session")
                .read_utf8(source_id(), &link),
            Err(ResourceError::OutsideRoots(_))
        ));
    }

    #[cfg(unix)]
    #[test]
    fn deepest_root_rejects_a_symlink_back_into_an_allowed_parent_root() {
        use std::os::unix::fs::symlink;

        let outer = TestDir::new("nested-outer");
        let inner = outer.path().join("inner");
        fs::create_dir(&inner).expect("inner root");
        let outer_file = outer.path().join("outer.adoc");
        fs::write(&outer_file, "outer").expect("outer file");
        let link = inner.join("escape.adoc");
        symlink(&outer_file, &link).expect("cross-boundary symlink");
        let policy = LocalFilesystemPolicy::new(
            [outer.path().to_owned(), inner],
            FilesystemReadLimits::default(),
        )
        .expect("policy");

        assert!(matches!(
            policy
                .session()
                .expect("session")
                .read_utf8(source_id(), &link),
            Err(ResourceError::OutsideRoots(_))
        ));
    }

    #[cfg(unix)]
    #[test]
    fn symlinks_between_allowed_roots_remain_confined_to_the_selected_root() {
        use std::os::unix::fs::symlink;

        let first = TestDir::new("cross-root-first");
        let second = TestDir::new("cross-root-second");
        let second_file = second.path().join("second.adoc");
        fs::write(&second_file, "second").expect("second file");
        let link = first.path().join("escape.adoc");
        symlink(&second_file, &link).expect("cross-root symlink");
        let policy = LocalFilesystemPolicy::new(
            [first.path().to_owned(), second.path().to_owned()],
            FilesystemReadLimits::default(),
        )
        .expect("policy");

        assert!(matches!(
            policy
                .session()
                .expect("session")
                .read_utf8(source_id(), &link),
            Err(ResourceError::OutsideRoots(_))
        ));
    }

    #[test]
    fn missing_and_permission_errors_keep_typed_identity() {
        let missing = PathBuf::from("missing.adoc");
        let denied = PathBuf::from("denied.adoc");
        assert_eq!(
            ResourceError::from(LocalTargetError::Missing(missing.clone())),
            ResourceError::Missing(missing)
        );
        assert_eq!(
            ResourceError::from(LocalTargetError::PermissionDenied(denied.clone())),
            ResourceError::PermissionDenied(denied)
        );
    }

    #[test]
    fn policy_constructor_preserves_public_root_error_categories() {
        let root = TestDir::new("policy-constructor-errors");
        let file = root.path().join("file.adoc");
        let missing = root.path().join("missing");
        fs::write(&file, "text").expect("regular file");

        assert!(matches!(
            LocalFilesystemPolicy::new([missing], FilesystemReadLimits::default()),
            Err(ResourceError::Missing(_))
        ));
        assert!(matches!(
            LocalFilesystemPolicy::new([file], FilesystemReadLimits::default()),
            Err(ResourceError::InvalidRoot)
        ));
    }

    #[cfg(unix)]
    #[test]
    fn authored_target_read_keeps_the_opened_file_when_the_leaf_is_replaced() {
        use std::os::unix::fs::symlink;

        let root = TestDir::new("target-race-root");
        let outside = TestDir::new("target-race-outside");
        let candidate = root.path().join("part.adoc");
        let moved = root.path().join("opened.adoc");
        let outside_file = outside.path().join("outside.adoc");
        fs::write(&candidate, "inside").expect("inside file");
        fs::write(&outside_file, "outside").expect("outside file");
        let mut session = policy(root.path(), 100).session().expect("session");

        let loaded = session
            .read_target_utf8_after_open(source_id(), root.path(), "part.adoc", || {
                fs::rename(&candidate, &moved).expect("retain opened file");
                symlink(&outside_file, &candidate).expect("replace with outside symlink");
            })
            .expect("opened source remains valid");

        assert_eq!(loaded.source(), "inside");
        #[cfg(target_os = "linux")]
        assert_eq!(loaded.canonical_path(), moved);
        #[cfg(not(target_os = "linux"))]
        assert_eq!(loaded.canonical_path(), candidate);
    }

    #[test]
    fn validated_target_enforces_encoding_and_per_resource_limit() {
        let root = TestDir::new("read");
        let invalid = root.path().join("invalid.adoc");
        let oversized = root.path().join("oversized.adoc");
        fs::write(&invalid, [0xff]).expect("write invalid UTF-8");
        fs::write(&oversized, "1234").expect("write oversized file");
        let mut session = policy(root.path(), 3).session().expect("session");

        assert!(matches!(
            session.read_utf8(source_id(), &invalid),
            Err(ResourceError::InvalidUtf8 { .. })
        ));
        assert!(matches!(
            session.read_utf8(source_id(), &oversized),
            Err(ResourceError::ResourceTooLarge(_))
        ));
    }

    #[cfg(unix)]
    #[test]
    fn validated_target_owns_the_bytes_captured_before_a_path_is_replaced() {
        use std::os::unix::fs::symlink;

        let root = TestDir::new("captured-root");
        let outside = TestDir::new("captured-outside");
        let candidate = root.path().join("part.adoc");
        let outside_file = outside.path().join("outside.adoc");
        fs::write(&candidate, "inside").expect("inside source");
        fs::write(&outside_file, "outside").expect("outside source");
        let mut session = policy(root.path(), 100).session().expect("session");
        let loaded = session
            .read_utf8(source_id(), &candidate)
            .expect("loaded target");

        fs::remove_file(&candidate).expect("replace candidate");
        symlink(&outside_file, &candidate).expect("outside symlink");

        assert_eq!(loaded.source(), "inside");
        assert_eq!(loaded.canonical_path(), candidate);
        assert_eq!(loaded.source_id().as_str(), "test-source");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn shared_session_keeps_the_opened_file_when_an_ancestor_is_replaced() {
        use std::os::unix::fs::symlink;

        let root = TestDir::new("session-race-root");
        let outside = TestDir::new("session-race-outside");
        let directory = root.path().join("parts");
        let moved = root.path().join("parts-opened");
        fs::create_dir(&directory).expect("inside directory");
        fs::write(directory.join("part.adoc"), "inside").expect("inside source");
        fs::write(outside.path().join("part.adoc"), "outside").expect("outside source");
        let mut session = policy(root.path(), 100).session().expect("session");

        let loaded = session
            .read_utf8_after_open(source_id(), &directory.join("part.adoc"), || {
                fs::rename(&directory, &moved).expect("move opened ancestor");
                symlink(outside.path(), &directory).expect("replace ancestor with symlink");
            })
            .expect("opened file remains readable");

        assert_eq!(loaded.source(), "inside");
        assert_ne!(loaded.source(), "outside");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn unlinked_file_does_not_charge_or_cache_a_literal_deleted_suffix_path() {
        let root = TestDir::new("deleted-suffix-budget");
        let candidate = root.path().join("part.adoc");
        let suffix = root.path().join("part.adoc (deleted)");
        fs::write(&candidate, "opened").expect("opened source");
        fs::write(&suffix, "literal suffix").expect("suffix source");
        let mut session = policy(root.path(), 100).session().expect("session");

        let error = session
            .read_utf8_after_open(source_id(), &candidate, || {
                fs::remove_file(&candidate).expect("unlink opened source");
            })
            .expect_err("unlinked identity must fail closed");

        assert!(matches!(error, ResourceError::Unverifiable(_)));
        assert_eq!((session.budget().files(), session.budget().bytes()), (0, 0));

        let loaded = session
            .read_utf8(source_id(), &suffix)
            .expect("literal suffix remains independently readable");
        assert_eq!(loaded.canonical_path(), suffix);
        assert_eq!(loaded.source(), "literal suffix");
        assert_eq!(
            (session.budget().files(), session.budget().bytes()),
            (1, 14)
        );
    }
}
