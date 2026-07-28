//! Explicit, bounded local resource provider owned by the CLI binary.

use std::collections::{BTreeMap, BTreeSet};
use std::error::Error;
use std::fmt;
use std::path::{Component, Path, PathBuf};

use crate::preview::Fingerprint;
use adocweave::SourceId;
use adocweave::preprocess::{
    IncludeRequest, PreprocessError, PreprocessErrorKind, PreprocessOptions, PreprocessedDocument,
    ResourceDocument, ResourceSnapshot, preprocess,
};
use adocweave_host::{
    LoadedLocalTarget, LocalResourcePolicy, LocalTargetError, LocalTargetPolicy,
    LocalTargetSession, ResourceBudget, ResourceError, ResourceLimits,
};

#[derive(Debug)]
pub enum LocalIncludeError {
    InvalidBase {
        path: PathBuf,
        source: std::io::Error,
    },
    InvalidRoot {
        path: PathBuf,
        source: std::io::Error,
    },
    OutsideRoot(PathBuf),
    Position(adocweave::text::PositionError),
    Preprocess(PreprocessError),
    Analysis(String),
    MissingSource(String),
    Host(ResourceError),
}

pub struct PreparedInput {
    pub document: PreprocessedDocument,
    pub sources: BTreeMap<String, String>,
    pub source_bases: BTreeMap<String, PathBuf>,
    pub include_bases: BTreeMap<String, PathBuf>,
    pub local_session: Option<LocalTargetSession>,
    pub include_errors: BTreeMap<String, adocweave_host::LocalTargetError>,
    /// Validated filesystem paths whose state can affect this document.
    pub dependency_paths: BTreeSet<PathBuf>,
    /// Snapshots captured immediately after each successful dependency load.
    pub dependency_snapshots: BTreeMap<PathBuf, Fingerprint>,
}

/// Include target accepted by the filesystem policy but not yet loaded.
///
/// Construction is private so callers cannot attach an unchecked path to a
/// logical source identity.
#[derive(Clone, Debug, Eq, PartialEq)]
struct ValidatedFilesystemTarget {
    request: IncludeRequest,
    source_id: SourceId,
    canonical_path: PathBuf,
    base: PathBuf,
}

/// Filesystem provenance retained outside diagnostics and source maps.
#[derive(Clone, Debug, Eq, PartialEq)]
struct LoadedSourceProvenance {
    logical_target: String,
    canonical_path: PathBuf,
}

/// Bytes loaded only after target validation.
///
/// Its fields are private so a source identity cannot be combined with bytes
/// from another filesystem target.
#[derive(Clone, Debug, Eq, PartialEq)]
struct LoadedSource {
    source_id: SourceId,
    bytes: Vec<u8>,
    provenance: LoadedSourceProvenance,
}

struct IncludeLoader<'session> {
    session: &'session mut LocalTargetSession,
}

impl<'session> IncludeLoader<'session> {
    fn new(session: &'session mut LocalTargetSession) -> Self {
        Self { session }
    }

    fn validate(
        &mut self,
        base: &Path,
        request: IncludeRequest,
    ) -> Result<ValidatedFilesystemTarget, LocalTargetError> {
        let canonical_path = self.session.inspect(base, &request.target)?;
        let source_id = SourceId::new(include_source_id(&request.target));
        Ok(ValidatedFilesystemTarget {
            request,
            source_id,
            canonical_path,
            base: base.to_owned(),
        })
    }

    fn load(
        &mut self,
        target: ValidatedFilesystemTarget,
    ) -> Result<LoadedSource, LocalTargetError> {
        let loaded = self
            .session
            .read_utf8(&target.base, &target.request.target)?;
        if loaded.canonical_path() != target.canonical_path {
            return Err(LocalTargetError::Unverifiable(
                "validated include target changed before read".to_owned(),
            ));
        }
        Ok(loaded_source(target, loaded))
    }
}

fn loaded_source(target: ValidatedFilesystemTarget, loaded: LoadedLocalTarget) -> LoadedSource {
    LoadedSource {
        source_id: target.source_id,
        bytes: loaded.source().as_bytes().to_vec(),
        provenance: LoadedSourceProvenance {
            logical_target: target.request.target,
            canonical_path: target.canonical_path,
        },
    }
}

impl LoadedSource {
    fn into_utf8_parts(self) -> (SourceId, String, LoadedSourceProvenance) {
        // LocalTargetSession has already validated UTF-8. Keeping bytes in this
        // boundary prevents a loaded result from being confused with source text
        // before provenance is attached.
        let source = String::from_utf8(self.bytes).expect("host returned validated UTF-8");
        (self.source_id, source, self.provenance)
    }
}

impl fmt::Display for LocalIncludeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidBase { path, source } => {
                write!(
                    formatter,
                    "invalid include base {}: {source}",
                    path.display()
                )
            }
            Self::InvalidRoot { path, source } => {
                write!(
                    formatter,
                    "invalid include root {}: {source}",
                    path.display()
                )
            }
            Self::OutsideRoot(path) => {
                write!(
                    formatter,
                    "include target is outside allowed roots: {}",
                    path.display()
                )
            }
            Self::Position(error) => error.fmt(formatter),
            Self::Preprocess(error) => error.fmt(formatter),
            Self::Analysis(error) => formatter.write_str(error),
            Self::MissingSource(source_id) => {
                write!(formatter, "projected source is missing: {source_id}")
            }
            Self::Host(error) => error.fmt(formatter),
        }
    }
}

impl Error for LocalIncludeError {}

pub fn prepare(
    source: &str,
    source_id: Option<String>,
    base_dir: &Path,
    allowed_roots: &[PathBuf],
    limits: ResourceLimits,
    preprocess_options: &PreprocessOptions,
) -> Result<PreparedInput, LocalIncludeError> {
    let base_dir = base_dir
        .canonicalize()
        .map_err(|source| LocalIncludeError::InvalidBase {
            path: base_dir.to_owned(),
            source,
        })?;
    let roots = if allowed_roots.is_empty() {
        vec![base_dir.clone()]
    } else {
        allowed_roots
            .iter()
            .map(|path| {
                path.canonicalize()
                    .map_err(|source| LocalIncludeError::InvalidRoot {
                        path: path.clone(),
                        source,
                    })
            })
            .collect::<Result<Vec<_>, _>>()?
    };
    if !roots.iter().any(|root| base_dir.starts_with(root)) {
        return Err(LocalIncludeError::OutsideRoot(base_dir));
    }
    let policy = LocalResourcePolicy::new(roots, limits).map_err(LocalIncludeError::Host)?;

    let mut snapshot = ResourceSnapshot::default();
    let mut sources = BTreeMap::new();
    let mut source_bases = BTreeMap::new();
    let mut dependency_paths = BTreeSet::new();
    let mut dependency_snapshots = BTreeMap::new();
    if let Some(source_id) = &source_id {
        sources.insert(source_id.clone(), source.to_owned());
        source_bases.insert(source_id.clone(), base_dir.clone());
    }
    let mut budget = ResourceBudget::default();
    let mut preprocess_options = preprocess_options.clone();
    preprocess_options.source_id = source_id.clone().map(SourceId::new);
    preprocess_options.enable_includes = true;
    let document = loop {
        match preprocess(source, &snapshot, &preprocess_options) {
            Ok(document) => break document,
            Err(error) if error.kind == PreprocessErrorKind::MissingResource => {
                let target = error
                    .target
                    .clone()
                    .ok_or_else(|| LocalIncludeError::Preprocess(error.clone()))?;
                let path = base_dir.join(&target);
                let loaded = policy
                    .validate_file(&mut budget, &path)
                    .and_then(adocweave_host::ValidatedFilesystemTarget::into_loaded_utf8)
                    .map_err(LocalIncludeError::Host)?;
                let (canonical, text) = loaded.into_parts();
                dependency_paths.insert(canonical.clone());
                dependency_snapshots.insert(
                    canonical.clone(),
                    Fingerprint::from_loaded_bytes(&canonical, text.as_bytes()),
                );
                let resource_id = include_source_id(&target);
                sources.insert(resource_id.clone(), text.clone());
                source_bases.insert(
                    resource_id.clone(),
                    canonical
                        .parent()
                        .unwrap_or_else(|| Path::new(""))
                        .to_owned(),
                );
                snapshot.insert(
                    target,
                    ResourceDocument {
                        source_id: SourceId::new(resource_id),
                        source: text.into(),
                    },
                );
            }
            Err(error) => return Err(LocalIncludeError::Preprocess(error)),
        }
    };
    Ok(PreparedInput {
        document,
        sources,
        source_bases,
        include_bases: BTreeMap::new(),
        local_session: None,
        include_errors: BTreeMap::new(),
        dependency_paths,
        dependency_snapshots,
    })
}

pub fn prepare_local(
    source: &str,
    source_id: String,
    base_dir: &Path,
    source_base: &Path,
    project_root: &Path,
    limits: ResourceLimits,
    preprocess_options: &PreprocessOptions,
) -> Result<PreparedInput, LocalIncludeError> {
    let base_dir = base_dir
        .canonicalize()
        .map_err(|source| LocalIncludeError::InvalidBase {
            path: base_dir.to_owned(),
            source,
        })?;
    let policy = LocalTargetPolicy::new(project_root)
        .map_err(|error| LocalIncludeError::Analysis(format!("invalid project root: {error}")))?;
    if !base_dir.starts_with(policy.root()) {
        return Err(LocalIncludeError::OutsideRoot(base_dir));
    }
    let root = policy.root().to_owned();
    let mut session = LocalTargetSession::new(policy, limits.max_files, limits);
    let base_key = logical_key(
        base_dir
            .strip_prefix(&root)
            .expect("base checked below root"),
    );

    let mut sources = BTreeMap::from([(source_id.clone(), source.to_owned())]);
    let source_base =
        source_base
            .canonicalize()
            .map_err(|source| LocalIncludeError::InvalidBase {
                path: source_base.to_owned(),
                source,
            })?;
    let mut source_bases = BTreeMap::from([(source_id.clone(), source_base)]);
    let mut include_bases = BTreeMap::from([(source_id.clone(), base_dir.clone())]);
    let mut snapshot = ResourceSnapshot::default();
    let mut include_errors = BTreeMap::new();
    let mut dependency_paths = BTreeSet::new();
    let mut dependency_snapshots = BTreeMap::new();
    let mut preprocess_options = preprocess_options.clone();
    preprocess_options.source_id = Some(SourceId::new(source_id.clone()));
    preprocess_options.base_uri = (!base_key.is_empty()).then_some(base_key);
    preprocess_options.enable_includes = true;
    let document = loop {
        match preprocess(source, &snapshot, &preprocess_options) {
            Ok(document) => break document,
            Err(error) if error.kind == PreprocessErrorKind::MissingResource => {
                let target = error
                    .target
                    .clone()
                    .ok_or_else(|| LocalIncludeError::Preprocess(error.clone()))?;
                let requested_target = error.requested_target.as_deref().unwrap_or(target.as_str());
                let resource_id = include_source_id(&target);
                let inspect = adocweave::LocalTargetReference::from_include(
                    error.range,
                    error.range,
                    requested_target,
                )
                .is_some_and(|reference| {
                    reference.syntax == adocweave::LocalTargetSyntax::Candidate
                });
                if !inspect {
                    include_errors.insert(
                        target.clone(),
                        adocweave_host::LocalTargetError::Unverifiable(target.clone()),
                    );
                    snapshot.insert(
                        target,
                        ResourceDocument {
                            source_id: SourceId::new(resource_id),
                            source: String::new().into(),
                        },
                    );
                    continue;
                }
                let request = IncludeRequest {
                    range: error.range,
                    target_range: error.range,
                    target: target.clone(),
                    attributes: String::new(),
                };
                let candidates = dependency_candidates(&root, &target);
                let loaded = {
                    let mut loader = IncludeLoader::new(&mut session);
                    loader
                        .validate(&root, request)
                        .and_then(|validated| loader.load(validated))
                };
                dependency_paths.extend(candidates);
                match loaded {
                    Ok(loaded) => {
                        let (loaded_source_id, text, provenance) = loaded.into_utf8_parts();
                        debug_assert_eq!(loaded_source_id.as_str(), resource_id);
                        debug_assert_eq!(provenance.logical_target, target);
                        dependency_paths.insert(provenance.canonical_path.clone());
                        dependency_snapshots.insert(
                            provenance.canonical_path.clone(),
                            Fingerprint::from_loaded_bytes(
                                &provenance.canonical_path,
                                text.as_bytes(),
                            ),
                        );
                        sources.insert(loaded_source_id.as_str().to_owned(), text.clone());
                        source_bases.insert(
                            loaded_source_id.as_str().to_owned(),
                            provenance
                                .canonical_path
                                .parent()
                                .unwrap_or_else(|| Path::new(""))
                                .to_owned(),
                        );
                        include_bases.insert(
                            loaded_source_id.as_str().to_owned(),
                            provenance
                                .canonical_path
                                .parent()
                                .unwrap_or_else(|| Path::new(""))
                                .to_owned(),
                        );
                        snapshot.insert(
                            target,
                            ResourceDocument {
                                source_id: loaded_source_id,
                                source: text.into(),
                            },
                        );
                    }
                    Err(read_error) => {
                        include_errors.insert(target.clone(), read_error);
                        snapshot.insert(
                            target,
                            ResourceDocument {
                                source_id: SourceId::new(resource_id),
                                source: String::new().into(),
                            },
                        );
                    }
                }
            }
            Err(error) => return Err(LocalIncludeError::Preprocess(error)),
        }
    };
    Ok(PreparedInput {
        document,
        sources,
        source_bases,
        include_bases,
        local_session: Some(session),
        include_errors,
        dependency_paths,
        dependency_snapshots,
    })
}

/// Returns the nearest existing canonical in-root path for monitoring a
/// resource which may not exist yet. Watching the ancestor detects creation
/// without following an unchecked missing path through a replaceable symlink.
fn dependency_candidates(root: &Path, target: &str) -> BTreeSet<PathBuf> {
    let target = Path::new(target);
    if target.is_absolute()
        || target.components().any(|component| {
            matches!(
                component,
                Component::ParentDir | Component::RootDir | Component::Prefix(_)
            )
        })
    {
        return BTreeSet::new();
    }
    let mut paths = BTreeSet::from([root.to_owned()]);
    let mut current = root.to_owned();
    for component in target.components() {
        let Component::Normal(component) = component else {
            break;
        };
        current.push(component);
        paths.insert(current.clone());
        match current.symlink_metadata() {
            Ok(metadata) if metadata.file_type().is_symlink() => break,
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => break,
            Err(_) => break,
        }
    }
    paths
}

fn logical_key(path: &Path) -> String {
    path.components()
        .filter_map(|component| match component {
            Component::Normal(value) => Some(value.to_string_lossy()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("/")
}

fn include_source_id(logical_target: &str) -> String {
    format!("include:{logical_target}")
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::*;

    static NEXT_DIRECTORY: AtomicU64 = AtomicU64::new(0);

    struct TestDirectory(PathBuf);

    impl TestDirectory {
        fn new() -> Self {
            let id = NEXT_DIRECTORY.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "adocweave-include-loader-{}-{id}",
                std::process::id()
            ));
            fs::create_dir_all(&path).expect("test directory");
            Self(path)
        }
    }

    impl Drop for TestDirectory {
        fn drop(&mut self) {
            fs::remove_dir_all(&self.0).expect("test directory cleanup");
        }
    }

    fn session(root: &Path) -> LocalTargetSession {
        LocalTargetSession::new(
            LocalTargetPolicy::new(root).expect("policy"),
            16,
            ResourceLimits::default(),
        )
    }

    fn request(target: &str) -> IncludeRequest {
        adocweave::preprocess::discover_includes(&format!("include::{target}[]\n"))
            .expect("include discovery")
            .pop()
            .expect("include request")
    }

    #[test]
    fn duplicate_requests_keep_logical_identity_and_share_one_read() {
        let root = TestDirectory::new();
        fs::write(root.0.join("part.adoc"), "part\n").expect("fixture");
        let mut session = session(&root.0);

        for _ in 0..2 {
            let loaded = {
                let mut loader = IncludeLoader::new(&mut session);
                let validated = loader
                    .validate(&root.0, request("part.adoc"))
                    .expect("validated target");
                loader.load(validated).expect("loaded source")
            };
            let (source_id, source, provenance) = loaded.into_utf8_parts();
            assert_eq!(source_id.as_str(), "include:part.adoc");
            assert_eq!(source, "part\n");
            assert_eq!(provenance.logical_target, "part.adoc");
        }
        assert_eq!(session.read_files(), 1);
    }

    #[test]
    fn failed_validation_cannot_produce_a_loaded_source() {
        let root = TestDirectory::new();
        let mut session = session(&root.0);
        let result = IncludeLoader::new(&mut session).validate(&root.0, request("missing.adoc"));

        assert!(result.is_err());
        assert_eq!(session.read_files(), 0);
    }

    #[test]
    fn missing_dependency_candidates_stay_inside_root() {
        let root = TestDirectory::new();
        assert_eq!(
            dependency_candidates(&root.0, "chapters/new.adoc"),
            BTreeSet::from([root.0.clone(), root.0.join("chapters")])
        );
        assert!(dependency_candidates(&root.0, "../secret.adoc").is_empty());
        assert!(dependency_candidates(&root.0, "/etc/passwd").is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn aliases_keep_distinct_logical_ids_and_share_canonical_provenance() {
        use std::os::unix::fs::symlink;

        let root = TestDirectory::new();
        fs::write(root.0.join("part.adoc"), "part\n").expect("fixture");
        symlink("part.adoc", root.0.join("alias.adoc")).expect("alias");
        let mut session = session(&root.0);

        let direct = {
            let mut loader = IncludeLoader::new(&mut session);
            let target = loader
                .validate(&root.0, request("part.adoc"))
                .expect("direct validation");
            loader.load(target).expect("direct load")
        };
        let alias = {
            let mut loader = IncludeLoader::new(&mut session);
            let target = loader
                .validate(&root.0, request("alias.adoc"))
                .expect("alias validation");
            loader.load(target).expect("alias load")
        };
        let (direct_id, _, direct_provenance) = direct.into_utf8_parts();
        let (alias_id, _, alias_provenance) = alias.into_utf8_parts();

        assert_eq!(direct_id.as_str(), "include:part.adoc");
        assert_eq!(alias_id.as_str(), "include:alias.adoc");
        assert_ne!(direct_id, alias_id);
        assert_eq!(
            direct_provenance.canonical_path,
            alias_provenance.canonical_path
        );
        assert_eq!(session.read_files(), 1);
    }

    #[cfg(unix)]
    #[test]
    fn symlink_escape_fails_before_loaded_source_construction() {
        use std::os::unix::fs::symlink;

        let root = TestDirectory::new();
        let outside = TestDirectory::new();
        fs::write(outside.0.join("outside.adoc"), "outside\n").expect("outside fixture");
        symlink(outside.0.join("outside.adoc"), root.0.join("escape.adoc")).expect("escape");
        let mut session = session(&root.0);
        let error = IncludeLoader::new(&mut session)
            .validate(&root.0, request("escape.adoc"))
            .expect_err("symlink escape");

        assert_eq!(error.diagnostic_code(), "local-target-outside-root");
        assert_eq!(session.read_files(), 0);
    }

    #[cfg(unix)]
    #[test]
    fn logical_ancestor_symlink_is_retained_separately_from_target() {
        use std::os::unix::fs::symlink;

        let root = TestDirectory::new();
        fs::create_dir(root.0.join("dir-a")).expect("dir a");
        symlink("dir-a", root.0.join("current")).expect("logical symlink");
        let dependencies = dependency_candidates(&root.0, "current/part.adoc");
        assert!(dependencies.contains(&root.0));
        assert!(dependencies.contains(&root.0.join("current")));
        assert!(!dependencies.contains(&root.0.join("current/part.adoc")));
    }
}
