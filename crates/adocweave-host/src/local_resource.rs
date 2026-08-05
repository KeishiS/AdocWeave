use std::collections::{BTreeMap, VecDeque};
use std::error::Error;
use std::fmt;
#[cfg(not(target_os = "linux"))]
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::local_target::{
    FilesystemRaceResistance, LocalTargetError, LocalTargetPolicy, LocalTargetSession,
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
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LoadedFilesystemSource {
    source_id: LogicalSourceId,
    source: Arc<str>,
    provenance: FilesystemProvenance,
}

/// Opaque state used to undo one successfully charged filesystem reread.
#[derive(Clone, Debug)]
pub struct FilesystemReadRollback {
    session_id: u64,
    applied_generation: u64,
    canonical_path: PathBuf,
    candidate_path: PathBuf,
    session_index: usize,
    candidate_was_inspected: bool,
    previous_charge: Option<FilesystemCharge>,
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

    pub fn into_parts(self) -> (LogicalSourceId, Arc<str>) {
        (self.source_id, self.source)
    }
}

/// Per-command filesystem capability shared by all native resource consumers.
///
/// Construction opens one policy for each canonical root. Reads are delegated
/// to the same handle-relative implementation used by local-target checks, and
/// one budget is enforced across every root.
#[derive(Debug)]
pub struct LocalFilesystemSession {
    session_id: u64,
    next_generation: u64,
    roots: Vec<PathBuf>,
    sessions: Vec<LocalTargetSession>,
    limits: FilesystemReadLimits,
    budget: ResourceBudget,
    charged: BTreeMap<PathBuf, FilesystemCharge>,
}

static NEXT_FILESYSTEM_SESSION_ID: AtomicU64 = AtomicU64::new(1);

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

    /// Adds independently selected filesystem roots.
    ///
    /// Each root establishes a new authority from its current path. Use this
    /// for roots selected directly by the host rather than roots learned from
    /// a project configuration.
    pub fn add_independent_roots(
        &mut self,
        roots: impl IntoIterator<Item = PathBuf>,
    ) -> Result<Vec<PathBuf>, ResourceError> {
        let mut policies = BTreeMap::new();
        for path in roots {
            let policy = LocalTargetPolicy::new(&path)
                .map_err(|error| map_policy_root_error(path, error))?;
            let root = policy.root().to_owned();
            self.retain_pending_policy(&policies, &root)?;
            policies.entry(root).or_insert(policy);
        }
        Ok(self.insert_policies(policies.into_values()))
    }

    /// Adds roots derived below one already opened authority.
    ///
    /// No root path is reopened from the process namespace. On Linux each
    /// directory is opened relative to the retained anchor handle, so a
    /// concurrent replacement of the anchor path cannot redirect the derived
    /// authority. Requested roots must not cross symbolic links.
    pub fn add_confined_roots(
        &mut self,
        anchor: &Path,
        roots: impl IntoIterator<Item = PathBuf>,
    ) -> Result<Vec<PathBuf>, ResourceError> {
        let anchor_policy = self
            .root_policy(anchor)
            .cloned()
            .ok_or_else(|| ResourceError::OutsideRoots(anchor.to_owned()))?;
        let mut policies = BTreeMap::new();
        for path in roots {
            let policy = anchor_policy
                .derive_confined_directory(&path)
                .map_err(|error| map_policy_root_error(path, error))?;
            let root = policy.root().to_owned();
            self.retain_pending_policy(&policies, &root)?;
            policies.entry(root).or_insert(policy);
        }
        Ok(self.insert_policies(policies.into_values()))
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

    fn insert_policies(
        &mut self,
        policies: impl IntoIterator<Item = LocalTargetPolicy>,
    ) -> Vec<PathBuf> {
        let mut unique = std::mem::take(&mut self.root_policies)
            .into_iter()
            .map(|policy| (policy.root().to_owned(), policy))
            .collect::<BTreeMap<_, _>>();
        let mut selected = Vec::new();
        for policy in policies {
            let root = policy.root().to_owned();
            unique.entry(root.clone()).or_insert(policy);
            selected.push(root);
        }
        selected.sort();
        selected.dedup();
        self.roots = unique.keys().cloned().collect();
        self.root_policies = unique.into_values().collect();
        selected
    }

    /// Returns the retained authority for one exact canonical root.
    pub fn root_policy(&self, root: &Path) -> Option<&LocalTargetPolicy> {
        self.root_policies
            .iter()
            .find(|policy| policy.root() == root)
    }

    pub fn session(&self) -> Result<LocalFilesystemSession, ResourceError> {
        self.session_for_roots(&self.roots, self.limits)
    }

    /// Creates a bounded session from a subset of the already opened roots.
    ///
    /// Requested roots must use the exact canonical spelling returned by
    /// [`Self::roots`]. No filesystem path is reopened by this operation.
    pub fn session_for_roots(
        &self,
        roots: &[PathBuf],
        limits: FilesystemReadLimits,
    ) -> Result<LocalFilesystemSession, ResourceError> {
        if limits.max_files > self.limits.max_files
            || limits.max_total_bytes > self.limits.max_total_bytes
            || limits.max_resource_bytes > self.limits.max_resource_bytes
        {
            return Err(ResourceError::Unverifiable(
                "filesystem session limits exceed the policy limits".to_owned(),
            ));
        }
        if roots.is_empty() {
            return Err(ResourceError::NoRoots);
        }
        let mut root_policies = roots
            .iter()
            .map(|root| {
                self.root_policy(root)
                    .cloned()
                    .ok_or_else(|| ResourceError::OutsideRoots(root.clone()))
            })
            .collect::<Result<Vec<_>, _>>()?;
        root_policies.sort_by(|left, right| left.root().cmp(right.root()));
        root_policies.dedup_by(|left, right| left.root() == right.root());
        let roots = root_policies
            .iter()
            .map(|policy| policy.root().to_owned())
            .collect::<Vec<_>>();
        let sessions = root_policies
            .into_iter()
            .map(|policy| {
                LocalTargetSession::new(
                    policy,
                    limits.max_files,
                    FilesystemReadLimits {
                        max_files: usize::MAX,
                        max_total_bytes: u64::MAX,
                        max_resource_bytes: limits.max_resource_bytes,
                    },
                )
            })
            .collect();
        let session_id = NEXT_FILESYSTEM_SESSION_ID
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, next_session_id)
            .map_err(|_| ResourceError::SessionIdentityExhausted)?;
        Ok(LocalFilesystemSession {
            session_id,
            next_generation: 1,
            roots,
            sessions,
            limits,
            budget: ResourceBudget::default(),
            charged: BTreeMap::new(),
        })
    }
}

fn map_policy_root_error(path: PathBuf, error: LocalTargetError) -> ResourceError {
    match error {
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

    pub fn roots(&self) -> &[PathBuf] {
        &self.roots
    }

    pub const fn limits(&self) -> FilesystemReadLimits {
        self.limits
    }

    /// Scans every configured root for regular `.adoc` files.
    ///
    /// Directory entries and candidates are sorted before reading, symlinks are
    /// not followed, and all reads consume this session's shared resource
    /// budget. The caller supplies logical identities so canonical filesystem
    /// paths do not become semantic source IDs.
    pub fn scan_utf8(
        &mut self,
        mut source_id: impl FnMut(&Path) -> Result<LogicalSourceId, ResourceError>,
    ) -> Result<Vec<LoadedFilesystemSource>, ResourceError> {
        let paths = self.discover_adoc_paths()?;
        if paths.len() > self.limits.max_files {
            return Err(ResourceError::FileLimit {
                limit: self.limits.max_files,
            });
        }
        paths
            .into_iter()
            .map(|path| {
                let source_id = source_id(&path)?;
                self.read_utf8(source_id, &path)
            })
            .collect()
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
        self.discover_adoc_paths_with_limit(Self::MAX_SCAN_ENTRIES, exclude_directory, is_cancelled)
    }

    fn discover_adoc_paths_with_limit(
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
            for root in &self.roots {
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
                        let mut children = Vec::new();
                        for child in
                            fs::read_dir(&path).map_err(|source| ResourceError::Inspect {
                                path: path.clone(),
                                source: source.to_string(),
                            })?
                        {
                            if is_cancelled() {
                                return Err(ResourceError::Unverifiable(
                                    "local filesystem scan was cancelled".to_owned(),
                                ));
                            }
                            children.push(child.map_err(|source| ResourceError::Inspect {
                                path: path.clone(),
                                source: source.to_string(),
                            })?);
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
        for (root, session) in self.roots.iter().zip(&self.sessions) {
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
                let mut entries =
                    Dir::read_from(&directory).map_err(|source| ResourceError::Inspect {
                        path: path.clone(),
                        source: source.to_string(),
                    })?;
                let mut children = Vec::<(OsString, FileType)>::new();
                for child in &mut entries {
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

    /// Returns the concurrent-filesystem guarantee of all configured roots.
    pub fn race_resistance(&self) -> FilesystemRaceResistance {
        self.sessions
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
        self.read_utf8_with(source_id, path, || {})
    }

    /// Resolves and reads one authored target relative to an absolute base.
    pub fn read_target_utf8(
        &mut self,
        source_id: LogicalSourceId,
        base: &Path,
        target: &str,
    ) -> Result<LoadedFilesystemSource, ResourceError> {
        let index = self.root_index(base)?;
        let budget = self.budget;
        let charged = &self.charged;
        let limits = self.limits;
        let file_limit_denied = std::cell::Cell::new(false);
        let loaded = self.sessions[index]
            .read_utf8_with_capacity(base, target, |canonical| {
                shared_read_capacity(budget, charged, limits, canonical, &file_limit_denied)
            })
            .map_err(|error| map_shared_read_error(error, limits, file_limit_denied.get()))?;
        self.finish_read(source_id, loaded)
    }

    /// Reopens an absolute path while retaining this session's shared budget.
    pub fn reread_utf8(
        &mut self,
        source_id: LogicalSourceId,
        path: &Path,
    ) -> Result<LoadedFilesystemSource, ResourceError> {
        self.reread_utf8_with_rollback(source_id, path)
            .map(|(loaded, _)| loaded)
    }

    /// Reopens a path and returns the state needed to undo its budget charge.
    ///
    /// Callers which update another state store after reading must retain the
    /// rollback value until that update commits.
    pub fn reread_utf8_with_rollback(
        &mut self,
        source_id: LogicalSourceId,
        path: &Path,
    ) -> Result<(LoadedFilesystemSource, FilesystemReadRollback), ResourceError> {
        let index = self.root_index(path)?;
        let candidate_was_inspected = self.sessions[index].has_inspected_candidate(path);
        let budget = self.budget;
        let charged = &self.charged;
        let limits = self.limits;
        let file_limit_denied = std::cell::Cell::new(false);
        let loaded =
            match self.sessions[index].reread_candidate_utf8_with_capacity(path, |canonical| {
                shared_read_capacity(budget, charged, limits, canonical, &file_limit_denied)
            }) {
                Ok(loaded) => loaded,
                Err(error) => {
                    if !candidate_was_inspected {
                        self.sessions[index].release_candidate(path);
                    }
                    return Err(map_shared_read_error(
                        error,
                        limits,
                        file_limit_denied.get(),
                    ));
                }
            };
        let canonical_path = loaded.canonical_path().to_owned();
        let previous_charge = self.charged.get(&canonical_path).copied();
        match self.finish_read(source_id, loaded) {
            Ok(loaded) => {
                let applied_generation = self
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
                        candidate_was_inspected,
                        previous_charge,
                    },
                ))
            }
            Err(error) => {
                if !candidate_was_inspected {
                    self.sessions[index].release_candidate(path);
                }
                Err(error)
            }
        }
    }

    /// Restores the charge replaced by [`Self::reread_utf8_with_rollback`].
    pub fn rollback_reread(
        &mut self,
        rollback: FilesystemReadRollback,
    ) -> Result<(), ResourceError> {
        if rollback.session_id != self.session_id {
            return Err(ResourceError::InvalidRollback);
        }
        let Some(current) = self.charged.get(&rollback.canonical_path).copied() else {
            return Err(ResourceError::InvalidRollback);
        };
        if current.generation != rollback.applied_generation {
            return Err(ResourceError::InvalidRollback);
        }
        match rollback.previous_charge {
            Some(previous) => {
                self.budget
                    .restore_replacement(current.bytes, previous.bytes);
                self.charged.insert(rollback.canonical_path, previous);
            }
            None => {
                self.budget.release(current.bytes);
                self.charged.remove(&rollback.canonical_path);
            }
        }
        if !rollback.candidate_was_inspected {
            self.sessions[rollback.session_index].release_candidate(&rollback.candidate_path);
        }
        Ok(())
    }

    /// Releases the budget charge for a resource removed from the caller's workspace.
    pub fn release(&mut self, path: &Path) {
        if let Some(charge) = self.charged.remove(path) {
            self.budget.release(charge.bytes);
        }
        if let Ok(index) = self.root_index(path) {
            self.sessions[index].release_candidate(path);
        }
    }

    #[cfg(test)]
    fn read_utf8_after_open(
        &mut self,
        source_id: LogicalSourceId,
        path: &Path,
        after_open: impl FnOnce(),
    ) -> Result<LoadedFilesystemSource, ResourceError> {
        self.read_utf8_with(source_id, path, after_open)
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
        let loaded = self.sessions[index]
            .read_utf8_after_open(base, target, after_open)
            .map_err(ResourceError::from)?;
        self.finish_read(source_id, loaded)
    }

    fn read_utf8_with(
        &mut self,
        source_id: LogicalSourceId,
        path: &Path,
        after_open: impl FnOnce(),
    ) -> Result<LoadedFilesystemSource, ResourceError> {
        if !path.is_absolute() {
            return Err(ResourceError::PathNotAbsolute(path.to_owned()));
        }
        let index = self.root_index(path)?;
        let candidate = path.to_owned();
        if candidate == self.roots[index] {
            return Err(ResourceError::NotRegularFile(candidate));
        }
        let budget = self.budget;
        let charged = &self.charged;
        let limits = self.limits;
        let file_limit_denied = std::cell::Cell::new(false);
        let loaded = self.sessions[index]
            .read_candidate_utf8_with_capacity(&candidate, false, true, after_open, |canonical| {
                shared_read_capacity(budget, charged, limits, canonical, &file_limit_denied)
            })
            .map_err(|error| map_shared_read_error(error, limits, file_limit_denied.get()))?;
        self.finish_read(source_id, loaded)
    }

    fn root_index(&self, path: &Path) -> Result<usize, ResourceError> {
        if !path.is_absolute() {
            return Err(ResourceError::PathNotAbsolute(path.to_owned()));
        }
        self.roots
            .iter()
            .enumerate()
            .filter(|(_, root)| path.starts_with(root))
            .max_by_key(|(_, root)| root.components().count())
            .map(|(index, _)| index)
            .ok_or_else(|| ResourceError::OutsideRoots(path.to_owned()))
    }

    fn finish_read(
        &mut self,
        source_id: LogicalSourceId,
        loaded: crate::local_target::LoadedLocalTarget,
    ) -> Result<LoadedFilesystemSource, ResourceError> {
        let (canonical_path, source) = loaded.into_parts();
        let bytes = source.len() as u64;
        let previous = self.charged.get(&canonical_path).copied();
        self.budget.replace(
            &canonical_path,
            previous.map(|charge| charge.bytes),
            bytes,
            self.limits,
        )?;
        let generation = self.next_generation;
        self.next_generation = self
            .next_generation
            .checked_add(1)
            .expect("filesystem session generation exhausted");
        self.charged.insert(
            canonical_path.clone(),
            FilesystemCharge { bytes, generation },
        );
        Ok(LoadedFilesystemSource {
            source_id,
            source: Arc::from(source),
            provenance: FilesystemProvenance { canonical_path },
        })
    }

    pub const fn budget(&self) -> ResourceBudget {
        self.budget
    }
}

fn shared_read_capacity(
    budget: ResourceBudget,
    charged: &BTreeMap<PathBuf, FilesystemCharge>,
    limits: FilesystemReadLimits,
    canonical: &Path,
    file_limit_denied: &std::cell::Cell<bool>,
) -> crate::local_target::CandidateReadCapacity {
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
        let mut additions = Vec::new();
        for index in 1..MAX_FILESYSTEM_POLICY_ROOTS {
            let root = parent.path().join(format!("root-{index:03}"));
            fs::create_dir(&root).expect("additional root");
            additions.push(root);
        }
        policy
            .add_independent_roots(additions)
            .expect("fill policy root limit");
        let before = policy.roots().to_vec();
        let rejected = parent.path().join("root-over-limit");
        fs::create_dir(&rejected).expect("rejected root");

        assert_eq!(
            policy
                .add_independent_roots([rejected.clone()])
                .expect_err("root limit"),
            ResourceError::RootLimit {
                limit: MAX_FILESYSTEM_POLICY_ROOTS,
            }
        );
        assert_eq!(policy.roots(), before);
        assert!(before.iter().all(|root| policy.root_policy(root).is_some()));
        assert_eq!(
            policy
                .add_independent_roots([before[0].clone()])
                .expect("duplicate root at the limit"),
            [before[0].clone()]
        );
        assert_eq!(policy.roots(), before);
        drop(policy);

        let mut staged = LocalFilesystemPolicy::new(
            before[..MAX_FILESYSTEM_POLICY_ROOTS - 1].iter().cloned(),
            FilesystemReadLimits::default(),
        )
        .expect("policy below the limit");
        let staged_before = staged.roots().to_vec();
        assert_eq!(
            staged
                .add_independent_roots([
                    before[MAX_FILESYSTEM_POLICY_ROOTS - 1].clone(),
                    rejected.clone(),
                ])
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
                policy.session_for_roots(std::slice::from_ref(&root_path), limits),
                Err(ResourceError::Unverifiable(reason))
                    if reason == "filesystem session limits exceed the policy limits"
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

        let selected = policy
            .add_confined_roots(&anchor, [nested.clone()])
            .expect("derive nested authority");
        let mut session = policy
            .session_for_roots(&selected, FilesystemReadLimits::default())
            .expect("derived session");
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
        assert_eq!(session.sessions[0].inspected_paths(), 1);

        fs::write(&path, "ok").expect("accepted source");
        let (_, rollback) = session
            .reread_utf8_with_rollback(source_id(), &path)
            .expect("reread");
        session.rollback_reread(rollback).expect("rollback");

        assert_eq!(session.sessions[0].inspected_paths(), 1);
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
        assert_eq!(session.sessions[0].inspected_paths(), 1);

        fs::write(&path, "new").expect("replacement");
        let (_, rollback) = session
            .reread_utf8_with_rollback(source_id(), &alias)
            .expect("alias reread");
        assert_eq!(session.sessions[0].inspected_paths(), 2);
        session.rollback_reread(rollback).expect("rollback");

        assert_eq!(session.sessions[0].inspected_paths(), 1);
        assert_eq!((session.budget().files(), session.budget().bytes()), (1, 3));
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
        let inspected = session.sessions[0].inspected_paths();
        assert_eq!(
            session.reread_utf8(source_id(), &second),
            Err(ResourceError::ByteLimit)
        );
        assert_eq!(session.sessions[0].inspected_paths(), inspected);
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
            Err(ResourceError::Inspect { .. })
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
}
