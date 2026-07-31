use std::collections::BTreeMap;
#[cfg(not(target_os = "linux"))]
use std::collections::BTreeSet;
use std::error::Error;
use std::fmt;
use std::fs;
use std::io::Read;
use std::path::{Component, Path, PathBuf};

use crate::local_resource::FilesystemReadLimits;

/// How many times a confined open may be retried after a concurrent-change race.
///
/// Each retry is a single syscall against a path that is almost always stable,
/// so a small bound absorbs ordinary churn without turning a persistently
/// changing directory into an unbounded wait.
#[cfg(target_os = "linux")]
const CONFINED_OPEN_ATTEMPTS: u32 = 8;

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
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LocalTargetPolicy {
    root: PathBuf,
}

#[cfg(target_os = "linux")]
struct OpenedTarget {
    canonical_path: PathBuf,
    file: fs::File,
}

impl LocalTargetPolicy {
    pub fn new(root: &Path) -> Result<Self, LocalTargetError> {
        let canonical = root
            .canonicalize()
            .map_err(|source| classify_io(root.to_owned(), source))?;
        if !canonical.is_dir() {
            return Err(LocalTargetError::NotDirectory(canonical));
        }
        Ok(Self { root: canonical })
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

    pub fn inspect(&self, base: &Path, target: &str) -> Result<PathBuf, LocalTargetError> {
        let candidate = self.candidate(base, target)?;
        self.inspect_candidate(&candidate)
    }

    /// Resolves URL path syntax and parent components without touching the target.
    ///
    /// Callers may use the returned normalized path as a per-run cache key.
    pub fn candidate(&self, base: &Path, target: &str) -> Result<PathBuf, LocalTargetError> {
        let base = base
            .canonicalize()
            .map_err(|source| classify_io(base.to_owned(), source))?;
        self.candidate_from_canonical_base(&base, target)
    }

    fn candidate_from_canonical_base(
        &self,
        base: &Path,
        target: &str,
    ) -> Result<PathBuf, LocalTargetError> {
        if !base.is_dir() {
            return Err(LocalTargetError::NotDirectory(base.to_owned()));
        }
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
        use std::os::fd::AsRawFd;

        use rustix::fd::OwnedFd;
        use rustix::fs::{Mode, OFlags, ResolveFlags, openat, openat2};

        let relative = candidate
            .strip_prefix(&self.root)
            .map_err(|_| LocalTargetError::OutsideRoot(candidate.to_owned()))?;
        let root =
            fs::File::open(&self.root).map_err(|source| classify_io(self.root.clone(), source))?;
        let flags = OFlags::RDONLY | OFlags::CLOEXEC;
        // `RESOLVE_BENEATH` makes the kernel give up with `EAGAIN` when another
        // process renames or mounts something along this path while it is being
        // resolved. The lookup was neither denied nor granted, so the only
        // correct response is to look again. The attempt count is bounded so a
        // filesystem under constant churn fails instead of spinning.
        let mut attempts = 0;
        let file = loop {
            let outcome = openat2(
                &root,
                relative,
                flags,
                Mode::empty(),
                ResolveFlags::BENEATH | ResolveFlags::NO_MAGICLINKS,
            );
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
                    &root,
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
        let descriptor_path = PathBuf::from(format!("/proc/self/fd/{}", file.as_raw_fd()));
        let canonical_path = fs::read_link(&descriptor_path)
            .map_err(|source| classify_io(candidate.to_owned(), source))?;
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
    text: BTreeMap<PathBuf, Result<String, LocalTargetError>>,
}

/// UTF-8 local target returned by a bounded validation session.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LoadedLocalTarget {
    canonical_path: PathBuf,
    source: String,
}

impl LoadedLocalTarget {
    pub fn canonical_path(&self) -> &Path {
        &self.canonical_path
    }

    pub fn source(&self) -> &str {
        &self.source
    }

    pub fn into_parts(self) -> (PathBuf, String) {
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

    fn candidate(&mut self, base: &Path, target: &str) -> Result<PathBuf, LocalTargetError> {
        let canonical_base = if let Some(result) = self.bases.get(base) {
            result.clone()?
        } else {
            let result = base
                .canonicalize()
                .map_err(|source| classify_io(base.to_owned(), source));
            self.bases.insert(base.to_owned(), result.clone());
            result?
        };
        self.policy
            .candidate_from_canonical_base(&canonical_base, target)
    }

    pub fn read_utf8(
        &mut self,
        base: &Path,
        target: &str,
    ) -> Result<LoadedLocalTarget, LocalTargetError> {
        let candidate = self.candidate(base, target)?;
        self.read_candidate_utf8(&candidate)
    }

    pub(crate) fn read_utf8_with_capacity(
        &mut self,
        base: &Path,
        target: &str,
        capacity: impl FnOnce(&Path) -> CandidateReadCapacity,
    ) -> Result<LoadedLocalTarget, LocalTargetError> {
        let candidate = self.candidate(base, target)?;
        self.read_candidate_utf8_with_capacity(&candidate, false, || {}, capacity)
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
        self.read_candidate_utf8_with_capacity(candidate, true, || {}, |_| capacity)
    }

    /// Reopens an already normalized path without reusing cached text.
    pub(crate) fn reread_candidate_utf8_with_capacity(
        &mut self,
        candidate: &Path,
        capacity: impl FnOnce(&Path) -> CandidateReadCapacity,
    ) -> Result<LoadedLocalTarget, LocalTargetError> {
        self.read_candidate_utf8_with_capacity(candidate, false, || {}, capacity)
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
        self.read_candidate_utf8_with_capacity(&candidate, true, after_open, |_| capacity)
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
        after_open: impl FnOnce(),
        capacity: impl FnOnce(&Path) -> CandidateReadCapacity,
    ) -> Result<LoadedLocalTarget, LocalTargetError> {
        // The number of distinct paths a session may examine is a resource
        // bound, so it holds on every platform. The two branches below differ
        // only in how a path is resolved and opened.
        self.charge_path_request(candidate)?;
        #[cfg(target_os = "linux")]
        let (canonical, file) = {
            let opened = self.policy.open_confined(candidate)?;
            self.inspections
                .insert(candidate.to_owned(), Ok(opened.canonical_path.clone()));
            (opened.canonical_path, opened.file)
        };
        #[cfg(not(target_os = "linux"))]
        let (canonical, file) = {
            let canonical = self.policy.inspect_candidate(candidate)?;
            self.inspections
                .insert(candidate.to_owned(), Ok(canonical.clone()));
            let file = self.policy.open_confined(&canonical)?;
            (canonical, file)
        };
        after_open();
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
            String::from_utf8(bytes).map_err(|_| LocalTargetError::InvalidUtf8(canonical.clone()))
        })();
        if reuse_cached_text {
            self.text.insert(canonical.clone(), result.clone());
        }
        result.map(|source| LoadedLocalTarget {
            canonical_path: canonical,
            source,
        })
    }

    pub fn inspected_paths(&self) -> usize {
        self.inspections.len()
    }

    pub(crate) fn has_inspected_candidate(&self, candidate: &Path) -> bool {
        self.inspections.contains_key(candidate)
    }

    pub(crate) fn release_candidate(&mut self, candidate: &Path) {
        if let Some(result) = self.inspections.remove(candidate) {
            self.requests = self.requests.saturating_sub(1);
            if let Ok(canonical) = result {
                self.text.remove(&canonical);
            }
        }
    }

    pub fn read_files(&self) -> usize {
        self.read_files
    }
}

#[cfg(target_os = "linux")]
fn classify_errno(path: &Path, source: rustix::io::Errno) -> LocalTargetError {
    if source == rustix::io::Errno::XDEV {
        return LocalTargetError::OutsideRoot(path.to_owned());
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
    }
}
