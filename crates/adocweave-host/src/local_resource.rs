use std::error::Error;
use std::fmt;
use std::fs;
use std::io::Read;
use std::path::{Component, Path, PathBuf};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ResourceLimits {
    pub max_files: usize,
    pub max_total_bytes: u64,
    pub max_resource_bytes: u64,
}

impl Default for ResourceLimits {
    fn default() -> Self {
        Self {
            max_files: 10_000,
            max_total_bytes: 50 * 1024 * 1024,
            max_resource_bytes: 10 * 1024 * 1024,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LocalResourcePolicy {
    roots: Vec<PathBuf>,
    limits: ResourceLimits,
}

/// Bytes captured after filesystem-policy validation and budget accounting.
///
/// The fields are private so a validated capability cannot be forged or
/// combined with a different policy before decoding.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ValidatedFilesystemTarget {
    canonical_path: PathBuf,
    bytes: Vec<u8>,
}

impl ValidatedFilesystemTarget {
    pub fn canonical_path(&self) -> &Path {
        &self.canonical_path
    }

    pub fn into_loaded_utf8(self) -> Result<LoadedLocalResource, ResourceError> {
        let source =
            String::from_utf8(self.bytes).map_err(|source| ResourceError::InvalidUtf8 {
                path: self.canonical_path.clone(),
                source: source.to_string(),
            })?;
        Ok(LoadedLocalResource {
            canonical_path: self.canonical_path,
            source,
        })
    }
}

/// UTF-8 resource loaded through a [`LocalResourcePolicy`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LoadedLocalResource {
    canonical_path: PathBuf,
    source: String,
}

impl LoadedLocalResource {
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

impl LocalResourcePolicy {
    pub fn new(
        roots: impl IntoIterator<Item = PathBuf>,
        limits: ResourceLimits,
    ) -> Result<Self, ResourceError> {
        let mut roots = roots
            .into_iter()
            .map(|path| {
                path.canonicalize()
                    .map_err(|source| ResourceError::Inspect {
                        path,
                        source: source.to_string(),
                    })
            })
            .collect::<Result<Vec<_>, _>>()?;
        roots.sort();
        roots.dedup();
        if roots.is_empty() {
            return Err(ResourceError::NoRoots);
        }
        if roots.iter().any(|root| !root.is_dir()) {
            return Err(ResourceError::InvalidRoot);
        }
        Ok(Self { roots, limits })
    }

    pub fn roots(&self) -> &[PathBuf] {
        &self.roots
    }

    pub const fn limits(&self) -> ResourceLimits {
        self.limits
    }

    pub fn canonical_file(&self, path: &Path) -> Result<PathBuf, ResourceError> {
        let canonical = path
            .canonicalize()
            .map_err(|source| ResourceError::Inspect {
                path: path.to_owned(),
                source: source.to_string(),
            })?;
        if !self.roots.iter().any(|root| canonical.starts_with(root)) {
            return Err(ResourceError::OutsideRoots(canonical));
        }
        let metadata = fs::metadata(&canonical).map_err(|source| ResourceError::Inspect {
            path: canonical.clone(),
            source: source.to_string(),
        })?;
        if !metadata.is_file() {
            return Err(ResourceError::NotRegularFile(canonical));
        }
        Ok(canonical)
    }

    pub fn resolve_relative(&self, base: &Path, target: &str) -> Result<PathBuf, ResourceError> {
        let relative = normalize_relative(target)?;
        self.canonical_file(&base.join(relative))
    }

    pub fn validate_file(
        &self,
        budget: &mut ResourceBudget,
        path: &Path,
    ) -> Result<ValidatedFilesystemTarget, ResourceError> {
        let canonical = self.canonical_file(path)?;
        let file = fs::File::open(&canonical).map_err(|source| ResourceError::Read {
            path: canonical.clone(),
            source: source.to_string(),
        })?;
        let mut bytes = Vec::new();
        file.take(self.limits.max_resource_bytes.saturating_add(1))
            .read_to_end(&mut bytes)
            .map_err(|source| ResourceError::Read {
                path: canonical.clone(),
                source: source.to_string(),
            })?;
        budget.charge(&canonical, bytes.len() as u64, self.limits)?;
        Ok(ValidatedFilesystemTarget {
            canonical_path: canonical,
            bytes,
        })
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
        limits: ResourceLimits,
    ) -> Result<(), ResourceError> {
        if bytes > limits.max_resource_bytes {
            return Err(ResourceError::ResourceTooLarge(path.to_owned()));
        }
        let files = self.files.checked_add(1).ok_or(ResourceError::FileLimit)?;
        if files > limits.max_files {
            return Err(ResourceError::FileLimit);
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
    InvalidTarget(String),
    OutsideRoots(PathBuf),
    NotRegularFile(PathBuf),
    Inspect { path: PathBuf, source: String },
    Read { path: PathBuf, source: String },
    InvalidUtf8 { path: PathBuf, source: String },
    ResourceTooLarge(PathBuf),
    FileLimit,
    ByteLimit,
}

impl fmt::Display for ResourceError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoRoots => formatter.write_str("no local resource roots were configured"),
            Self::InvalidRoot => formatter.write_str("local resource root is not a directory"),
            Self::InvalidTarget(target) => {
                write!(formatter, "unsafe local resource target: {target}")
            }
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
            Self::FileLimit => formatter.write_str("local resource file limit exceeded"),
            Self::ByteLimit => formatter.write_str("local resource byte limit exceeded"),
        }
    }
}

impl Error for ResourceError {}

pub fn normalize_relative(target: &str) -> Result<PathBuf, ResourceError> {
    if target.is_empty() || target.contains(':') || target.chars().any(char::is_control) {
        return Err(ResourceError::InvalidTarget(target.to_owned()));
    }
    let mut safe = PathBuf::new();
    for component in Path::new(target).components() {
        match component {
            Component::Normal(value) => safe.push(value),
            Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                return Err(ResourceError::InvalidTarget(target.to_owned()));
            }
        }
    }
    if safe.as_os_str().is_empty() {
        Err(ResourceError::InvalidTarget(target.to_owned()))
    } else {
        Ok(safe)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
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

    fn policy(root: &Path, max_resource_bytes: u64) -> LocalResourcePolicy {
        LocalResourcePolicy::new(
            [root.to_owned()],
            ResourceLimits {
                max_files: 10,
                max_total_bytes: 100,
                max_resource_bytes,
            },
        )
        .expect("valid policy")
    }

    #[test]
    fn budget_rejects_without_partially_charging() {
        let limits = ResourceLimits {
            max_files: 1,
            max_total_bytes: 3,
            max_resource_bytes: 3,
        };
        let mut budget = ResourceBudget::default();
        budget.charge(Path::new("a"), 3, limits).expect("boundary");
        assert_eq!((budget.files(), budget.bytes()), (1, 3));
        assert_eq!(
            budget.charge(Path::new("b"), 1, limits),
            Err(ResourceError::FileLimit)
        );
        assert_eq!((budget.files(), budget.bytes()), (1, 3));
    }

    #[test]
    fn relative_targets_reject_parent_absolute_scheme_and_controls() {
        for target in ["../a", "/a", "file:a", "a\0b", ""] {
            assert!(matches!(
                normalize_relative(target),
                Err(ResourceError::InvalidTarget(_))
            ));
        }
        assert_eq!(
            normalize_relative("a/./b").expect("safe"),
            PathBuf::from("a/b")
        );
    }

    #[test]
    fn policy_rejects_files_outside_roots_and_directories() {
        let root = TestDir::new("root");
        let outside = TestDir::new("outside");
        let outside_file = outside.path().join("outside.adoc");
        fs::write(&outside_file, "outside").expect("write outside file");
        let policy = policy(root.path(), 100);

        assert!(matches!(
            policy.canonical_file(&outside_file),
            Err(ResourceError::OutsideRoots(_))
        ));
        assert!(matches!(
            policy.canonical_file(root.path()),
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
            policy(root.path(), 100).canonical_file(&link),
            Err(ResourceError::OutsideRoots(_))
        ));
    }

    #[test]
    fn validated_target_enforces_encoding_and_per_resource_limit() {
        let root = TestDir::new("read");
        let invalid = root.path().join("invalid.adoc");
        let oversized = root.path().join("oversized.adoc");
        fs::write(&invalid, [0xff]).expect("write invalid UTF-8");
        fs::write(&oversized, "1234").expect("write oversized file");
        let policy = policy(root.path(), 3);

        assert!(matches!(
            policy
                .validate_file(&mut ResourceBudget::default(), &invalid)
                .and_then(ValidatedFilesystemTarget::into_loaded_utf8),
            Err(ResourceError::InvalidUtf8 { .. })
        ));
        assert!(matches!(
            policy.validate_file(&mut ResourceBudget::default(), &oversized),
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
        let validated = policy(root.path(), 100)
            .validate_file(&mut ResourceBudget::default(), &candidate)
            .expect("validated target");

        fs::remove_file(&candidate).expect("replace candidate");
        symlink(&outside_file, &candidate).expect("outside symlink");

        let loaded = validated.into_loaded_utf8().expect("captured UTF-8");
        assert_eq!(loaded.source(), "inside");
        assert_eq!(loaded.canonical_path(), candidate);
    }
}
