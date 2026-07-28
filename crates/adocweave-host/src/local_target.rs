use std::collections::BTreeMap;
#[cfg(not(target_os = "linux"))]
use std::collections::BTreeSet;
use std::error::Error;
use std::fmt;
use std::fs;
use std::io::Read;
use std::path::{Component, Path, PathBuf};

use crate::local_resource::ResourceLimits;

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
        let file = match openat2(
            &root,
            relative,
            flags,
            Mode::empty(),
            ResolveFlags::BENEATH | ResolveFlags::NO_MAGICLINKS,
        ) {
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
    limits: ResourceLimits,
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
    pub fn new(policy: LocalTargetPolicy, max_paths: usize, limits: ResourceLimits) -> Self {
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
        if self.requests >= self.max_paths {
            return Err(LocalTargetError::LimitExceeded {
                limit: self.max_paths,
            });
        }
        self.requests += 1;
        let result = self.policy.inspect_candidate(&candidate);
        self.inspections.insert(candidate, result.clone());
        result
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
        self.read_utf8_after_open(base, target, || {})
    }

    fn read_utf8_after_open(
        &mut self,
        base: &Path,
        target: &str,
        after_open: impl FnOnce(),
    ) -> Result<LoadedLocalTarget, LocalTargetError> {
        #[cfg(target_os = "linux")]
        let (canonical, file) = {
            let candidate = self.candidate(base, target)?;
            if !self.inspections.contains_key(&candidate) {
                if self.requests >= self.max_paths {
                    return Err(LocalTargetError::LimitExceeded {
                        limit: self.max_paths,
                    });
                }
                self.requests += 1;
            }
            let opened = self.policy.open_confined(&candidate)?;
            self.inspections
                .insert(candidate, Ok(opened.canonical_path.clone()));
            (opened.canonical_path, opened.file)
        };
        #[cfg(not(target_os = "linux"))]
        let (canonical, file) = {
            let canonical = self.inspect(base, target)?;
            let file = self.policy.open_confined(&canonical)?;
            (canonical, file)
        };
        after_open();
        if let Some(result) = self.text.get(&canonical) {
            return result.clone().map(|source| LoadedLocalTarget {
                canonical_path: canonical,
                source,
            });
        }
        if self.read_files >= self.limits.max_files
            || self.read_bytes >= self.limits.max_total_bytes
        {
            return Err(LocalTargetError::ReadLimitExceeded);
        }
        let remaining = self.limits.max_total_bytes - self.read_bytes;
        let read_limit = self
            .limits
            .max_resource_bytes
            .min(remaining)
            .saturating_add(1);
        let result = (|| {
            self.read_files += 1;
            let mut bytes = Vec::new();
            file.take(read_limit)
                .read_to_end(&mut bytes)
                .map_err(|source| classify_io(canonical.clone(), source))?;
            self.read_bytes = self.read_bytes.saturating_add(bytes.len() as u64);
            if self.read_bytes > self.limits.max_total_bytes {
                return Err(LocalTargetError::ReadLimitExceeded);
            }
            if bytes.len() as u64 > self.limits.max_resource_bytes {
                return Err(LocalTargetError::ResourceTooLarge(canonical.clone()));
            }
            String::from_utf8(bytes).map_err(|source| {
                LocalTargetError::Unverifiable(format!(
                    "{} is not UTF-8: {source}",
                    canonical.display()
                ))
            })
        })();
        self.text.insert(canonical.clone(), result.clone());
        result.map(|source| LoadedLocalTarget {
            canonical_path: canonical,
            source,
        })
    }

    pub fn inspected_paths(&self) -> usize {
        self.inspections.len()
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
            Self::Unverifiable(_) => "local-target-unverifiable",
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
    use std::time::{SystemTime, UNIX_EPOCH};

    struct TestDir(PathBuf);

    impl TestDir {
        fn new() -> Self {
            let nonce = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("clock after epoch")
                .as_nanos();
            let path = std::env::temp_dir().join(format!(
                "adocweave-local-target-{}-{nonce}",
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
        let mut session = LocalTargetSession::new(policy, 1, ResourceLimits::default());

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
            ResourceLimits {
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
        let mut session = LocalTargetSession::new(policy, 2, ResourceLimits::default());

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
            assert_eq!(
                policy
                    .inspect(&root.0.join("docs"), target)
                    .expect_err("dangling symlink escape")
                    .diagnostic_code(),
                "local-target-outside-root"
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
        let mut session = LocalTargetSession::new(policy, 1, ResourceLimits::default());
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
        let mut session = LocalTargetSession::new(policy, 1, ResourceLimits::default());
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
