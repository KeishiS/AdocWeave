//! Shared filesystem boundary for native AdocWeave hosts.
//!
//! This crate performs local I/O, canonical path validation and byte accounting.
//! It deliberately does not depend on the parser core.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::error::Error;
use std::fmt;
use std::fs;
use std::io::Read;
use std::path::{Component, Path, PathBuf};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DependencyGraph<K: Ord> {
    forward: BTreeMap<K, BTreeSet<K>>,
    reverse: BTreeMap<K, BTreeSet<K>>,
}

impl<K: Ord> Default for DependencyGraph<K> {
    fn default() -> Self {
        Self {
            forward: BTreeMap::new(),
            reverse: BTreeMap::new(),
        }
    }
}

impl<K: Clone + Ord> DependencyGraph<K> {
    pub fn replace(&mut self, key: K, dependencies: BTreeSet<K>) {
        if let Some(previous) = self.forward.remove(&key) {
            for dependency in previous {
                remove_reverse(&mut self.reverse, &dependency, &key);
            }
        }
        for dependency in &dependencies {
            self.reverse
                .entry(dependency.clone())
                .or_default()
                .insert(key.clone());
        }
        self.forward.insert(key, dependencies);
    }

    pub fn remove(&mut self, key: &K) {
        if let Some(previous) = self.forward.remove(key) {
            for dependency in previous {
                remove_reverse(&mut self.reverse, &dependency, key);
            }
        }
    }

    pub fn affected(&self, key: &K) -> BTreeSet<K> {
        closure(key.clone(), |item| self.reverse.get(item))
    }

    pub fn dependencies(&self, key: &K) -> BTreeSet<K> {
        let mut output = closure(key.clone(), |item| self.forward.get(item));
        output.remove(key);
        output
    }
}

fn closure<'a, K: Clone + Ord + 'a>(
    root: K,
    edges: impl Fn(&K) -> Option<&'a BTreeSet<K>>,
) -> BTreeSet<K> {
    let mut found = BTreeSet::from([root.clone()]);
    let mut pending = VecDeque::from([root]);
    while let Some(item) = pending.pop_front() {
        if let Some(next) = edges(&item) {
            for value in next {
                if found.insert(value.clone()) {
                    pending.push_back(value.clone());
                }
            }
        }
    }
    found
}

fn remove_reverse<K: Ord>(reverse: &mut BTreeMap<K, BTreeSet<K>>, dependency: &K, owner: &K) {
    let remove_entry = reverse.get_mut(dependency).is_some_and(|owners| {
        owners.remove(owner);
        owners.is_empty()
    });
    if remove_entry {
        reverse.remove(dependency);
    }
}

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

    pub fn read_utf8(
        &self,
        budget: &mut ResourceBudget,
        path: &Path,
    ) -> Result<(PathBuf, String), ResourceError> {
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
        let text = String::from_utf8(bytes).map_err(|source| ResourceError::InvalidUtf8 {
            path: canonical.clone(),
            source: source.to_string(),
        })?;
        Ok((canonical, text))
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
    fn dependency_graph_updates_forward_and_reverse_closures_atomically() {
        let mut graph = DependencyGraph::default();
        graph.replace("a", BTreeSet::from(["b"]));
        graph.replace("b", BTreeSet::from(["c"]));
        assert_eq!(graph.dependencies(&"a"), BTreeSet::from(["b", "c"]));
        assert_eq!(graph.affected(&"c"), BTreeSet::from(["a", "b", "c"]));

        graph.replace("b", BTreeSet::new());
        assert_eq!(graph.dependencies(&"a"), BTreeSet::from(["b"]));
        assert_eq!(graph.affected(&"c"), BTreeSet::from(["c"]));
    }
}
