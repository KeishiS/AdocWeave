use std::collections::BTreeMap;
#[cfg(not(target_os = "linux"))]
use std::collections::BTreeSet;
use std::error::Error;
use std::fmt;
use std::fs;
use std::io::Read;
#[cfg(target_os = "linux")]
use std::io::Write;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
#[cfg(target_os = "linux")]
use std::sync::atomic::{AtomicU64, Ordering};

use crate::local_resource::FilesystemReadLimits;

/// How many times a confined open may be retried after a concurrent-change race.
///
/// Each retry is a single syscall against a path that is almost always stable,
/// so a small bound absorbs ordinary churn without turning a persistently
/// changing directory into an unbounded wait.
#[cfg(target_os = "linux")]
const CONFINED_OPEN_ATTEMPTS: u32 = 8;

#[cfg(target_os = "linux")]
static NEXT_REPLACEMENT_FILE: AtomicU64 = AtomicU64::new(1);

#[cfg(target_os = "linux")]
struct TemporaryReplacement {
    path: PathBuf,
    file: fs::File,
    committed: bool,
}

#[cfg(target_os = "linux")]
impl Drop for TemporaryReplacement {
    fn drop(&mut self) {
        if !self.committed {
            let _ = fs::remove_file(&self.path);
        }
    }
}

#[cfg(all(test, target_os = "linux"))]
thread_local! {
    static FORCED_OPENAT2_ERROR: std::cell::Cell<Option<rustix::io::Errno>> = const {
        std::cell::Cell::new(None)
    };
    static FORCED_FD_PATH_ERROR: std::cell::Cell<Option<std::io::ErrorKind>> = const {
        std::cell::Cell::new(None)
    };
    static FORCED_EXPLICIT_RETRY_OPEN_ERROR: std::cell::Cell<Option<rustix::io::Errno>> = const {
        std::cell::Cell::new(None)
    };
}

#[cfg(target_os = "linux")]
fn confined_openat2(
    root: &fs::File,
    relative: &Path,
    flags: rustix::fs::OFlags,
    resolve: rustix::fs::ResolveFlags,
) -> rustix::io::Result<rustix::fd::OwnedFd> {
    #[cfg(test)]
    if let Some(error) = FORCED_OPENAT2_ERROR.with(std::cell::Cell::get) {
        return Err(error);
    }
    rustix::fs::openat2(root, relative, flags, rustix::fs::Mode::empty(), resolve)
}

#[derive(Clone, Copy)]
pub(crate) struct CandidateReadCapacity {
    pub allow_file: bool,
    pub max_total_bytes: u64,
    pub max_resource_bytes: u64,
}

/// Concurrent-filesystem guarantee provided by the active platform adapter.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FilesystemRaceResistance {
    /// Resolution and use are confined to handles below the configured root.
    HandleRelative,
    /// Path checks assume the workspace is not modified concurrently.
    StaticSnapshotOnly,
}

/// Filesystem boundary for checking an authored relative target.
///
/// The policy owns one canonical project root. It permits parent components
/// only while the normalized path remains below that root, then resolves
/// symlinks before accepting an existing regular file.
/// Equality compares this configured canonical root, not the identity of an
/// operating-system handle acquired by two independently constructed values.
#[derive(Clone)]
pub struct LocalTargetPolicy {
    root: PathBuf,
    #[cfg(target_os = "linux")]
    root_handle: Arc<fs::File>,
}

impl fmt::Debug for LocalTargetPolicy {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("LocalTargetPolicy")
            .field("root", &self.root)
            .finish_non_exhaustive()
    }
}

impl PartialEq for LocalTargetPolicy {
    fn eq(&self, other: &Self) -> bool {
        self.root == other.root
    }
}

impl Eq for LocalTargetPolicy {}

#[cfg(target_os = "linux")]
struct OpenedTarget {
    canonical_path: PathBuf,
    file: fs::File,
}

impl LocalTargetPolicy {
    pub fn new(root: &Path) -> Result<Self, LocalTargetError> {
        #[cfg(target_os = "linux")]
        {
            let (canonical, root_handle) = open_root_directory(root)?;
            Ok(Self {
                root: canonical,
                root_handle: Arc::new(root_handle),
            })
        }
        #[cfg(not(target_os = "linux"))]
        {
            let canonical = root
                .canonicalize()
                .map_err(|source| classify_io(root.to_owned(), source))?;
            if !canonical.is_dir() {
                return Err(LocalTargetError::NotDirectory(canonical));
            }
            Ok(Self { root: canonical })
        }
    }

    /// Loads one explicitly selected UTF-8 file and retains its parent
    /// directory as the resulting authority.
    ///
    /// Symbolic links are accepted because the caller selected this path
    /// directly. On Linux the opened file is matched to a second open through
    /// the retained parent handle, so a concurrent parent replacement cannot
    /// combine bytes from one namespace with authority from another.
    pub fn load_explicit_utf8(
        path: &Path,
        max_bytes: u64,
    ) -> Result<(Self, LoadedLocalTarget), LocalTargetError> {
        Self::load_explicit_utf8_with(path, max_bytes, || {})
    }

    fn load_explicit_utf8_with(
        path: &Path,
        max_bytes: u64,
        after_open: impl FnOnce(),
    ) -> Result<(Self, LoadedLocalTarget), LocalTargetError> {
        #[cfg(target_os = "linux")]
        {
            use std::os::unix::fs::MetadataExt;

            use rustix::fs::{Mode, OFlags, open};

            let mut after_open = Some(after_open);
            let mut prior_race_failure = None;
            for _ in 0..CONFINED_OPEN_ATTEMPTS {
                #[cfg(test)]
                if prior_race_failure.is_some()
                    && let Some(error) = FORCED_EXPLICIT_RETRY_OPEN_ERROR.with(std::cell::Cell::get)
                {
                    return Err(classify_errno(path, error));
                }
                let file = match open(
                    path,
                    OFlags::RDONLY | OFlags::NONBLOCK | OFlags::CLOEXEC,
                    Mode::empty(),
                ) {
                    Ok(file) => fs::File::from(file),
                    Err(error) => {
                        let error = classify_errno(path, error);
                        // A missing retry target confirms the same namespace
                        // race that made the preceding verification fail. All
                        // other errors describe the current attempt and keep
                        // their more specific public classification.
                        return Err(if matches!(error, LocalTargetError::Missing(_)) {
                            prior_race_failure.unwrap_or(error)
                        } else {
                            error
                        });
                    }
                };
                let metadata = file
                    .metadata()
                    .map_err(|source| classify_io(path.to_owned(), source))?;
                if !metadata.is_file() {
                    return Err(LocalTargetError::NotFile(path.to_owned()));
                }
                if let Some(callback) = after_open.take() {
                    callback();
                }
                let selected = path_from_file_handle(&file, path)?;
                let procfs_reports_deleted = procfs_reports_deleted_path(&selected);
                let file_name = selected
                    .file_name()
                    .ok_or_else(|| LocalTargetError::Unverifiable(path.display().to_string()))?;
                let parent = selected
                    .parent()
                    .ok_or_else(|| LocalTargetError::Unverifiable(path.display().to_string()))?;
                let policy = match Self::new(parent) {
                    Ok(policy) => policy,
                    Err(LocalTargetError::Missing(_)) => {
                        prior_race_failure = Some(explicit_target_changed_error());
                        continue;
                    }
                    Err(error) => return Err(error),
                };
                let candidate = policy.root.join(file_name);
                let verified = match policy.open_confined(&candidate) {
                    Ok(verified) => verified,
                    Err(LocalTargetError::Missing(_)) => {
                        prior_race_failure = Some(explicit_target_changed_error());
                        continue;
                    }
                    Err(LocalTargetError::NotFile(_)) if procfs_reports_deleted => {
                        return Err(explicit_target_changed_error());
                    }
                    Err(error) => return Err(error),
                };
                let verified_metadata = verified
                    .file
                    .metadata()
                    .map_err(|source| classify_io(candidate.clone(), source))?;
                if metadata.dev() != verified_metadata.dev()
                    || metadata.ino() != verified_metadata.ino()
                {
                    prior_race_failure = Some(explicit_target_changed_error());
                    continue;
                }
                let loaded = read_bounded_utf8(file, candidate, max_bytes)?;
                return Ok((policy, loaded));
            }
            Err(prior_race_failure.unwrap_or_else(explicit_target_changed_error))
        }
        #[cfg(not(target_os = "linux"))]
        {
            let selected = path
                .canonicalize()
                .map_err(|source| classify_io(path.to_owned(), source))?;
            let file_name = selected
                .file_name()
                .ok_or_else(|| LocalTargetError::Unverifiable(path.display().to_string()))?;
            let parent = selected
                .parent()
                .ok_or_else(|| LocalTargetError::Unverifiable(path.display().to_string()))?;
            let policy = Self::new(parent)?;
            after_open();
            let candidate = policy.root.join(file_name);
            let file = policy.open_confined(&candidate)?;
            let loaded = read_bounded_utf8(file, candidate, max_bytes)?;
            Ok((policy, loaded))
        }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub const fn race_resistance(&self) -> FilesystemRaceResistance {
        #[cfg(target_os = "linux")]
        {
            FilesystemRaceResistance::HandleRelative
        }
        #[cfg(not(target_os = "linux"))]
        {
            FilesystemRaceResistance::StaticSnapshotOnly
        }
    }

    /// Compares a regular file through its retained parent-directory handle.
    ///
    /// Linux resolves the target, temporary file and rename from the retained
    /// parent-directory handle, so replacing an ancestor path cannot redirect
    /// the read. `Ok(false)` reports a concurrent content change.
    #[cfg(target_os = "linux")]
    pub fn candidate_contents_match(
        &self,
        candidate: &Path,
        expected: &[u8],
    ) -> Result<bool, LocalTargetError> {
        use std::os::fd::AsRawFd;

        let candidate = self.normalize_candidate(candidate)?;
        let parent = candidate.parent().ok_or_else(|| {
            LocalTargetError::Unverifiable("write target has no parent directory".to_owned())
        })?;
        let file_name = candidate.file_name().ok_or_else(|| {
            LocalTargetError::Unverifiable("write target has no file name".to_owned())
        })?;
        let directory = self.open_directory_no_symlinks(parent)?;
        let target =
            PathBuf::from(format!("/proc/self/fd/{}", directory.as_raw_fd())).join(file_name);
        Ok(read_for_comparison(&target, expected.len(), &candidate)? == expected)
    }

    /// Rechecks a regular file immediately before replacing it atomically.
    ///
    /// Linux resolves the target, temporary file and rename from the retained
    /// parent-directory handle, so replacing an ancestor path cannot redirect
    /// the write. `Ok(false)` reports a content change observed by the final
    /// recheck. The comparison and rename are separate operations, so this is
    /// not a compare-and-swap against writers that ignore this API.
    #[cfg(target_os = "linux")]
    pub fn replace_candidate_after_recheck(
        &self,
        candidate: &Path,
        original: &[u8],
        replacement: &[u8],
    ) -> Result<bool, LocalTargetError> {
        use std::os::fd::AsRawFd;

        let candidate = self.normalize_candidate(candidate)?;
        let parent = candidate.parent().ok_or_else(|| {
            LocalTargetError::Unverifiable("write target has no parent directory".to_owned())
        })?;
        let file_name = candidate.file_name().ok_or_else(|| {
            LocalTargetError::Unverifiable("write target has no file name".to_owned())
        })?;
        let directory = self.open_directory_no_symlinks(parent)?;
        let descriptor_root = PathBuf::from(format!("/proc/self/fd/{}", directory.as_raw_fd()));
        let target = descriptor_root.join(file_name);
        let metadata = fs::symlink_metadata(&target)
            .map_err(|source| classify_io(candidate.clone(), source))?;
        if metadata.file_type().is_symlink() || !metadata.is_file() {
            return Err(LocalTargetError::NotFile(candidate));
        }
        if read_for_comparison(&target, original.len(), &candidate)? != original {
            return Ok(false);
        }

        let mut temporary = None;
        for _ in 0..CONFINED_OPEN_ATTEMPTS {
            let sequence = NEXT_REPLACEMENT_FILE.fetch_add(1, Ordering::Relaxed);
            let path =
                descriptor_root.join(format!(".adocweave-{}-{sequence}.tmp", std::process::id()));
            match fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&path)
            {
                Ok(file) => {
                    temporary = Some(TemporaryReplacement {
                        path,
                        file,
                        committed: false,
                    });
                    break;
                }
                Err(source) if source.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(source) => return Err(classify_io(candidate.clone(), source)),
            }
        }
        let mut temporary = temporary.ok_or_else(|| {
            LocalTargetError::Unverifiable(format!(
                "could not create a unique temporary file for {}",
                candidate.display()
            ))
        })?;
        temporary
            .file
            .set_permissions(metadata.permissions())
            .map_err(|source| classify_io(candidate.clone(), source))?;
        temporary
            .file
            .write_all(replacement)
            .and_then(|()| temporary.file.sync_all())
            .map_err(|source| classify_io(candidate.clone(), source))?;
        if read_for_comparison(&target, original.len(), &candidate)? != original {
            return Ok(false);
        }
        fs::rename(&temporary.path, &target)
            .map_err(|source| classify_io(candidate.clone(), source))?;
        temporary.committed = true;
        directory
            .sync_all()
            .map_err(|source| classify_io(candidate, source))?;
        Ok(true)
    }

    #[cfg(target_os = "linux")]
    pub(crate) fn root_directory_handle(&self) -> Result<fs::File, LocalTargetError> {
        use rustix::fs::{Mode, OFlags, openat};

        openat(
            self.root_handle.as_ref(),
            ".",
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .map(fs::File::from)
        .map_err(|error| classify_errno(&self.root, error))
    }

    #[cfg(target_os = "linux")]
    pub(crate) fn open_directory_no_symlinks(
        &self,
        candidate: &Path,
    ) -> Result<fs::File, LocalTargetError> {
        use rustix::fd::OwnedFd;
        use rustix::fs::{Mode, OFlags, ResolveFlags, openat};

        let relative = candidate
            .strip_prefix(&self.root)
            .map_err(|_| LocalTargetError::OutsideRoot(candidate.to_owned()))?;
        if relative.as_os_str().is_empty() {
            return self.root_directory_handle();
        }
        let flags = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC;
        let mut attempts = 0;
        let directory = loop {
            let outcome = confined_openat2(
                self.root_handle.as_ref(),
                relative,
                flags,
                ResolveFlags::BENEATH | ResolveFlags::NO_MAGICLINKS | ResolveFlags::NO_SYMLINKS,
            );
            attempts += 1;
            if matches!(outcome, Err(rustix::io::Errno::AGAIN)) && attempts < CONFINED_OPEN_ATTEMPTS
            {
                continue;
            }
            break outcome;
        };
        let directory = match directory {
            Ok(directory) => directory,
            Err(error)
                if error == rustix::io::Errno::NOSYS || error == rustix::io::Errno::INVAL =>
            {
                let mut directory: OwnedFd = openat(
                    self.root_handle.as_ref(),
                    ".",
                    OFlags::PATH | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                    Mode::empty(),
                )
                .map_err(|error| classify_errno(candidate, error))?;
                let mut components = relative.components().peekable();
                while let Some(component) = components.next() {
                    let Component::Normal(name) = component else {
                        return Err(LocalTargetError::Unverifiable(
                            candidate.to_string_lossy().into_owned(),
                        ));
                    };
                    let component_flags = if components.peek().is_some() {
                        OFlags::PATH | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC
                    } else {
                        flags
                    };
                    directory = openat(&directory, name, component_flags, Mode::empty())
                        .map_err(|error| classify_errno(candidate, error))?;
                }
                directory
            }
            Err(error) => return Err(classify_errno(candidate, error)),
        };
        Ok(fs::File::from(directory))
    }

    /// Derives a directory policy below this already opened root.
    ///
    /// The authored path must not contain a symbolic-link hop. Keeping the
    /// candidate spelling while opening it relative to `root_handle` lets the
    /// derived policy continue to name the same logical namespace after the
    /// root's directory entry is concurrently replaced.
    pub fn derive_confined_directory(&self, candidate: &Path) -> Result<Self, LocalTargetError> {
        #[cfg(target_os = "linux")]
        {
            let relative = candidate
                .strip_prefix(&self.root)
                .map_err(|_| LocalTargetError::OutsideRoot(candidate.to_owned()))?;
            let logical_root = self.root.join(relative);
            let root_handle = self.open_directory_no_symlinks(&logical_root)?;
            Ok(Self {
                root: logical_root,
                root_handle: Arc::new(root_handle),
            })
        }
        #[cfg(not(target_os = "linux"))]
        {
            candidate
                .strip_prefix(&self.root)
                .map_err(|_| LocalTargetError::OutsideRoot(candidate.to_owned()))?;
            reject_symlink_components(&self.root, candidate)?;
            let canonical = candidate
                .canonicalize()
                .map_err(|source| classify_io(candidate.to_owned(), source))?;
            if !canonical.starts_with(&self.root) {
                return Err(LocalTargetError::OutsideRoot(canonical));
            }
            if !canonical.is_dir() {
                return Err(LocalTargetError::NotDirectory(canonical));
            }
            Ok(Self { root: canonical })
        }
    }

    pub fn inspect(&self, base: &Path, target: &str) -> Result<PathBuf, LocalTargetError> {
        let candidate = self.candidate(base, target)?;
        self.inspect_candidate(&candidate)
    }

    /// Resolves URL path syntax and parent components without touching the target.
    ///
    /// Callers may use the returned normalized path as a per-run cache key.
    pub fn candidate(&self, base: &Path, target: &str) -> Result<PathBuf, LocalTargetError> {
        let base = self.inspect_directory_no_symlinks(base)?;
        self.candidate_from_verified_base(&base, target)
    }

    /// Normalizes an absolute logical path without consulting the current
    /// process namespace.
    ///
    /// Parent components may move within this policy's root but never above
    /// it. The result is suitable for handle-relative inspection, including
    /// paths whose final component does not exist yet.
    pub fn normalize_candidate(&self, candidate: &Path) -> Result<PathBuf, LocalTargetError> {
        let relative = candidate
            .strip_prefix(&self.root)
            .map_err(|_| LocalTargetError::OutsideRoot(candidate.to_owned()))?;
        let mut normalized = self.root.clone();
        let mut depth = 0_usize;
        for component in relative.components() {
            match component {
                Component::Normal(name) => {
                    normalized.push(name);
                    depth = depth.saturating_add(1);
                }
                Component::CurDir => {}
                Component::ParentDir if depth > 0 => {
                    normalized.pop();
                    depth -= 1;
                }
                Component::ParentDir => {
                    return Err(LocalTargetError::OutsideRoot(candidate.to_owned()));
                }
                Component::Prefix(_) | Component::RootDir => {
                    return Err(LocalTargetError::Unverifiable(
                        candidate.to_string_lossy().into_owned(),
                    ));
                }
            }
        }
        Ok(normalized)
    }

    fn candidate_from_verified_base(
        &self,
        base: &Path,
        target: &str,
    ) -> Result<PathBuf, LocalTargetError> {
        if !base.starts_with(&self.root) {
            return Err(LocalTargetError::OutsideRoot(base.to_owned()));
        }
        let relative = decode_relative_path(target)?;
        normalize_below_root(&self.root, base, &relative)
    }

    #[cfg(target_os = "linux")]
    pub fn inspect_candidate(&self, candidate: &Path) -> Result<PathBuf, LocalTargetError> {
        if !candidate.starts_with(&self.root) {
            return Err(LocalTargetError::OutsideRoot(candidate.to_owned()));
        }
        Ok(self.open_confined(candidate)?.canonical_path)
    }

    /// Resolves a normalized regular file while rejecting every symbolic link.
    pub fn inspect_candidate_no_symlinks(
        &self,
        candidate: &Path,
    ) -> Result<PathBuf, LocalTargetError> {
        #[cfg(target_os = "linux")]
        {
            if !candidate.starts_with(&self.root) {
                return Err(LocalTargetError::OutsideRoot(candidate.to_owned()));
            }
            self.open_confined_with_symlinks(candidate, false)
                .map(|opened| opened.canonical_path)
        }
        #[cfg(not(target_os = "linux"))]
        {
            reject_symlink_components(&self.root, candidate)?;
            self.inspect_candidate(candidate)
        }
    }

    /// Resolves an existing directory without crossing a symbolic link.
    ///
    /// On handle-relative platforms the returned path remains in the logical
    /// namespace established when this policy was created.
    pub fn inspect_directory_no_symlinks(
        &self,
        candidate: &Path,
    ) -> Result<PathBuf, LocalTargetError> {
        #[cfg(target_os = "linux")]
        {
            let relative = candidate
                .strip_prefix(&self.root)
                .map_err(|_| LocalTargetError::OutsideRoot(candidate.to_owned()))?;
            self.open_directory_no_symlinks(candidate)?;
            Ok(self.root.join(relative))
        }
        #[cfg(not(target_os = "linux"))]
        {
            reject_symlink_components(&self.root, candidate)?;
            let canonical = candidate
                .canonicalize()
                .map_err(|source| classify_io(candidate.to_owned(), source))?;
            if !canonical.starts_with(&self.root) {
                return Err(LocalTargetError::OutsideRoot(canonical));
            }
            if !canonical.is_dir() {
                return Err(LocalTargetError::NotDirectory(canonical));
            }
            Ok(canonical)
        }
    }

    #[cfg(not(target_os = "linux"))]
    pub fn inspect_candidate(&self, candidate: &Path) -> Result<PathBuf, LocalTargetError> {
        if !candidate.starts_with(&self.root) {
            return Err(LocalTargetError::OutsideRoot(candidate.to_owned()));
        }
        let canonical = match candidate.canonicalize() {
            Ok(path) => path,
            Err(source) if source.kind() == std::io::ErrorKind::NotFound => {
                reject_dangling_symlink_escape(&self.root, candidate)?;
                ensure_existing_ancestor_is_inside(&self.root, candidate)?;
                return Err(LocalTargetError::Missing(candidate.to_owned()));
            }
            Err(source) => return Err(classify_io(candidate.to_owned(), source)),
        };
        if !canonical.starts_with(&self.root) {
            return Err(LocalTargetError::OutsideRoot(canonical));
        }
        let metadata =
            fs::metadata(&canonical).map_err(|source| classify_io(canonical.clone(), source))?;
        if !metadata.is_file() {
            return Err(LocalTargetError::NotFile(canonical));
        }
        Ok(canonical)
    }

    #[cfg(target_os = "linux")]
    fn open_confined(&self, candidate: &Path) -> Result<OpenedTarget, LocalTargetError> {
        self.open_confined_with_symlinks(candidate, true)
    }

    #[cfg(target_os = "linux")]
    fn open_confined_with_symlinks(
        &self,
        candidate: &Path,
        follow_symlinks: bool,
    ) -> Result<OpenedTarget, LocalTargetError> {
        self.open_confined_with_symlinks_after_open(candidate, follow_symlinks, || {})
    }

    #[cfg(target_os = "linux")]
    fn open_confined_with_symlinks_after_open(
        &self,
        candidate: &Path,
        follow_symlinks: bool,
        after_open: impl FnOnce(),
    ) -> Result<OpenedTarget, LocalTargetError> {
        use rustix::fd::OwnedFd;
        use rustix::fs::{Mode, OFlags, ResolveFlags, openat};

        let relative = candidate
            .strip_prefix(&self.root)
            .map_err(|_| LocalTargetError::OutsideRoot(candidate.to_owned()))?;
        let root = self.root_handle.as_ref();
        // Opening a FIFO for reading can otherwise wait for a writer before we
        // have a handle whose file type can be rejected.
        let flags = OFlags::RDONLY | OFlags::NONBLOCK | OFlags::CLOEXEC;
        // `RESOLVE_BENEATH` makes the kernel give up with `EAGAIN` when another
        // process renames or mounts something along this path while it is being
        // resolved. The lookup was neither denied nor granted, so the only
        // correct response is to look again. The attempt count is bounded so a
        // filesystem under constant churn fails instead of spinning.
        let mut attempts = 0;
        let file = loop {
            let mut resolve = ResolveFlags::BENEATH | ResolveFlags::NO_MAGICLINKS;
            if !follow_symlinks {
                resolve |= ResolveFlags::NO_SYMLINKS;
            }
            let outcome = confined_openat2(root, relative, flags, resolve);
            attempts += 1;
            if matches!(outcome, Err(rustix::io::Errno::AGAIN)) && attempts < CONFINED_OPEN_ATTEMPTS
            {
                continue;
            }
            break outcome;
        };
        let file = match file {
            Ok(file) => fs::File::from(file),
            Err(error)
                if error == rustix::io::Errno::NOSYS || error == rustix::io::Errno::INVAL =>
            {
                let mut directory: OwnedFd = openat(
                    root,
                    ".",
                    OFlags::PATH | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                    Mode::empty(),
                )
                .map_err(|error| classify_errno(candidate, error))?;
                let mut components = relative.components().peekable();
                while let Some(component) = components.next() {
                    let Component::Normal(name) = component else {
                        return Err(LocalTargetError::Unverifiable(
                            candidate.to_string_lossy().into_owned(),
                        ));
                    };
                    let component_flags = if components.peek().is_some() {
                        OFlags::PATH | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC
                    } else {
                        flags | OFlags::NOFOLLOW
                    };
                    directory = openat(&directory, name, component_flags, Mode::empty())
                        .map_err(|error| classify_errno(candidate, error))?;
                }
                fs::File::from(directory)
            }
            Err(error) => return Err(classify_errno(candidate, error)),
        };
        if !file
            .metadata()
            .map_err(|source| classify_io(candidate.to_owned(), source))?
            .is_file()
        {
            return Err(LocalTargetError::NotFile(candidate.to_owned()));
        }
        after_open();
        let canonical_path = logical_path_from_opened_handle(
            &self.root,
            self.root_handle.as_ref(),
            &file,
            candidate,
        )?;
        Ok(OpenedTarget {
            canonical_path,
            file,
        })
    }

    #[cfg(not(target_os = "linux"))]
    fn open_confined(&self, candidate: &Path) -> Result<fs::File, LocalTargetError> {
        let canonical = self.inspect_candidate(candidate)?;
        fs::File::open(&canonical).map_err(|source| classify_io(canonical, source))
    }
}

#[cfg(target_os = "linux")]
fn explicit_target_changed_error() -> LocalTargetError {
    LocalTargetError::Unverifiable(
        "explicit local target changed while its authority was established".to_owned(),
    )
}

#[cfg(target_os = "linux")]
fn procfs_reports_deleted_path(path: &Path) -> bool {
    use std::os::unix::ffi::OsStrExt;

    path.as_os_str().as_bytes().ends_with(b" (deleted)")
}

#[cfg(target_os = "linux")]
fn logical_path_from_opened_handle(
    logical_root: &Path,
    root_handle: &fs::File,
    opened: &fs::File,
    candidate: &Path,
) -> Result<PathBuf, LocalTargetError> {
    logical_path_from_opened_handle_with(logical_root, root_handle, opened, candidate, || {}, || {})
}

#[cfg(target_os = "linux")]
fn logical_path_from_opened_handle_with(
    logical_root: &Path,
    root_handle: &fs::File,
    opened: &fs::File,
    candidate: &Path,
    after_root_path: impl FnOnce(),
    before_identity_open: impl FnOnce(),
) -> Result<PathBuf, LocalTargetError> {
    let root_path = path_from_fd(root_handle, candidate)?;
    after_root_path();
    let opened_path = path_from_fd(opened, candidate)?;
    let relative = opened_path
        .strip_prefix(&root_path)
        .map_err(|_| LocalTargetError::Unverifiable(candidate.to_string_lossy().into_owned()))?;
    before_identity_open();
    verify_opened_path_identity(root_handle, opened, relative, candidate)?;
    Ok(logical_root.join(relative))
}

#[cfg(target_os = "linux")]
fn verify_opened_path_identity(
    root_handle: &fs::File,
    opened: &fs::File,
    relative: &Path,
    candidate: &Path,
) -> Result<(), LocalTargetError> {
    use std::os::unix::fs::MetadataExt;

    use rustix::fd::OwnedFd;
    use rustix::fs::{Mode, OFlags, ResolveFlags, openat};

    let flags = OFlags::PATH | OFlags::CLOEXEC;
    let mut attempts = 0;
    let verified = loop {
        let outcome = confined_openat2(
            root_handle,
            relative,
            flags,
            ResolveFlags::BENEATH | ResolveFlags::NO_MAGICLINKS,
        );
        attempts += 1;
        if matches!(outcome, Err(rustix::io::Errno::AGAIN)) && attempts < CONFINED_OPEN_ATTEMPTS {
            continue;
        }
        break outcome;
    };
    let verified = match verified {
        Ok(file) => fs::File::from(file),
        Err(error) if error == rustix::io::Errno::NOSYS || error == rustix::io::Errno::INVAL => {
            let mut directory: OwnedFd = openat(
                root_handle,
                ".",
                OFlags::PATH | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                Mode::empty(),
            )
            .map_err(|error| opened_identity_error(candidate, error))?;
            let mut components = relative.components().peekable();
            while let Some(component) = components.next() {
                let Component::Normal(name) = component else {
                    return Err(opened_identity_mismatch(candidate));
                };
                let component_flags = if components.peek().is_some() {
                    OFlags::PATH | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC
                } else {
                    flags | OFlags::NOFOLLOW
                };
                directory = openat(&directory, name, component_flags, Mode::empty())
                    .map_err(|error| opened_identity_error(candidate, error))?;
            }
            fs::File::from(directory)
        }
        Err(error) => return Err(opened_identity_error(candidate, error)),
    };
    let opened_metadata = opened
        .metadata()
        .map_err(|error| opened_identity_io_error(candidate, error))?;
    let verified_metadata = verified
        .metadata()
        .map_err(|error| opened_identity_io_error(candidate, error))?;
    if opened_metadata.dev() != verified_metadata.dev()
        || opened_metadata.ino() != verified_metadata.ino()
        || opened_metadata.file_type() != verified_metadata.file_type()
    {
        return Err(opened_identity_mismatch(candidate));
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn opened_identity_error(candidate: &Path, source: rustix::io::Errno) -> LocalTargetError {
    LocalTargetError::Unverifiable(format!(
        "cannot verify the opened local target path for {}: {source}",
        candidate.display()
    ))
}

#[cfg(target_os = "linux")]
fn opened_identity_io_error(candidate: &Path, source: std::io::Error) -> LocalTargetError {
    LocalTargetError::Unverifiable(format!(
        "cannot verify the opened local target identity for {}: {source}",
        candidate.display()
    ))
}

#[cfg(target_os = "linux")]
fn opened_identity_mismatch(candidate: &Path) -> LocalTargetError {
    LocalTargetError::Unverifiable(format!(
        "opened local target no longer matches its filesystem path: {}",
        candidate.display()
    ))
}

#[cfg(target_os = "linux")]
fn path_from_file_handle(file: &fs::File, candidate: &Path) -> Result<PathBuf, LocalTargetError> {
    path_from_fd(file, candidate)
}

#[cfg(target_os = "linux")]
fn path_from_fd(file: &fs::File, candidate: &Path) -> Result<PathBuf, LocalTargetError> {
    use std::os::fd::AsRawFd;

    #[cfg(test)]
    if let Some(kind) = FORCED_FD_PATH_ERROR.with(std::cell::Cell::get) {
        return Err(fd_path_error(candidate, std::io::Error::from(kind)));
    }

    fs::read_link(format!("/proc/self/fd/{}", file.as_raw_fd()))
        .map_err(|source| fd_path_error(candidate, source))
}

#[cfg(target_os = "linux")]
fn fd_path_error(candidate: &Path, source: std::io::Error) -> LocalTargetError {
    LocalTargetError::Unverifiable(format!(
        "cannot resolve the opened local target through /proc/self/fd for {}: {source}",
        candidate.display()
    ))
}

fn read_bounded_utf8(
    file: fs::File,
    canonical_path: PathBuf,
    max_bytes: u64,
) -> Result<LoadedLocalTarget, LocalTargetError> {
    let mut bytes = Vec::new();
    file.take(max_bytes.saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(|source| classify_io(canonical_path.clone(), source))?;
    if bytes.len() as u64 > max_bytes {
        return Err(LocalTargetError::ResourceTooLarge(canonical_path));
    }
    let source = String::from_utf8(bytes)
        .map_err(|_| LocalTargetError::InvalidUtf8(canonical_path.clone()))?;
    Ok(LoadedLocalTarget {
        canonical_path,
        source: Arc::from(source),
    })
}

#[cfg(target_os = "linux")]
fn read_for_comparison(
    path: &Path,
    expected_len: usize,
    logical_path: &Path,
) -> Result<Vec<u8>, LocalTargetError> {
    use rustix::fs::{Mode, OFlags, open};

    let file = open(
        path,
        OFlags::RDONLY | OFlags::NONBLOCK | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map(fs::File::from)
    .map_err(|error| classify_errno(logical_path, error))?;
    if !file
        .metadata()
        .map_err(|source| classify_io(logical_path.to_owned(), source))?
        .is_file()
    {
        return Err(LocalTargetError::NotFile(logical_path.to_owned()));
    }
    let limit = u64::try_from(expected_len)
        .unwrap_or(u64::MAX)
        .saturating_add(1);
    let mut current = Vec::with_capacity(expected_len.saturating_add(1));
    file.take(limit)
        .read_to_end(&mut current)
        .map_err(|source| classify_io(logical_path.to_owned(), source))?;
    Ok(current)
}

#[cfg(target_os = "linux")]
fn open_root_directory(root: &Path) -> Result<(PathBuf, fs::File), LocalTargetError> {
    use rustix::fs::{Mode, OFlags, open};

    let file = open(
        root,
        OFlags::PATH | OFlags::DIRECTORY | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map(fs::File::from)
    .map_err(|error| {
        if error == rustix::io::Errno::NOTDIR {
            LocalTargetError::NotDirectory(root.to_owned())
        } else {
            classify_errno(root, error)
        }
    })?;
    let canonical = path_from_fd(&file, root)?;
    Ok((canonical, file))
}

/// Per-command cache and unique-path budget shared by validation and readers.
#[derive(Clone, Debug)]
pub struct LocalTargetSession {
    policy: LocalTargetPolicy,
    max_paths: usize,
    limits: FilesystemReadLimits,
    requests: usize,
    read_files: usize,
    read_bytes: u64,
    bases: BTreeMap<PathBuf, Result<PathBuf, LocalTargetError>>,
    inspections: BTreeMap<PathBuf, Result<PathBuf, LocalTargetError>>,
    text: BTreeMap<PathBuf, Result<Arc<str>, LocalTargetError>>,
}

#[derive(Clone, Debug)]
pub(crate) struct LocalTargetTextRollback {
    canonical_path: PathBuf,
    previous: Option<Result<Arc<str>, LocalTargetError>>,
}

#[derive(Clone, Debug)]
pub(crate) struct LocalTargetCandidateRollback {
    candidate: PathBuf,
    previous: Option<Result<PathBuf, LocalTargetError>>,
}

/// UTF-8 local target returned by a bounded validation session.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LoadedLocalTarget {
    canonical_path: PathBuf,
    source: Arc<str>,
}

/// Bounded local file bytes paired with their stable logical path.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LoadedLocalBytes {
    canonical_path: PathBuf,
    source: Vec<u8>,
}

impl LoadedLocalTarget {
    pub fn canonical_path(&self) -> &Path {
        &self.canonical_path
    }

    pub fn source(&self) -> &str {
        &self.source
    }

    pub fn into_parts(self) -> (PathBuf, Arc<str>) {
        (self.canonical_path, self.source)
    }
}

impl LoadedLocalBytes {
    /// Returns the stable logical path used for this read.
    pub fn canonical_path(&self) -> &Path {
        &self.canonical_path
    }

    /// Returns the bytes captured from the opened file.
    pub fn source(&self) -> &[u8] {
        &self.source
    }

    /// Splits the loaded value into its logical path and bytes.
    pub fn into_parts(self) -> (PathBuf, Vec<u8>) {
        (self.canonical_path, self.source)
    }
}

impl LocalTargetSession {
    pub fn new(policy: LocalTargetPolicy, max_paths: usize, limits: FilesystemReadLimits) -> Self {
        Self {
            policy,
            max_paths,
            limits,
            requests: 0,
            read_files: 0,
            read_bytes: 0,
            bases: BTreeMap::new(),
            inspections: BTreeMap::new(),
            text: BTreeMap::new(),
        }
    }

    pub fn policy(&self) -> &LocalTargetPolicy {
        &self.policy
    }

    pub fn inspect(&mut self, base: &Path, target: &str) -> Result<PathBuf, LocalTargetError> {
        let candidate = self.candidate(base, target)?;
        if let Some(result) = self.inspections.get(&candidate) {
            return result.clone();
        }
        self.charge_path_request(&candidate)?;
        let result = self.policy.inspect_candidate(&candidate);
        self.inspections.insert(candidate, result.clone());
        result
    }

    /// Records a failed examination so a repeated reference costs nothing.
    ///
    /// The bound counts distinct paths and `charge_path_request` skips a path
    /// already recorded, but only the successful branches recorded. Reading the
    /// same missing path twice therefore charged twice and could exhaust the
    /// bound before the first path that exists. `inspect` already records its
    /// failures, so the two entry points disagreed on what a repeated reference
    /// costs.
    ///
    /// The charge is kept rather than refunded: a path that does not exist
    /// still costs the work of looking, and refunding it would let a document
    /// name unlimited missing paths for free.
    fn remember<T>(
        &mut self,
        candidate: &Path,
        result: Result<T, LocalTargetError>,
    ) -> Result<T, LocalTargetError> {
        if let Err(error) = &result {
            self.inspections
                .insert(candidate.to_owned(), Err(error.clone()));
        }
        result
    }

    /// Counts one path against the number this session may examine.
    ///
    /// A path already examined costs nothing, so repeated references to the
    /// same target do not exhaust the bound. The bound itself applies on every
    /// platform: it limits how much work an authored document can ask for, which
    /// does not depend on how the filesystem resolves a path.
    fn charge_path_request(&mut self, candidate: &Path) -> Result<(), LocalTargetError> {
        if self.inspections.contains_key(candidate) {
            return Ok(());
        }
        if self.requests >= self.max_paths {
            return Err(LocalTargetError::LimitExceeded {
                limit: self.max_paths,
            });
        }
        self.requests += 1;
        Ok(())
    }

    pub(crate) fn candidate(
        &mut self,
        base: &Path,
        target: &str,
    ) -> Result<PathBuf, LocalTargetError> {
        let canonical_base = if let Some(result) = self.bases.get(base) {
            result.clone()?
        } else {
            let result = self.policy.inspect_directory_no_symlinks(base);
            self.bases.insert(base.to_owned(), result.clone());
            result?
        };
        self.policy
            .candidate_from_verified_base(&canonical_base, target)
    }

    pub fn read_utf8(
        &mut self,
        base: &Path,
        target: &str,
    ) -> Result<LoadedLocalTarget, LocalTargetError> {
        let candidate = self.candidate(base, target)?;
        self.read_candidate_utf8(&candidate)
    }

    /// Opens and reads an already normalized path below this session's root.
    ///
    /// The path is resolved from the root handle on platforms which advertise
    /// [`FilesystemRaceResistance::HandleRelative`].
    pub fn read_candidate_utf8(
        &mut self,
        candidate: &Path,
    ) -> Result<LoadedLocalTarget, LocalTargetError> {
        let capacity = self.default_read_capacity();
        self.read_candidate_utf8_with_capacity(candidate, true, true, || {}, |_| capacity)
    }

    /// Opens and reads a normalized path while rejecting every symbolic link.
    ///
    /// This is intended for files which define policy, where even a symbolic
    /// link that remains below the root would make the selected authority
    /// ambiguous.
    pub fn read_candidate_utf8_no_symlinks(
        &mut self,
        candidate: &Path,
    ) -> Result<LoadedLocalTarget, LocalTargetError> {
        let capacity = self.default_read_capacity();
        self.read_candidate_utf8_with_capacity(candidate, true, false, || {}, |_| capacity)
    }

    /// Opens and reads bounded bytes from an already normalized path.
    pub fn read_candidate_bytes(
        &mut self,
        candidate: &Path,
    ) -> Result<LoadedLocalBytes, LocalTargetError> {
        let capacity = self.default_read_capacity();
        self.read_candidate_bytes_with_capacity(candidate, capacity, true, || {})
    }

    /// Opens and reads bounded bytes while rejecting every symbolic link.
    pub fn read_candidate_bytes_no_symlinks(
        &mut self,
        candidate: &Path,
    ) -> Result<LoadedLocalBytes, LocalTargetError> {
        let capacity = self.default_read_capacity();
        self.read_candidate_bytes_with_capacity(candidate, capacity, false, || {})
    }

    #[cfg(test)]
    fn read_candidate_bytes_after_open(
        &mut self,
        candidate: &Path,
        after_open: impl FnOnce(),
    ) -> Result<LoadedLocalBytes, LocalTargetError> {
        let capacity = self.default_read_capacity();
        self.read_candidate_bytes_with_capacity(candidate, capacity, true, after_open)
    }

    fn read_candidate_bytes_with_capacity(
        &mut self,
        candidate: &Path,
        capacity: CandidateReadCapacity,
        follow_symlinks: bool,
        after_open: impl FnOnce(),
    ) -> Result<LoadedLocalBytes, LocalTargetError> {
        let (canonical, file) = self.open_candidate(candidate, follow_symlinks, after_open)?;
        if !capacity.allow_file {
            return Err(LocalTargetError::ReadLimitExceeded);
        }
        let read_limit = capacity
            .max_resource_bytes
            .min(capacity.max_total_bytes)
            .saturating_add(1);
        self.read_files += 1;
        let mut bytes = Vec::new();
        file.take(read_limit)
            .read_to_end(&mut bytes)
            .map_err(|source| classify_io(canonical.clone(), source))?;
        self.read_bytes = self.read_bytes.saturating_add(bytes.len() as u64);
        if bytes.len() as u64 > capacity.max_total_bytes {
            return Err(LocalTargetError::ReadLimitExceeded);
        }
        if bytes.len() as u64 > capacity.max_resource_bytes {
            return Err(LocalTargetError::ResourceTooLarge(canonical));
        }
        Ok(LoadedLocalBytes {
            canonical_path: canonical,
            source: bytes,
        })
    }

    /// Reopens an already normalized path without reusing cached text.
    pub(crate) fn reread_candidate_utf8_with_capacity(
        &mut self,
        candidate: &Path,
        capacity: impl FnOnce(&Path) -> CandidateReadCapacity,
    ) -> Result<(LoadedLocalTarget, LocalTargetTextRollback), LocalTargetError> {
        let loaded =
            self.read_candidate_utf8_with_capacity(candidate, false, true, || {}, capacity)?;
        let canonical_path = loaded.canonical_path.clone();
        let previous = self
            .text
            .insert(canonical_path.clone(), Ok(loaded.source.clone()));
        Ok((
            loaded,
            LocalTargetTextRollback {
                canonical_path,
                previous,
            },
        ))
    }

    pub(crate) fn rollback_cached_text(&mut self, rollback: LocalTargetTextRollback) {
        match rollback.previous {
            Some(previous) => {
                self.text.insert(rollback.canonical_path, previous);
            }
            None => {
                self.text.remove(&rollback.canonical_path);
            }
        }
    }

    #[cfg(test)]
    pub(crate) fn read_utf8_after_open(
        &mut self,
        base: &Path,
        target: &str,
        after_open: impl FnOnce(),
    ) -> Result<LoadedLocalTarget, LocalTargetError> {
        let candidate = self.candidate(base, target)?;
        let capacity = self.default_read_capacity();
        self.read_candidate_utf8_with_capacity(&candidate, true, true, after_open, |_| capacity)
    }

    fn default_read_capacity(&self) -> CandidateReadCapacity {
        CandidateReadCapacity {
            allow_file: self.read_files < self.limits.max_files
                && self.read_bytes < self.limits.max_total_bytes,
            max_total_bytes: self.limits.max_total_bytes.saturating_sub(self.read_bytes),
            max_resource_bytes: self.limits.max_resource_bytes,
        }
    }

    pub(crate) fn read_candidate_utf8_with_capacity(
        &mut self,
        candidate: &Path,
        reuse_cached_text: bool,
        follow_symlinks: bool,
        after_open: impl FnOnce(),
        capacity: impl FnOnce(&Path) -> CandidateReadCapacity,
    ) -> Result<LoadedLocalTarget, LocalTargetError> {
        let (canonical, file) = self.open_candidate(candidate, follow_symlinks, after_open)?;
        if reuse_cached_text && let Some(result) = self.text.get(&canonical) {
            return result.clone().map(|source| LoadedLocalTarget {
                canonical_path: canonical,
                source,
            });
        }
        let capacity = capacity(&canonical);
        if !capacity.allow_file {
            return Err(LocalTargetError::ReadLimitExceeded);
        }
        let read_limit = capacity
            .max_resource_bytes
            .min(capacity.max_total_bytes)
            .saturating_add(1);
        let result = (|| {
            self.read_files += 1;
            let mut bytes = Vec::new();
            file.take(read_limit)
                .read_to_end(&mut bytes)
                .map_err(|source| classify_io(canonical.clone(), source))?;
            self.read_bytes = self.read_bytes.saturating_add(bytes.len() as u64);
            if bytes.len() as u64 > capacity.max_total_bytes {
                return Err(LocalTargetError::ReadLimitExceeded);
            }
            if bytes.len() as u64 > capacity.max_resource_bytes {
                return Err(LocalTargetError::ResourceTooLarge(canonical.clone()));
            }
            String::from_utf8(bytes)
                .map(Arc::<str>::from)
                .map_err(|_| LocalTargetError::InvalidUtf8(canonical.clone()))
        })();
        if reuse_cached_text {
            self.text.insert(canonical.clone(), result.clone());
        }
        result.map(|source| LoadedLocalTarget {
            canonical_path: canonical,
            source,
        })
    }

    fn open_candidate(
        &mut self,
        candidate: &Path,
        follow_symlinks: bool,
        after_open: impl FnOnce(),
    ) -> Result<(PathBuf, fs::File), LocalTargetError> {
        // The number of distinct paths a session may examine is a resource
        // bound on every platform. Only resolution and opening differ below.
        self.charge_path_request(candidate)?;
        #[cfg(target_os = "linux")]
        {
            let opened = self.remember(
                candidate,
                self.policy.open_confined_with_symlinks_after_open(
                    candidate,
                    follow_symlinks,
                    after_open,
                ),
            )?;
            self.inspections
                .insert(candidate.to_owned(), Ok(opened.canonical_path.clone()));
            Ok((opened.canonical_path, opened.file))
        }
        #[cfg(not(target_os = "linux"))]
        {
            if !follow_symlinks {
                reject_symlink_components(&self.policy.root, candidate)?;
            }
            let canonical = self.remember(candidate, self.policy.inspect_candidate(candidate))?;
            self.inspections
                .insert(candidate.to_owned(), Ok(canonical.clone()));
            let file = self.policy.open_confined(&canonical)?;
            after_open();
            Ok((canonical, file))
        }
    }

    pub fn inspected_paths(&self) -> usize {
        self.inspections.len()
    }

    pub(crate) fn candidate_rollback(&self, candidate: &Path) -> LocalTargetCandidateRollback {
        LocalTargetCandidateRollback {
            candidate: candidate.to_owned(),
            previous: self.inspections.get(candidate).cloned(),
        }
    }

    pub(crate) fn rollback_candidate(&mut self, rollback: LocalTargetCandidateRollback) {
        let current = self.inspections.remove(&rollback.candidate);
        match (current.is_some(), rollback.previous) {
            (true, Some(previous)) => {
                self.inspections.insert(rollback.candidate, previous);
            }
            (true, None) => {
                self.requests = self.requests.saturating_sub(1);
            }
            (false, Some(previous)) => {
                self.requests = self.requests.saturating_add(1);
                self.inspections.insert(rollback.candidate, previous);
            }
            (false, None) => {}
        }
    }

    pub(crate) fn release_candidate(&mut self, candidate: &Path) {
        if let Some(result) = self.inspections.remove(candidate) {
            self.requests = self.requests.saturating_sub(1);
            if let Ok(canonical) = result
                && !self
                    .inspections
                    .values()
                    .any(|result| result.as_ref() == Ok(&canonical))
            {
                self.text.remove(&canonical);
            }
        }
    }

    pub fn read_files(&self) -> usize {
        self.read_files
    }
}

#[cfg(not(target_os = "linux"))]
fn reject_symlink_components(root: &Path, candidate: &Path) -> Result<(), LocalTargetError> {
    let relative = candidate
        .strip_prefix(root)
        .map_err(|_| LocalTargetError::OutsideRoot(candidate.to_owned()))?;
    let mut current = root.to_owned();
    for component in relative.components() {
        let Component::Normal(name) = component else {
            return Err(LocalTargetError::Unverifiable(
                candidate.to_string_lossy().into_owned(),
            ));
        };
        current.push(name);
        match fs::symlink_metadata(&current) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err(LocalTargetError::OutsideRoot(current));
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => break,
            Err(source) => return Err(classify_io(current, source)),
        }
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn classify_errno(path: &Path, source: rustix::io::Errno) -> LocalTargetError {
    if source == rustix::io::Errno::XDEV {
        return LocalTargetError::OutsideRoot(path.to_owned());
    }
    if source == rustix::io::Errno::NOTDIR {
        return LocalTargetError::NotDirectory(path.to_owned());
    }
    classify_io(
        path.to_owned(),
        std::io::Error::from_raw_os_error(source.raw_os_error()),
    )
}

fn decode_relative_path(target: &str) -> Result<PathBuf, LocalTargetError> {
    if target.is_empty()
        || target.starts_with(['/', '\\'])
        || target.contains('\\')
        || target.contains(':')
        || target.chars().any(char::is_control)
    {
        return Err(LocalTargetError::Unverifiable(target.to_owned()));
    }
    let bytes = target.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] != b'%' {
            decoded.push(bytes[index]);
            index += 1;
            continue;
        }
        if index + 2 >= bytes.len() {
            return Err(LocalTargetError::Unverifiable(target.to_owned()));
        }
        let (Some(high), Some(low)) = (hex(bytes[index + 1]), hex(bytes[index + 2])) else {
            return Err(LocalTargetError::Unverifiable(target.to_owned()));
        };
        decoded.push(high * 16 + low);
        index += 3;
    }
    let decoded = String::from_utf8(decoded)
        .map_err(|_| LocalTargetError::Unverifiable(target.to_owned()))?;
    if decoded.contains(':') || decoded.contains('\\') || decoded.chars().any(char::is_control) {
        return Err(LocalTargetError::Unverifiable(target.to_owned()));
    }
    Ok(PathBuf::from(decoded))
}

fn normalize_below_root(
    root: &Path,
    base: &Path,
    relative: &Path,
) -> Result<PathBuf, LocalTargetError> {
    let mut candidate = base.to_owned();
    for component in relative.components() {
        match component {
            Component::Normal(value) => candidate.push(value),
            Component::CurDir => {}
            Component::ParentDir => {
                if candidate == root || !candidate.pop() || !candidate.starts_with(root) {
                    return Err(LocalTargetError::OutsideRoot(candidate));
                }
            }
            Component::RootDir | Component::Prefix(_) => {
                return Err(LocalTargetError::OutsideRoot(candidate));
            }
        }
    }
    if candidate == base && relative.as_os_str().is_empty() {
        return Err(LocalTargetError::Unverifiable(
            relative.to_string_lossy().into_owned(),
        ));
    }
    Ok(candidate)
}

#[cfg(not(target_os = "linux"))]
fn reject_dangling_symlink_escape(root: &Path, candidate: &Path) -> Result<(), LocalTargetError> {
    reject_dangling_symlink_escape_inner(root, candidate, &mut BTreeSet::new(), 0)
}

#[cfg(not(target_os = "linux"))]
fn reject_dangling_symlink_escape_inner(
    root: &Path,
    candidate: &Path,
    visited: &mut BTreeSet<PathBuf>,
    depth: usize,
) -> Result<(), LocalTargetError> {
    const MAX_SYMLINK_DEPTH: usize = 64;
    if depth > MAX_SYMLINK_DEPTH {
        return Err(LocalTargetError::Unverifiable(format!(
            "local target symlink depth exceeds {MAX_SYMLINK_DEPTH}: {}",
            candidate.display()
        )));
    }
    if !candidate.starts_with(root) {
        return Err(LocalTargetError::OutsideRoot(candidate.to_owned()));
    }
    let metadata = match fs::symlink_metadata(candidate) {
        Ok(metadata) => metadata,
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => {
            return candidate.parent().map_or(Ok(()), |parent| {
                reject_dangling_symlink_escape_inner(root, parent, visited, depth)
            });
        }
        Err(source) => return Err(classify_io(candidate.to_owned(), source)),
    };
    if !metadata.file_type().is_symlink() {
        return Ok(());
    }
    if !visited.insert(candidate.to_owned()) {
        return Err(LocalTargetError::Unverifiable(format!(
            "local target symlink cycle: {}",
            candidate.display()
        )));
    }
    let destination =
        fs::read_link(candidate).map_err(|source| classify_io(candidate.to_owned(), source))?;
    let resolved = if destination.is_absolute() {
        destination
    } else {
        candidate
            .parent()
            .unwrap_or_else(|| Path::new(""))
            .join(destination)
    };
    let normalized = normalize_absolute(&resolved);
    reject_dangling_symlink_escape_inner(root, &normalized, visited, depth + 1)
}

#[cfg(not(target_os = "linux"))]
fn normalize_absolute(path: &Path) -> PathBuf {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Prefix(prefix) => normalized.push(prefix.as_os_str()),
            Component::RootDir => normalized.push(component.as_os_str()),
            Component::CurDir => {}
            Component::ParentDir => {
                normalized.pop();
            }
            Component::Normal(value) => normalized.push(value),
        }
    }
    normalized
}

#[cfg(not(target_os = "linux"))]
fn ensure_existing_ancestor_is_inside(
    root: &Path,
    candidate: &Path,
) -> Result<(), LocalTargetError> {
    let mut ancestor = candidate.parent();
    while let Some(path) = ancestor {
        reject_dangling_symlink_escape(root, path)?;
        match path.canonicalize() {
            Ok(canonical) => {
                return if canonical.starts_with(root) {
                    Ok(())
                } else {
                    Err(LocalTargetError::OutsideRoot(canonical))
                };
            }
            Err(source) if source.kind() == std::io::ErrorKind::NotFound => {
                ancestor = path.parent();
            }
            Err(source) => return Err(classify_io(path.to_owned(), source)),
        }
    }
    Err(LocalTargetError::Unverifiable(
        candidate.to_string_lossy().into_owned(),
    ))
}

const fn hex(value: u8) -> Option<u8> {
    match value {
        b'0'..=b'9' => Some(value - b'0'),
        b'a'..=b'f' => Some(value - b'a' + 10),
        b'A'..=b'F' => Some(value - b'A' + 10),
        _ => None,
    }
}

fn classify_io(path: PathBuf, source: std::io::Error) -> LocalTargetError {
    match source.kind() {
        std::io::ErrorKind::NotFound => LocalTargetError::Missing(path),
        std::io::ErrorKind::PermissionDenied => LocalTargetError::PermissionDenied(path),
        _ => LocalTargetError::Unverifiable(format!("{}: {source}", path.display())),
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum LocalTargetError {
    Missing(PathBuf),
    OutsideRoot(PathBuf),
    NotFile(PathBuf),
    NotDirectory(PathBuf),
    PermissionDenied(PathBuf),
    InvalidUtf8(PathBuf),
    Unverifiable(String),
    LimitExceeded { limit: usize },
    ResourceTooLarge(PathBuf),
    ReadLimitExceeded,
}

impl LocalTargetError {
    pub const fn diagnostic_code(&self) -> &'static str {
        match self {
            Self::Missing(_) => "local-target-missing",
            Self::OutsideRoot(_) => "local-target-outside-root",
            Self::NotFile(_) | Self::NotDirectory(_) => "local-target-not-file",
            Self::PermissionDenied(_) => "local-target-permission-denied",
            Self::InvalidUtf8(_) | Self::Unverifiable(_) => "local-target-unverifiable",
            Self::LimitExceeded { .. } => "local-target-limit-exceeded",
            Self::ResourceTooLarge(_) | Self::ReadLimitExceeded => "local-target-unverifiable",
        }
    }
}

impl fmt::Display for LocalTargetError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Missing(path) => write!(formatter, "local target is missing: {}", path.display()),
            Self::OutsideRoot(path) => {
                write!(
                    formatter,
                    "local target is outside project root: {}",
                    path.display()
                )
            }
            Self::NotFile(path) => {
                write!(formatter, "local target is not a file: {}", path.display())
            }
            Self::NotDirectory(path) => {
                write!(
                    formatter,
                    "local target base is not a directory: {}",
                    path.display()
                )
            }
            Self::PermissionDenied(path) => {
                write!(
                    formatter,
                    "permission denied for local target: {}",
                    path.display()
                )
            }
            Self::InvalidUtf8(path) => {
                write!(
                    formatter,
                    "local target is not valid UTF-8: {}",
                    path.display()
                )
            }
            Self::Unverifiable(reason) => {
                write!(formatter, "local target cannot be verified: {reason}")
            }
            Self::LimitExceeded { limit } => {
                write!(formatter, "local target inspection limit exceeded: {limit}")
            }
            Self::ResourceTooLarge(path) => {
                write!(formatter, "local target is too large: {}", path.display())
            }
            Self::ReadLimitExceeded => formatter.write_str("local target read limit exceeded"),
        }
    }
}

impl Error for LocalTargetError {}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};

    struct TestDir(PathBuf);

    impl TestDir {
        fn new() -> Self {
            // The test harness runs these in parallel threads of one process, so
            // the process id is shared and a coarse clock can hand two callers the
            // same nonce. A colliding directory is removed by the first `Drop`
            // while the other test is still using it, so the counter is what keeps
            // the names distinct.
            static SEQUENCE: AtomicU64 = AtomicU64::new(0);

            let nonce = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("clock after epoch")
                .as_nanos();
            let sequence = SEQUENCE.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "adocweave-local-target-{}-{nonce}-{sequence}",
                std::process::id()
            ));
            fs::create_dir_all(path.join("docs/sub")).expect("create directories");
            fs::write(path.join("docs/guide.adoc"), "= Guide").expect("write file");
            Self(path)
        }
    }

    impl Drop for TestDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn parent_and_percent_encoded_paths_are_checked_below_root() {
        let root = TestDir::new();
        fs::write(root.0.join("docs/my guide.pdf"), b"pdf").expect("write encoded target");
        let policy = LocalTargetPolicy::new(&root.0).expect("policy");

        assert_eq!(
            policy
                .inspect(&root.0.join("docs/sub"), "../guide.adoc")
                .expect("parent path"),
            root.0.join("docs/guide.adoc")
        );
        assert_eq!(
            policy
                .inspect(&root.0.join("docs"), "my%20guide.pdf")
                .expect("encoded path"),
            root.0.join("docs/my guide.pdf")
        );
    }

    #[test]
    fn logical_candidate_normalization_does_not_consult_missing_paths() {
        let root = TestDir::new();
        let policy = LocalTargetPolicy::new(&root.0).expect("policy");

        assert_eq!(
            policy
                .normalize_candidate(&root.0.join("docs/missing/../guide.adoc"))
                .expect("logical path"),
            root.0.join("docs/guide.adoc")
        );
        assert!(matches!(
            policy.normalize_candidate(&root.0.join("../outside.adoc")),
            Err(LocalTargetError::OutsideRoot(_))
        ));
    }

    #[test]
    fn missing_directory_and_lexical_escape_have_stable_codes() {
        let root = TestDir::new();
        let policy = LocalTargetPolicy::new(&root.0).expect("policy");

        assert_eq!(
            policy
                .inspect(&root.0.join("docs"), "missing.adoc")
                .expect_err("missing")
                .diagnostic_code(),
            "local-target-missing"
        );
        assert_eq!(
            policy
                .inspect(&root.0.join("docs"), ".")
                .expect_err("directory")
                .diagnostic_code(),
            "local-target-not-file"
        );
        assert_eq!(
            policy
                .inspect(&root.0.join("docs"), "../../outside")
                .expect_err("outside")
                .diagnostic_code(),
            "local-target-outside-root"
        );
        for target in ["bad%0Aname", "stream%3Adata", "bad%5Cname"] {
            assert_eq!(
                policy
                    .inspect(&root.0.join("docs"), target)
                    .expect_err("encoded unsafe path")
                    .diagnostic_code(),
                "local-target-unverifiable"
            );
        }
    }

    #[test]
    fn bounded_byte_reads_accept_non_utf8_and_enforce_the_resource_limit() {
        let root = TestDir::new();
        let binary = root.0.join("style.css");
        fs::write(&binary, [0xff, 0x00]).expect("binary stylesheet");
        let policy = LocalTargetPolicy::new(&root.0).expect("policy");
        let mut accepted = LocalTargetSession::new(
            policy.clone(),
            1,
            FilesystemReadLimits {
                max_files: 1,
                max_total_bytes: 2,
                max_resource_bytes: 2,
            },
        );

        let loaded = accepted
            .read_candidate_bytes(&binary)
            .expect("bounded bytes");
        assert_eq!(loaded.source(), [0xff, 0x00]);

        let mut rejected = LocalTargetSession::new(
            policy,
            1,
            FilesystemReadLimits {
                max_files: 1,
                max_total_bytes: 1,
                max_resource_bytes: 1,
            },
        );
        assert!(matches!(
            rejected.read_candidate_bytes(&binary),
            Err(LocalTargetError::ReadLimitExceeded) | Err(LocalTargetError::ResourceTooLarge(_))
        ));
    }

    #[test]
    fn policy_constructor_reports_a_regular_file_as_not_a_directory() {
        let root = TestDir::new();
        let file = root.0.join("docs/guide.adoc");

        assert!(matches!(
            LocalTargetPolicy::new(&file),
            Err(LocalTargetError::NotDirectory(path)) if path == file
        ));
    }

    #[cfg(unix)]
    #[test]
    fn explicit_utf8_load_accepts_a_symbolic_link_and_retains_the_target_parent() {
        use std::os::unix::fs::symlink;

        let selected_parent = TestDir::new();
        let target_parent = TestDir::new();
        let target = target_parent.0.join("project.toml");
        let selected = selected_parent.0.join("selected.toml");
        fs::write(&target, "schema-version = 1\n").expect("target file");
        symlink(&target, &selected).expect("selected symlink");

        let (policy, loaded) =
            LocalTargetPolicy::load_explicit_utf8(&selected, 1024).expect("explicit file");

        assert_eq!(policy.root(), target_parent.0);
        assert_eq!(loaded.canonical_path(), target);
        assert_eq!(loaded.source(), "schema-version = 1\n");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn explicit_utf8_load_matches_bytes_to_the_retained_parent_authority() {
        use std::os::unix::fs::symlink;

        let parent = TestDir::new();
        let outside = TestDir::new();
        let selected = parent.0.join("project.toml");
        fs::write(&selected, "schema-version = 1\n").expect("trusted file");
        fs::write(outside.0.join("project.toml"), "schema-version = 99\n").expect("outside file");
        let displaced = parent.0.with_extension("explicit-authority");

        let (policy, loaded) = LocalTargetPolicy::load_explicit_utf8_with(&selected, 1024, || {
            fs::rename(&parent.0, &displaced).expect("displace selected parent");
            symlink(&outside.0, &parent.0).expect("replace selected parent");
        })
        .expect("stable explicit file");

        assert_eq!(policy.root(), displaced);
        assert_eq!(loaded.source(), "schema-version = 1\n");
        assert_ne!(loaded.source(), "schema-version = 99\n");
        fs::remove_file(&parent.0).expect("remove replacement symlink");
        fs::rename(&displaced, &parent.0).expect("restore selected parent");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn explicit_utf8_load_preserves_unverifiable_after_an_opened_file_is_unlinked() {
        let root = TestDir::new();
        let selected = root.0.join("docs/guide.adoc");

        let error = LocalTargetPolicy::load_explicit_utf8_with(&selected, 1024, || {
            fs::remove_file(&selected).expect("unlink selected file");
        })
        .expect_err("an unlinked explicit target cannot establish its authority");

        assert!(matches!(error, LocalTargetError::Unverifiable(_)));
        assert_eq!(error.diagnostic_code(), "local-target-unverifiable");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn explicit_utf8_load_rejects_a_literal_deleted_suffix_inode_collision() {
        let root = TestDir::new();
        let selected = root.0.join("docs/guide.adoc");
        let suffix = root.0.join("docs/guide.adoc (deleted)");
        fs::write(&suffix, "literal suffix").expect("suffix source");

        let error = LocalTargetPolicy::load_explicit_utf8_with(&selected, 1024, || {
            fs::remove_file(&selected).expect("unlink selected file");
        })
        .expect_err("the procfs suffix must not select another inode");

        assert!(matches!(error, LocalTargetError::Unverifiable(_)));
        assert_eq!(error.diagnostic_code(), "local-target-unverifiable");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn explicit_utf8_retry_preserves_permission_and_fd_limit_errors() {
        for error in [rustix::io::Errno::ACCESS, rustix::io::Errno::MFILE] {
            let root = TestDir::new();
            let selected = root.0.join("docs/guide.adoc");
            FORCED_EXPLICIT_RETRY_OPEN_ERROR.with(|forced| forced.set(Some(error)));
            let result = LocalTargetPolicy::load_explicit_utf8_with(&selected, 1024, || {
                fs::remove_file(&selected).expect("unlink selected file");
            });
            FORCED_EXPLICIT_RETRY_OPEN_ERROR.with(|forced| forced.set(None));

            match error {
                rustix::io::Errno::ACCESS => {
                    assert_eq!(result, Err(LocalTargetError::PermissionDenied(selected)));
                }
                rustix::io::Errno::MFILE => {
                    assert!(matches!(
                        result,
                        Err(LocalTargetError::Unverifiable(reason))
                            if reason.contains(&selected.to_string_lossy().into_owned())
                                && !reason.contains("authority was established")
                    ));
                }
                _ => unreachable!("test covers two explicit errors"),
            }
        }
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn explicit_utf8_retry_does_not_replace_not_file_with_a_saved_race() {
        let root = TestDir::new();
        let selected = root.0.join("docs/guide.adoc");

        let error = LocalTargetPolicy::load_explicit_utf8_with(&selected, 1024, || {
            fs::remove_file(&selected).expect("unlink selected file");
            fs::create_dir(&selected).expect("replace selected file with directory");
        })
        .expect_err("the retry must preserve the current file type error");

        assert_eq!(error, LocalTargetError::NotFile(selected));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn explicit_utf8_preserves_self_new_not_directory() {
        let root = TestDir::new();
        let parent = root.0.join("explicit-parent");
        fs::create_dir(&parent).expect("explicit parent");
        let selected = parent.join("guide.adoc");
        fs::write(&selected, "= Guide").expect("selected file");

        let error = LocalTargetPolicy::load_explicit_utf8_with(&selected, 1024, || {
            fs::remove_file(&selected).expect("unlink selected file");
            fs::remove_dir(&parent).expect("remove selected parent");
            fs::write(&parent, "not a directory").expect("replace parent with file");
        })
        .expect_err("policy construction must preserve not-directory");

        assert_eq!(error, LocalTargetError::NotDirectory(parent));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn explicit_utf8_reports_unverifiable_for_a_deleted_suffix_directory_collision() {
        let root = TestDir::new();
        let selected = root.0.join("docs/guide.adoc");
        let suffix = root.0.join("docs/guide.adoc (deleted)");

        let error = LocalTargetPolicy::load_explicit_utf8_with(&selected, 1024, || {
            fs::remove_file(&selected).expect("unlink selected file");
            fs::create_dir(&suffix).expect("create suffix directory");
        })
        .expect_err("a procfs deletion race must not be reported as an authored directory");

        assert!(
            matches!(error, LocalTargetError::Unverifiable(_)),
            "unexpected error: {error:?}"
        );
        assert_eq!(error.diagnostic_code(), "local-target-unverifiable");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn explicit_utf8_preserves_not_file_for_an_authored_directory() {
        let root = TestDir::new();
        let selected = root.0.join("docs");

        let error = LocalTargetPolicy::load_explicit_utf8(&selected, 1024)
            .expect_err("an explicitly selected directory is not a file");

        assert_eq!(error, LocalTargetError::NotFile(selected));
    }

    #[test]
    fn confined_directory_derivation_rejects_an_outside_directory() {
        let root = TestDir::new();
        let outside = TestDir::new();
        let policy = LocalTargetPolicy::new(&root.0).expect("policy");

        assert!(matches!(
            policy.derive_confined_directory(&outside.0),
            Err(LocalTargetError::OutsideRoot(_))
        ));
    }

    #[cfg(unix)]
    #[test]
    fn confined_directory_derivation_rejects_a_symbolic_link() {
        use std::os::unix::fs::symlink;

        let root = TestDir::new();
        let outside = TestDir::new();
        symlink(&outside.0, root.0.join("linked-directory")).expect("directory symlink");
        let policy = LocalTargetPolicy::new(&root.0).expect("policy");

        assert!(
            policy
                .derive_confined_directory(&root.0.join("linked-directory"))
                .is_err()
        );
    }

    #[test]
    fn session_caches_normalized_paths_and_bounds_unique_inspections() {
        let root = TestDir::new();
        let policy = LocalTargetPolicy::new(&root.0).expect("policy");
        let mut session = LocalTargetSession::new(policy, 1, FilesystemReadLimits::default());

        session
            .inspect(&root.0.join("docs/sub"), "../guide.adoc")
            .expect("first spelling");
        session
            .inspect(&root.0.join("docs"), "./guide.adoc")
            .expect("same normalized path");
        assert_eq!(session.inspected_paths(), 1);
        assert!(matches!(
            session.inspect(&root.0.join("docs"), "missing.adoc"),
            Err(LocalTargetError::LimitExceeded { limit: 1 })
        ));
    }

    #[test]
    fn permission_and_inspection_limit_have_specific_diagnostic_codes() {
        assert_eq!(
            LocalTargetError::PermissionDenied(PathBuf::from("private")).diagnostic_code(),
            "local-target-permission-denied"
        );
        assert_eq!(
            LocalTargetError::LimitExceeded { limit: 1 }.diagnostic_code(),
            "local-target-limit-exceeded"
        );
    }

    #[test]
    fn read_budget_stops_io_after_total_bytes_are_exhausted() {
        let root = TestDir::new();
        fs::write(root.0.join("docs/other.adoc"), "other").expect("second file");
        let policy = LocalTargetPolicy::new(&root.0).expect("policy");
        let mut session = LocalTargetSession::new(
            policy,
            2,
            FilesystemReadLimits {
                max_files: 2,
                max_resource_bytes: 10,
                max_total_bytes: 1,
            },
        );

        assert!(matches!(
            session.read_utf8(&root.0.join("docs"), "guide.adoc"),
            Err(LocalTargetError::ReadLimitExceeded)
        ));
        assert_eq!(session.read_files, 1);
        assert!(matches!(
            session.read_utf8(&root.0.join("docs"), "other.adoc"),
            Err(LocalTargetError::ReadLimitExceeded)
        ));
        assert_eq!(session.read_files, 1);
    }

    #[cfg(unix)]
    #[test]
    fn session_preserves_logical_aliases_while_caching_canonical_file_reads() {
        use std::os::unix::fs::symlink;

        let root = TestDir::new();
        symlink("guide.adoc", root.0.join("docs/alias.adoc")).expect("inside alias");
        let policy = LocalTargetPolicy::new(&root.0).expect("policy");
        let mut session = LocalTargetSession::new(policy, 2, FilesystemReadLimits::default());

        let direct = session
            .read_utf8(&root.0.join("docs"), "guide.adoc")
            .expect("direct target");
        let alias = session
            .read_utf8(&root.0.join("docs"), "alias.adoc")
            .expect("alias target");

        assert_eq!(direct.canonical_path(), alias.canonical_path());
        assert_eq!(direct.source(), alias.source());
        assert_eq!(session.inspected_paths(), 2);
        assert_eq!(session.read_files(), 1);
    }

    #[cfg(unix)]
    #[test]
    fn symlink_escape_is_rejected_even_when_the_leaf_is_missing() {
        use std::os::unix::fs::symlink;

        let root = TestDir::new();
        let outside = TestDir::new();
        symlink(&outside.0, root.0.join("docs/outside")).expect("symlink");
        let policy = LocalTargetPolicy::new(&root.0).expect("policy");

        for target in ["outside/guide.adoc", "outside/missing.adoc"] {
            assert_eq!(
                policy
                    .inspect(&root.0.join("docs"), target)
                    .expect_err("symlink escape")
                    .diagnostic_code(),
                "local-target-outside-root"
            );
        }
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn a_concurrent_rename_along_the_path_does_not_change_the_verdict() {
        use std::os::unix::fs::symlink;
        use std::sync::atomic::{AtomicBool, Ordering};

        let root = TestDir::new();
        let destination =
            include_str!("../../../fixtures/local-target/dangling-symlink.target").trim();
        symlink(destination, root.0.join("docs/escape-dir")).expect("dangling directory symlink");
        fs::write(root.0.join("docs/inside.adoc"), "= Inside\n").expect("regular file");
        fs::create_dir(root.0.join("docs/churn")).expect("churn directory");
        let policy = LocalTargetPolicy::new(&root.0).expect("policy");

        // Renaming a sibling makes the kernel abandon a confined lookup with
        // `EAGAIN`, which is the race that used to surface as an intermittent
        // `local-target-unverifiable`.
        let stop = std::sync::Arc::new(AtomicBool::new(false));
        let churn = {
            let stop = std::sync::Arc::clone(&stop);
            let docs = root.0.join("docs");
            std::thread::spawn(move || {
                let (left, right) = (docs.join("churn"), docs.join("churn-moved"));
                let mut at_left = true;
                while !stop.load(Ordering::Relaxed) {
                    let (from, to) = if at_left {
                        (&left, &right)
                    } else {
                        (&right, &left)
                    };
                    if fs::rename(from, to).is_ok() {
                        at_left = !at_left;
                    }
                }
            })
        };

        let docs = root.0.join("docs");
        for _ in 0..2000 {
            assert_eq!(
                policy
                    .inspect(&docs, "escape-dir/child.adoc")
                    .expect_err("dangling symlink escape")
                    .diagnostic_code(),
                "local-target-outside-root"
            );
            policy.inspect(&docs, "inside.adoc").expect("regular file");
        }
        stop.store(true, Ordering::Relaxed);
        churn.join().expect("churn thread");
    }

    #[cfg(unix)]
    /// The path bound holds whichever way the platform resolves a path.
    ///
    /// The bound limits how much filesystem work one authored document can ask
    /// for. That is a property of the document, not of the operating system, so
    /// a document rejected on Linux must be rejected on macOS and Windows too.
    #[test]
    fn reading_is_bounded_by_the_path_limit_on_every_platform() {
        let root = TestDir::new();
        for name in ["a.adoc", "b.adoc", "c.adoc"] {
            fs::write(root.0.join("docs").join(name), "text\n").expect("source");
        }
        let policy = LocalTargetPolicy::new(&root.0).expect("policy");
        let mut session = LocalTargetSession::new(policy, 2, FilesystemReadLimits::default());
        let docs = root.0.join("docs");

        assert!(session.read_candidate_utf8(&docs.join("a.adoc")).is_ok());
        assert!(session.read_candidate_utf8(&docs.join("b.adoc")).is_ok());
        // A path already read costs nothing, so a repeated reference does not
        // exhaust the bound.
        assert!(session.read_candidate_utf8(&docs.join("a.adoc")).is_ok());
        assert!(matches!(
            session.read_candidate_utf8(&docs.join("c.adoc")),
            Err(LocalTargetError::LimitExceeded { limit: 2 })
        ));
    }

    /// A path that does not exist costs the bound once, however often it is named.
    #[test]
    fn repeating_a_missing_path_costs_the_bound_once() {
        let root = TestDir::new();
        fs::write(root.0.join("docs").join("a.adoc"), "text\n").expect("source");
        let policy = LocalTargetPolicy::new(&root.0).expect("policy");
        let mut session = LocalTargetSession::new(policy, 2, FilesystemReadLimits::default());
        let docs = root.0.join("docs");

        // Only the successful branches recorded the examination, so the second
        // read charged the bound again and the file that exists was refused.
        assert!(
            session
                .read_candidate_utf8(&docs.join("missing.adoc"))
                .is_err()
        );
        assert!(
            session
                .read_candidate_utf8(&docs.join("missing.adoc"))
                .is_err()
        );
        assert!(session.read_candidate_utf8(&docs.join("a.adoc")).is_ok());
    }

    /// Both entry points agree on what a repeated missing path costs.
    #[test]
    fn inspecting_and_reading_charge_a_missing_path_the_same_way() {
        let root = TestDir::new();
        fs::write(root.0.join("docs").join("a.adoc"), "text\n").expect("source");
        let docs = root.0.join("docs");

        let policy = LocalTargetPolicy::new(&root.0).expect("policy");
        let mut reading = LocalTargetSession::new(policy, 2, FilesystemReadLimits::default());
        let _ = reading.read_candidate_utf8(&docs.join("missing.adoc"));
        let _ = reading.read_candidate_utf8(&docs.join("missing.adoc"));

        let policy = LocalTargetPolicy::new(&root.0).expect("policy");
        let mut inspecting = LocalTargetSession::new(policy, 2, FilesystemReadLimits::default());
        let _ = inspecting.inspect(&docs, "missing.adoc");
        let _ = inspecting.inspect(&docs, "missing.adoc");

        assert_eq!(
            reading.read_candidate_utf8(&docs.join("a.adoc")).is_ok(),
            inspecting.inspect(&docs, "a.adoc").is_ok()
        );
    }

    /// Distinct missing paths still each cost the bound.
    #[test]
    fn each_missing_path_costs_the_bound() {
        let root = TestDir::new();
        let policy = LocalTargetPolicy::new(&root.0).expect("policy");
        let mut session = LocalTargetSession::new(policy, 2, FilesystemReadLimits::default());
        let docs = root.0.join("docs");

        assert!(session.read_candidate_utf8(&docs.join("one.adoc")).is_err());
        assert!(session.read_candidate_utf8(&docs.join("two.adoc")).is_err());
        assert!(matches!(
            session.read_candidate_utf8(&docs.join("three.adoc")),
            Err(LocalTargetError::LimitExceeded { limit: 2 })
        ));
    }

    /// One bound covers both entry points rather than each holding its own.
    #[test]
    fn inspecting_and_reading_share_the_same_path_limit() {
        let root = TestDir::new();
        for name in ["a.adoc", "b.adoc"] {
            fs::write(root.0.join("docs").join(name), "text\n").expect("source");
        }
        let policy = LocalTargetPolicy::new(&root.0).expect("policy");
        let mut session = LocalTargetSession::new(policy, 1, FilesystemReadLimits::default());
        let docs = root.0.join("docs");

        assert!(session.inspect(&docs, "a.adoc").is_ok());
        assert!(matches!(
            session.read_candidate_utf8(&docs.join("b.adoc")),
            Err(LocalTargetError::LimitExceeded { limit: 1 })
        ));
        // The path the session already examined is still readable.
        assert!(session.read_candidate_utf8(&docs.join("a.adoc")).is_ok());
    }

    #[cfg(unix)]
    #[test]
    fn dangling_leaf_symlink_escape_uses_the_shared_fixture() {
        use std::os::unix::fs::symlink;

        let root = TestDir::new();
        let destination =
            include_str!("../../../fixtures/local-target/dangling-symlink.target").trim();
        symlink(destination, root.0.join("docs/escape.adoc")).expect("dangling symlink");
        symlink(destination, root.0.join("docs/escape-dir")).expect("dangling directory symlink");
        symlink("inner", root.0.join("docs/escape-chain")).expect("first symlink");
        symlink(destination, root.0.join("docs/inner")).expect("second symlink");
        let policy = LocalTargetPolicy::new(&root.0).expect("policy");

        for target in [
            "escape.adoc",
            "escape-dir/child.adoc",
            "escape-chain/child.adoc",
        ] {
            // `Unverifiable` carries the underlying errno in its message, so the
            // whole error is reported here. Without it a failure only states
            // that the code differs, which is what left the earlier occurrences
            // of this flake undiagnosable.
            let error = policy
                .inspect(&root.0.join("docs"), target)
                .expect_err("dangling symlink escape");
            assert_eq!(
                error.diagnostic_code(),
                "local-target-outside-root",
                "{target}: {error:?}"
            );
        }
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn opened_file_is_stable_when_ancestor_is_replaced_with_outside_symlink() {
        use std::os::unix::fs::symlink;

        let root = TestDir::new();
        let outside = TestDir::new();
        fs::write(outside.0.join("guide.adoc"), "= Outside").expect("outside file");
        let policy = LocalTargetPolicy::new(&root.0).expect("policy");
        assert_eq!(
            policy.race_resistance(),
            FilesystemRaceResistance::HandleRelative
        );
        let mut session = LocalTargetSession::new(policy, 1, FilesystemReadLimits::default());
        let docs = root.0.join("docs");
        let displaced = root.0.join("displaced");

        let loaded = session
            .read_utf8_after_open(&docs, "guide.adoc", || {
                fs::rename(&docs, &displaced).expect("rename inspected ancestor");
                symlink(&outside.0, &docs).expect("replace ancestor with outside symlink");
            })
            .expect("read opened file");

        assert_eq!(loaded.source(), "= Guide");
        assert_ne!(loaded.source(), "= Outside");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn policy_keeps_the_original_root_handle_after_namespace_replacement() {
        use std::os::unix::fs::symlink;

        let root = TestDir::new();
        let outside = TestDir::new();
        fs::write(outside.0.join("docs/guide.adoc"), "= Outside").expect("outside file");
        let policy = LocalTargetPolicy::new(&root.0).expect("policy");
        let mut session = LocalTargetSession::new(policy, 1, FilesystemReadLimits::default());
        let displaced = root.0.with_extension("anchored");
        fs::rename(&root.0, &displaced).expect("displace trusted root");
        symlink(&outside.0, &root.0).expect("replace root path");

        let loaded = session
            .read_candidate_utf8(&root.0.join("docs/guide.adoc"))
            .expect("read through retained root handle");

        assert_eq!(loaded.source(), "= Guide");
        assert_ne!(loaded.source(), "= Outside");
        fs::remove_file(&root.0).expect("remove replacement symlink");
        fs::rename(displaced, &root.0).expect("restore trusted root");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn openat2_unavailable_errors_use_the_handle_relative_fallback() {
        let root = TestDir::new();
        let policy = LocalTargetPolicy::new(&root.0).expect("policy");
        for error in [rustix::io::Errno::NOSYS, rustix::io::Errno::INVAL] {
            FORCED_OPENAT2_ERROR.with(|forced| forced.set(Some(error)));
            let mut session =
                LocalTargetSession::new(policy.clone(), 1, FilesystemReadLimits::default());
            let loaded = session
                .read_utf8(&root.0.join("docs"), "guide.adoc")
                .expect("fallback read");
            FORCED_OPENAT2_ERROR.with(|forced| forced.set(None));
            assert_eq!(loaded.source(), "= Guide");
        }
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn openat2_fallback_verifies_rename_and_rejects_unlink() {
        let renamed_root = TestDir::new();
        let renamed_policy = LocalTargetPolicy::new(&renamed_root.0).expect("rename policy");
        let mut renamed_session =
            LocalTargetSession::new(renamed_policy, 1, FilesystemReadLimits::default());
        let renamed_target = renamed_root.0.join("docs/guide.adoc");
        let displaced = renamed_root.0.join("docs/original.adoc");
        FORCED_OPENAT2_ERROR.with(|forced| forced.set(Some(rustix::io::Errno::NOSYS)));
        let renamed = renamed_session.read_utf8_after_open(
            &renamed_root.0.join("docs"),
            "guide.adoc",
            || fs::rename(&renamed_target, &displaced).expect("rename opened target"),
        );
        FORCED_OPENAT2_ERROR.with(|forced| forced.set(None));
        let renamed = renamed.expect("fallback verifies the renamed inode");
        assert_eq!(renamed.canonical_path(), displaced);
        assert_eq!(renamed.source(), "= Guide");

        let unlinked_root = TestDir::new();
        let unlinked_policy = LocalTargetPolicy::new(&unlinked_root.0).expect("unlink policy");
        let mut unlinked_session =
            LocalTargetSession::new(unlinked_policy, 1, FilesystemReadLimits::default());
        let unlinked_target = unlinked_root.0.join("docs/guide.adoc");
        FORCED_OPENAT2_ERROR.with(|forced| forced.set(Some(rustix::io::Errno::NOSYS)));
        let unlinked = unlinked_session.read_utf8_after_open(
            &unlinked_root.0.join("docs"),
            "guide.adoc",
            || fs::remove_file(&unlinked_target).expect("unlink opened target"),
        );
        FORCED_OPENAT2_ERROR.with(|forced| forced.set(None));
        assert!(matches!(unlinked, Err(LocalTargetError::Unverifiable(_))));
        assert_eq!(unlinked_session.read_files(), 0);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn procfs_failures_are_unverifiable_during_policy_creation() {
        let root = TestDir::new();
        for kind in [
            std::io::ErrorKind::NotFound,
            std::io::ErrorKind::PermissionDenied,
        ] {
            FORCED_FD_PATH_ERROR.with(|forced| forced.set(Some(kind)));
            let error = LocalTargetPolicy::new(&root.0).expect_err("procfs failure");
            FORCED_FD_PATH_ERROR.with(|forced| forced.set(None));

            assert!(matches!(error, LocalTargetError::Unverifiable(_)));
            assert_eq!(error.diagnostic_code(), "local-target-unverifiable");
            assert!(error.to_string().contains("/proc/self/fd"));
        }
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn procfs_failures_are_unverifiable_after_policy_creation() {
        let root = TestDir::new();
        let policy = LocalTargetPolicy::new(&root.0).expect("policy");
        let target = root.0.join("docs/guide.adoc");
        for kind in [
            std::io::ErrorKind::NotFound,
            std::io::ErrorKind::PermissionDenied,
        ] {
            FORCED_FD_PATH_ERROR.with(|forced| forced.set(Some(kind)));
            let error = policy
                .inspect_candidate(&target)
                .expect_err("procfs failure");
            FORCED_FD_PATH_ERROR.with(|forced| forced.set(None));

            assert!(matches!(error, LocalTargetError::Unverifiable(_)));
            assert_eq!(error.diagnostic_code(), "local-target-unverifiable");
            assert!(error.to_string().contains("/proc/self/fd"));
        }
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn relative_base_keeps_the_original_root_namespace_after_replacement() {
        use std::os::unix::fs::symlink;

        let root = TestDir::new();
        fs::create_dir_all(root.0.join("docs/sub")).expect("trusted base");
        fs::create_dir_all(root.0.join("docs/other")).expect("alternate trusted base");
        fs::write(root.0.join("docs/sub/guide.adoc"), "= Trusted").expect("trusted target");
        fs::write(root.0.join("docs/other/guide.adoc"), "= Redirected").expect("redirected target");
        let policy = LocalTargetPolicy::new(&root.0).expect("policy");
        let mut session = LocalTargetSession::new(policy, 1, FilesystemReadLimits::default());
        let displaced = root.0.with_extension("anchored-base");

        fs::rename(&root.0, &displaced).expect("displace trusted root");
        fs::create_dir_all(root.0.join("docs/other")).expect("replacement target directory");
        symlink("other", root.0.join("docs/sub")).expect("redirect replacement base");

        let loaded = session
            .read_utf8(&root.0.join("docs/sub"), "guide.adoc")
            .expect("read from retained base namespace");

        assert_eq!(loaded.source(), "= Trusted");
        assert_ne!(loaded.source(), "= Redirected");
        fs::remove_dir_all(&root.0).expect("remove replacement root");
        fs::rename(displaced, &root.0).expect("restore trusted root");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn fifo_is_rejected_without_waiting_for_a_writer() {
        use rustix::fs::{CWD, Mode, mkfifoat};

        let root = TestDir::new();
        let fifo = root.0.join("docs/input.adoc");
        mkfifoat(CWD, &fifo, Mode::RUSR | Mode::WUSR).expect("FIFO");
        let policy = LocalTargetPolicy::new(&root.0).expect("policy");
        let mut session = LocalTargetSession::new(policy, 1, FilesystemReadLimits::default());

        assert!(matches!(
            session.read_candidate_utf8(&fifo),
            Err(LocalTargetError::NotFile(path)) if path == fifo
        ));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn opened_file_is_stable_when_leaf_is_renamed_and_replaced() {
        let root = TestDir::new();
        let policy = LocalTargetPolicy::new(&root.0).expect("policy");
        let mut session = LocalTargetSession::new(policy, 1, FilesystemReadLimits::default());
        let target = root.0.join("docs/guide.adoc");
        let displaced = root.0.join("docs/original.adoc");

        let loaded = session
            .read_utf8_after_open(&root.0.join("docs"), "guide.adoc", || {
                fs::rename(&target, &displaced).expect("rename inspected file");
                fs::write(&target, "= Replacement").expect("replace inspected file");
            })
            .expect("read opened file");

        assert_eq!(loaded.source(), "= Guide");
        assert_ne!(loaded.source(), "= Replacement");
        assert_eq!(loaded.canonical_path(), displaced);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn unlinked_opened_file_fails_closed_instead_of_accepting_the_deleted_suffix() {
        let root = TestDir::new();
        let policy = LocalTargetPolicy::new(&root.0).expect("policy");
        let mut session = LocalTargetSession::new(policy, 2, FilesystemReadLimits::default());
        let target = root.0.join("docs/guide.adoc");

        let error = session
            .read_utf8_after_open(&root.0.join("docs"), "guide.adoc", || {
                fs::remove_file(&target).expect("unlink opened file");
            })
            .expect_err("unlinked identity must fail closed");

        assert!(matches!(error, LocalTargetError::Unverifiable(_)));
        assert_eq!(error.diagnostic_code(), "local-target-unverifiable");
        assert_eq!(session.read_files, 0);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn a_real_file_name_ending_in_deleted_is_preserved() {
        let root = TestDir::new();
        let target = root.0.join("docs/guide.adoc (deleted)");
        fs::write(&target, "literal suffix").expect("suffix source");
        let policy = LocalTargetPolicy::new(&root.0).expect("policy");
        let mut session = LocalTargetSession::new(policy, 1, FilesystemReadLimits::default());

        let loaded = session
            .read_candidate_utf8(&target)
            .expect("literal suffix is a valid file name");

        assert_eq!(loaded.canonical_path(), target);
        assert_eq!(loaded.source(), "literal suffix");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn deleted_suffix_collision_cannot_reuse_an_unlinked_files_cache_entry() {
        let root = TestDir::new();
        let target = root.0.join("docs/guide.adoc");
        let suffix = root.0.join("docs/guide.adoc (deleted)");
        fs::write(&suffix, "literal suffix").expect("suffix source");
        let policy = LocalTargetPolicy::new(&root.0).expect("policy");
        let mut session = LocalTargetSession::new(policy, 2, FilesystemReadLimits::default());

        let error = session
            .read_utf8_after_open(&root.0.join("docs"), "guide.adoc", || {
                fs::remove_file(&target).expect("unlink opened file");
            })
            .expect_err("suffix collision must not verify another inode");
        assert!(matches!(error, LocalTargetError::Unverifiable(_)));

        let loaded = session
            .read_candidate_utf8(&suffix)
            .expect("read the literal suffix file");
        assert_eq!(loaded.canonical_path(), suffix);
        assert_eq!(loaded.source(), "literal suffix");
        assert_eq!(session.read_files, 1);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn identity_reopen_rejects_a_leaf_swapped_after_procfs_resolution() {
        let root = TestDir::new();
        let policy = LocalTargetPolicy::new(&root.0).expect("policy");
        let target = root.0.join("docs/guide.adoc");
        let displaced = root.0.join("docs/original.adoc");
        let opened = fs::File::open(&target).expect("opened target");

        let result = logical_path_from_opened_handle_with(
            policy.root(),
            policy.root_handle.as_ref(),
            &opened,
            &target,
            || {},
            || {
                fs::rename(&target, &displaced).expect("rename resolved target");
                fs::write(&target, "= Replacement").expect("replace resolved target");
            },
        );

        assert!(matches!(result, Err(LocalTargetError::Unverifiable(_))));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn root_rename_between_procfs_reads_fails_closed() {
        let root = TestDir::new();
        let policy = LocalTargetPolicy::new(&root.0).expect("policy");
        let target = root.0.join("docs/guide.adoc");
        let opened = fs::File::open(&target).expect("opened target");
        let displaced = root.0.with_extension("between-fd-reads");

        let result = logical_path_from_opened_handle_with(
            policy.root(),
            policy.root_handle.as_ref(),
            &opened,
            &target,
            || fs::rename(&root.0, &displaced).expect("rename root between fd reads"),
            || {},
        );

        fs::rename(&displaced, &root.0).expect("restore root");
        assert!(matches!(result, Err(LocalTargetError::Unverifiable(_))));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn byte_read_unlink_does_not_consume_the_read_budget() {
        let root = TestDir::new();
        let target = root.0.join("docs/guide.adoc");
        let suffix = root.0.join("docs/guide.adoc (deleted)");
        fs::write(&suffix, b"suffix").expect("suffix bytes");
        let policy = LocalTargetPolicy::new(&root.0).expect("policy");
        let mut session = LocalTargetSession::new(policy, 2, FilesystemReadLimits::default());

        let error = session
            .read_candidate_bytes_after_open(&target, || {
                fs::remove_file(&target).expect("unlink opened bytes");
            })
            .expect_err("unlinked bytes must fail before reading");
        assert!(matches!(error, LocalTargetError::Unverifiable(_)));
        assert_eq!(session.read_files(), 0);
        assert_eq!(session.read_bytes, 0);

        let loaded = session
            .read_candidate_bytes(&suffix)
            .expect("literal suffix bytes remain readable");
        assert_eq!(loaded.canonical_path(), suffix);
        assert_eq!(loaded.source(), b"suffix");
        assert_eq!(session.read_files(), 1);
        assert_eq!(session.read_bytes, 6);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn same_path_replacement_keeps_the_command_snapshot_until_reread() {
        let root = TestDir::new();
        let target = root.0.join("docs/guide.adoc");
        let displaced = root.0.join("docs/first.adoc");
        let policy = LocalTargetPolicy::new(&root.0).expect("policy");
        let mut session = LocalTargetSession::new(policy, 3, FilesystemReadLimits::default());

        let first = session
            .read_candidate_utf8(&target)
            .expect("initial snapshot");
        fs::rename(&target, &displaced).expect("retain first inode");
        fs::write(&target, "= Second").expect("second inode");
        let cached = session
            .read_candidate_utf8(&target)
            .expect("cached command snapshot");

        assert_eq!(first.source(), "= Guide");
        assert_eq!(cached.source(), "= Guide");
        assert_eq!(session.read_files(), 1);

        let capacity = session.default_read_capacity();
        let (reread, _) = session
            .reread_candidate_utf8_with_capacity(&target, |_| capacity)
            .expect("explicit reread");
        assert_eq!(reread.source(), "= Second");
        assert_eq!(session.read_files(), 2);
    }
}
