use std::collections::BTreeMap;
use std::error::Error;
use std::fmt;
use std::fs;
use std::io::Read;
use std::path::{Component, Path, PathBuf};

use crate::local_resource::ResourceLimits;

/// Filesystem boundary for checking an authored relative target.
///
/// The policy owns one canonical project root. It permits parent components
/// only while the normalized path remains below that root, then resolves
/// symlinks before accepting an existing regular file.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LocalTargetPolicy {
    root: PathBuf,
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

    pub fn inspect_candidate(&self, candidate: &Path) -> Result<PathBuf, LocalTargetError> {
        if !candidate.starts_with(&self.root) {
            return Err(LocalTargetError::OutsideRoot(candidate.to_owned()));
        }
        let canonical = match candidate.canonicalize() {
            Ok(path) => path,
            Err(source) if source.kind() == std::io::ErrorKind::NotFound => {
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
        let canonical_base = if let Some(result) = self.bases.get(base) {
            result.clone()?
        } else {
            let result = base
                .canonicalize()
                .map_err(|source| classify_io(base.to_owned(), source));
            self.bases.insert(base.to_owned(), result.clone());
            result?
        };
        let candidate = self
            .policy
            .candidate_from_canonical_base(&canonical_base, target)?;
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

    pub fn read_utf8(
        &mut self,
        base: &Path,
        target: &str,
    ) -> Result<(PathBuf, String), LocalTargetError> {
        let canonical = self.inspect(base, target)?;
        if let Some(result) = self.text.get(&canonical) {
            return result.clone().map(|text| (canonical, text));
        }
        let result = fs::File::open(&canonical)
            .map_err(|source| classify_io(canonical.clone(), source))
            .and_then(|file| {
                let mut bytes = Vec::new();
                file.take(self.limits.max_resource_bytes.saturating_add(1))
                    .read_to_end(&mut bytes)
                    .map_err(|source| classify_io(canonical.clone(), source))?;
                if bytes.len() as u64 > self.limits.max_resource_bytes {
                    return Err(LocalTargetError::ResourceTooLarge(canonical.clone()));
                }
                if self.read_files >= self.limits.max_files {
                    return Err(LocalTargetError::ReadLimitExceeded);
                }
                let total = self
                    .read_bytes
                    .checked_add(bytes.len() as u64)
                    .filter(|total| *total <= self.limits.max_total_bytes)
                    .ok_or(LocalTargetError::ReadLimitExceeded)?;
                self.read_files += 1;
                self.read_bytes = total;
                String::from_utf8(bytes).map_err(|source| {
                    LocalTargetError::Unverifiable(format!(
                        "{} is not UTF-8: {source}",
                        canonical.display()
                    ))
                })
            });
        self.text.insert(canonical.clone(), result.clone());
        result.map(|text| (canonical, text))
    }

    pub fn inspected_paths(&self) -> usize {
        self.inspections.len()
    }
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

fn ensure_existing_ancestor_is_inside(
    root: &Path,
    candidate: &Path,
) -> Result<(), LocalTargetError> {
    let mut ancestor = candidate.parent();
    while let Some(path) = ancestor {
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
            Self::LimitExceeded { .. } | Self::ResourceTooLarge(_) | Self::ReadLimitExceeded => {
                "local-target-unverifiable"
            }
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
}
