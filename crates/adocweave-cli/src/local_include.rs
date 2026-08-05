//! Explicit, bounded local resource provider owned by the CLI binary.

use std::collections::{BTreeMap, BTreeSet};
use std::error::Error;
use std::fmt;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

use adocweave::SourceId;
use adocweave::preprocess::{
    IncludeRequest, PreprocessError, PreprocessErrorKind, PreprocessOptions, PreprocessedDocument,
    ResourceDocument, ResourceSnapshot, preprocess,
};
use adocweave_host::{
    FilesystemReadLimits, LocalFilesystemPolicy, LocalFilesystemSession, LocalTargetError,
    LocalTargetPolicy, LocalTargetSession, LogicalSourceId, ResourceError,
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
    projection: ProjectionInput,
    validation: Option<LocalValidationContext>,
}

pub struct ProjectionInput {
    document: PreprocessedDocument,
    sources: BTreeMap<String, Arc<str>>,
    source_bases: BTreeMap<String, PathBuf>,
    include_bases: BTreeMap<String, PathBuf>,
}

struct ProjectionState {
    sources: BTreeMap<String, Arc<str>>,
    source_bases: BTreeMap<String, PathBuf>,
    include_bases: BTreeMap<String, PathBuf>,
}

pub struct LocalValidationContext {
    session: LocalTargetSession,
    include_errors: BTreeMap<String, LocalTargetError>,
}

pub(crate) trait DependencyObserver {
    fn observe_path(&mut self, path: &Path);
    fn observe_loaded(&mut self, path: &Path, source: &str);
}

struct IgnoreDependencies;

impl DependencyObserver for IgnoreDependencies {
    fn observe_path(&mut self, _: &Path) {}
    fn observe_loaded(&mut self, _: &Path, _: &str) {}
}

impl PreparedInput {
    pub fn projection(&self) -> &ProjectionInput {
        &self.projection
    }

    pub fn validation(&self) -> Option<&LocalValidationContext> {
        self.validation.as_ref()
    }

    pub fn projection_and_validation_mut(
        &mut self,
    ) -> (&ProjectionInput, Option<&mut LocalValidationContext>) {
        (&self.projection, self.validation.as_mut())
    }

    pub(crate) fn resource_sizes(&self) -> impl Iterator<Item = u64> + '_ {
        self.projection.resource_lengths()
    }

    pub(crate) fn resource_entries(&self) -> impl Iterator<Item = (&str, u64)> + '_ {
        self.projection
            .sources
            .iter()
            .map(|(id, source)| (id.as_str(), source.len() as u64))
    }
}

impl ProjectionInput {
    pub fn document(&self) -> &PreprocessedDocument {
        &self.document
    }

    pub fn source(&self, source_id: &str) -> Option<&str> {
        self.sources.get(source_id).map(AsRef::as_ref)
    }

    pub fn source_base(&self, source_id: &str) -> Option<&Path> {
        self.source_bases.get(source_id).map(PathBuf::as_path)
    }

    pub fn include_base(&self, source_id: &str) -> Option<&Path> {
        self.include_bases.get(source_id).map(PathBuf::as_path)
    }

    pub fn resource_lengths(&self) -> impl Iterator<Item = u64> + '_ {
        self.sources
            .values()
            .map(|source| u64::try_from(source.len()).unwrap_or(u64::MAX))
    }
}

impl LocalValidationContext {
    pub fn session_mut(&mut self) -> &mut LocalTargetSession {
        &mut self.session
    }

    pub fn include_error(&self, target: &str) -> Option<&LocalTargetError> {
        self.include_errors.get(target)
    }

    pub(crate) fn include_errors(&self) -> &BTreeMap<String, LocalTargetError> {
        &self.include_errors
    }
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
    source: Arc<str>,
    provenance: LoadedSourceProvenance,
}

struct IncludeLoader<'session> {
    session: &'session mut LocalFilesystemSession,
}

impl<'session> IncludeLoader<'session> {
    fn new(session: &'session mut LocalFilesystemSession) -> Self {
        Self { session }
    }

    fn load(
        &mut self,
        base: &Path,
        request: IncludeRequest,
    ) -> Result<LoadedSource, ResourceError> {
        let source_id = SourceId::new(include_source_id(&request.target));
        let loaded = self.session.read_target_utf8(
            LogicalSourceId::new(source_id.as_str())?,
            base,
            &request.target,
        )?;
        let canonical_path = loaded.canonical_path().to_owned();
        let (_, source) = loaded.into_parts();
        Ok(LoadedSource {
            source_id,
            source,
            provenance: LoadedSourceProvenance {
                logical_target: request.target,
                canonical_path,
            },
        })
    }
}

impl LoadedSource {
    fn into_parts(self) -> (SourceId, Arc<str>, LoadedSourceProvenance) {
        (self.source_id, self.source, self.provenance)
    }
}

fn include_target_error(error: ResourceError) -> LocalTargetError {
    match error {
        ResourceError::Missing(path) => LocalTargetError::Missing(path),
        ResourceError::PermissionDenied(path) => LocalTargetError::PermissionDenied(path),
        ResourceError::OutsideRoots(path) => LocalTargetError::OutsideRoot(path),
        ResourceError::NotRegularFile(path) => LocalTargetError::NotFile(path),
        ResourceError::InvalidUtf8 { path, .. } => LocalTargetError::InvalidUtf8(path),
        ResourceError::ResourceTooLarge(path) => LocalTargetError::ResourceTooLarge(path),
        ResourceError::FileLimit { limit } => LocalTargetError::LimitExceeded { limit },
        ResourceError::ByteLimit => LocalTargetError::ReadLimitExceeded,
        ResourceError::Unverifiable(reason) => LocalTargetError::Unverifiable(reason),
        other => LocalTargetError::Unverifiable(other.to_string()),
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

enum ResolvedInclude {
    Loaded {
        source_id: SourceId,
        source: Arc<str>,
        source_base: PathBuf,
        include_base: Option<PathBuf>,
    },
    Failed {
        source_id: SourceId,
        error: LocalTargetError,
    },
}

fn preprocess_with(
    source: &str,
    preprocess_options: PreprocessOptions,
    mut projection: ProjectionState,
    mut resolve: impl FnMut(&PreprocessError, &str) -> Result<ResolvedInclude, LocalIncludeError>,
) -> Result<(ProjectionInput, BTreeMap<String, LocalTargetError>), LocalIncludeError> {
    let mut snapshot = ResourceSnapshot::default();
    let mut include_errors = BTreeMap::new();
    let document = loop {
        match preprocess(source, &snapshot, &preprocess_options) {
            Ok(document) => break document,
            Err(error) if error.kind == PreprocessErrorKind::MissingResource => {
                let target = error
                    .target
                    .clone()
                    .ok_or_else(|| LocalIncludeError::Preprocess(error.clone()))?;
                match resolve(&error, &target)? {
                    ResolvedInclude::Loaded {
                        source_id,
                        source,
                        source_base,
                        include_base,
                    } => {
                        projection
                            .sources
                            .insert(source_id.as_str().to_owned(), source.clone());
                        projection
                            .source_bases
                            .insert(source_id.as_str().to_owned(), source_base);
                        if let Some(include_base) = include_base {
                            projection
                                .include_bases
                                .insert(source_id.as_str().to_owned(), include_base);
                        }
                        snapshot.insert(target, ResourceDocument { source_id, source });
                    }
                    ResolvedInclude::Failed { source_id, error } => {
                        include_errors.insert(target.clone(), error);
                        snapshot.insert(
                            target,
                            ResourceDocument {
                                source_id,
                                source: String::new().into(),
                            },
                        );
                    }
                }
            }
            Err(error) => return Err(LocalIncludeError::Preprocess(error)),
        }
    };
    Ok((
        ProjectionInput {
            document,
            sources: projection.sources,
            source_bases: projection.source_bases,
            include_bases: projection.include_bases,
        },
        include_errors,
    ))
}

pub fn prepare(
    source: &str,
    source_id: Option<String>,
    base_dir: &Path,
    allowed_roots: &[PathBuf],
    limits: FilesystemReadLimits,
    preprocess_options: &PreprocessOptions,
) -> Result<PreparedInput, LocalIncludeError> {
    let base_dir = base_dir
        .canonicalize()
        .map_err(|source| LocalIncludeError::InvalidBase {
            path: base_dir.to_owned(),
            source,
        })?;
    let allowed_roots = if allowed_roots.is_empty() {
        Vec::new()
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
    let mut session_roots = allowed_roots.clone();
    if !session_roots.contains(&base_dir) {
        session_roots.push(base_dir.clone());
    }
    let policy =
        LocalFilesystemPolicy::new(session_roots, limits).map_err(LocalIncludeError::Host)?;
    let mut filesystem = policy.session().map_err(LocalIncludeError::Host)?;
    prepare_with_session(
        source,
        source_id,
        &base_dir,
        &allowed_roots,
        preprocess_options,
        &mut filesystem,
    )
}

pub(crate) fn prepare_with_session(
    source: &str,
    source_id: Option<String>,
    base_dir: &Path,
    allowed_roots: &[PathBuf],
    preprocess_options: &PreprocessOptions,
    filesystem: &mut LocalFilesystemSession,
) -> Result<PreparedInput, LocalIncludeError> {
    let base_policy = filesystem
        .policy_for_path(base_dir)
        .ok_or_else(|| LocalIncludeError::OutsideRoot(base_dir.to_owned()))?
        .derive_confined_directory(base_dir)
        .map_err(|error| LocalIncludeError::Analysis(format!("invalid include base: {error}")))?;
    let base_dir = base_policy.root().to_owned();
    let allowed_policies = allowed_roots
        .iter()
        .map(|path| {
            filesystem
                .policy_for_path(path)
                .ok_or_else(|| LocalIncludeError::OutsideRoot(path.clone()))?
                .derive_confined_directory(path)
                .map_err(|error| {
                    LocalIncludeError::Analysis(format!("invalid include root: {error}"))
                })
        })
        .collect::<Result<Vec<_>, _>>()?;
    let mut sources = BTreeMap::new();
    let mut source_bases = BTreeMap::new();
    if let Some(source_id) = &source_id {
        sources.insert(source_id.clone(), Arc::from(source));
        source_bases.insert(source_id.clone(), base_dir.clone());
    }
    let mut preprocess_options = preprocess_options.clone();
    preprocess_options.source_id = source_id.clone().map(SourceId::new);
    preprocess_options.enable_includes = true;
    let projection = ProjectionState {
        sources,
        source_bases,
        include_bases: BTreeMap::new(),
    };
    let (projection, include_errors) =
        preprocess_with(source, preprocess_options, projection, |_, target| {
            let candidate = base_dir.join(target);
            let path = if allowed_policies.is_empty() {
                base_policy
                    .normalize_candidate(&candidate)
                    .map_err(|_| LocalIncludeError::OutsideRoot(candidate.clone()))?
            } else {
                allowed_policies
                    .iter()
                    .find_map(|policy| policy.normalize_candidate(&candidate).ok())
                    .ok_or_else(|| LocalIncludeError::OutsideRoot(candidate.clone()))?
            };
            let resource_id = include_source_id(target);
            let loaded = filesystem
                .read_utf8(
                    LogicalSourceId::new(resource_id.clone()).map_err(LocalIncludeError::Host)?,
                    &path,
                )
                .map_err(LocalIncludeError::Host)?;
            let canonical = loaded.canonical_path().to_owned();
            let (loaded_id, text) = loaded.into_parts();
            debug_assert_eq!(loaded_id.as_str(), resource_id);
            Ok(ResolvedInclude::Loaded {
                source_id: SourceId::new(resource_id),
                source: text,
                source_base: canonical
                    .parent()
                    .unwrap_or_else(|| Path::new(""))
                    .to_owned(),
                include_base: None,
            })
        })?;
    debug_assert!(include_errors.is_empty());
    Ok(PreparedInput {
        projection,
        validation: None,
    })
}

pub fn prepare_local(
    source: &str,
    source_id: String,
    base_dir: &Path,
    source_base: &Path,
    project_root: &Path,
    limits: FilesystemReadLimits,
    preprocess_options: &PreprocessOptions,
) -> Result<PreparedInput, LocalIncludeError> {
    prepare_local_tracking(
        source,
        source_id,
        base_dir,
        source_base,
        project_root,
        limits,
        preprocess_options,
        &mut IgnoreDependencies,
    )
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn prepare_local_with_session(
    source: &str,
    source_id: String,
    base_dir: &Path,
    source_base: &Path,
    project_root: &Path,
    preprocess_options: &PreprocessOptions,
    filesystem_session: &mut LocalFilesystemSession,
) -> Result<PreparedInput, LocalIncludeError> {
    prepare_local_tracking_with_existing_session(
        source,
        source_id,
        base_dir,
        source_base,
        project_root,
        preprocess_options,
        &mut IgnoreDependencies,
        filesystem_session,
    )
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn prepare_local_tracking_with_existing_session(
    source: &str,
    source_id: String,
    base_dir: &Path,
    source_base: &Path,
    project_root: &Path,
    preprocess_options: &PreprocessOptions,
    observer: &mut dyn DependencyObserver,
    filesystem_session: &mut LocalFilesystemSession,
) -> Result<PreparedInput, LocalIncludeError> {
    let policy = filesystem_session
        .policy_for_path(project_root)
        .ok_or_else(|| LocalIncludeError::OutsideRoot(project_root.to_owned()))?
        .derive_confined_directory(project_root)
        .map_err(|error| LocalIncludeError::Analysis(format!("invalid project root: {error}")))?;
    let base_dir = policy
        .inspect_directory_no_symlinks(base_dir)
        .map_err(|error| LocalIncludeError::Analysis(format!("invalid include base: {error}")))?;
    let root = policy.root().to_owned();
    prepare_local_tracking_with_session(
        source,
        source_id,
        &base_dir,
        source_base,
        policy,
        &root,
        preprocess_options,
        observer,
        filesystem_session,
    )
}

// Keep this adapter parallel to `prepare_local`; the final argument is an
// out-parameter used by preview to retain dependencies after preprocessing errors.
#[allow(clippy::too_many_arguments)]
pub(crate) fn prepare_local_tracking(
    source: &str,
    source_id: String,
    base_dir: &Path,
    source_base: &Path,
    project_root: &Path,
    limits: FilesystemReadLimits,
    preprocess_options: &PreprocessOptions,
    observer: &mut dyn DependencyObserver,
) -> Result<PreparedInput, LocalIncludeError> {
    let filesystem_policy = LocalFilesystemPolicy::new([project_root.to_owned()], limits)
        .map_err(LocalIncludeError::Host)?;
    let root = filesystem_policy.roots()[0].clone();
    let policy = filesystem_policy
        .root_policy(&root)
        .expect("filesystem policy retains its root")
        .clone();
    let base_dir = policy
        .inspect_directory_no_symlinks(base_dir)
        .map_err(|error| LocalIncludeError::Analysis(format!("invalid include base: {error}")))?;
    let mut filesystem_session = filesystem_policy
        .session()
        .map_err(LocalIncludeError::Host)?;
    prepare_local_tracking_with_session(
        source,
        source_id,
        &base_dir,
        source_base,
        policy,
        &root,
        preprocess_options,
        observer,
        &mut filesystem_session,
    )
}

#[allow(clippy::too_many_arguments)]
fn prepare_local_tracking_with_session(
    source: &str,
    source_id: String,
    base_dir: &Path,
    source_base: &Path,
    policy: LocalTargetPolicy,
    root: &Path,
    preprocess_options: &PreprocessOptions,
    observer: &mut dyn DependencyObserver,
    filesystem_session: &mut LocalFilesystemSession,
) -> Result<PreparedInput, LocalIncludeError> {
    let limits = filesystem_session.limits();
    let base_key = logical_key(
        base_dir
            .strip_prefix(root)
            .expect("base checked below root"),
    );

    let sources = BTreeMap::from([(source_id.clone(), Arc::from(source))]);
    let source_base = policy
        .inspect_directory_no_symlinks(source_base)
        .map_err(|error| LocalIncludeError::Analysis(format!("invalid source base: {error}")))?;
    let session = LocalTargetSession::new(policy, limits.max_files, limits);
    let source_bases = BTreeMap::from([(source_id.clone(), source_base)]);
    let include_bases = BTreeMap::from([(source_id.clone(), base_dir.to_owned())]);
    let mut preprocess_options = preprocess_options.clone();
    preprocess_options.source_id = Some(SourceId::new(source_id.clone()));
    preprocess_options.base_uri = (!base_key.is_empty()).then_some(base_key);
    preprocess_options.enable_includes = true;
    let projection = ProjectionState {
        sources,
        source_bases,
        include_bases,
    };
    let (projection, include_errors) =
        preprocess_with(source, preprocess_options, projection, |error, target| {
            let requested_target = error.requested_target.as_deref().unwrap_or(target);
            let resource_id = include_source_id(target);
            let inspect = adocweave::LocalTargetReference::from_include(
                error.range,
                error.range,
                requested_target,
            )
            .is_some_and(|reference| reference.syntax == adocweave::LocalTargetSyntax::Candidate);
            if !inspect {
                return Ok(ResolvedInclude::Failed {
                    source_id: SourceId::new(resource_id),
                    error: LocalTargetError::Unverifiable(target.to_owned()),
                });
            }
            let request = IncludeRequest {
                range: error.range,
                target_range: error.range,
                target: target.to_owned(),
                attributes: String::new(),
            };
            let candidates = dependency_candidates(root, target);
            for candidate in &candidates {
                observer.observe_path(candidate);
            }
            let loaded = {
                IncludeLoader::new(filesystem_session)
                    .load(root, request)
                    .map_err(include_target_error)
            };
            match loaded {
                Ok(loaded) => {
                    let (loaded_source_id, text, provenance) = loaded.into_parts();
                    debug_assert_eq!(loaded_source_id.as_str(), resource_id);
                    debug_assert_eq!(provenance.logical_target, target);
                    observer.observe_loaded(&provenance.canonical_path, &text);
                    let base = provenance
                        .canonical_path
                        .parent()
                        .unwrap_or_else(|| Path::new(""))
                        .to_owned();
                    Ok(ResolvedInclude::Loaded {
                        source_id: loaded_source_id,
                        source: text,
                        source_base: base.clone(),
                        include_base: Some(base),
                    })
                }
                Err(error) => Ok(ResolvedInclude::Failed {
                    source_id: SourceId::new(resource_id),
                    error,
                }),
            }
        })?;
    Ok(PreparedInput {
        projection,
        validation: Some(LocalValidationContext {
            session,
            include_errors,
        }),
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

    fn session(root: &Path) -> LocalFilesystemSession {
        LocalFilesystemPolicy::new([root.to_owned()], FilesystemReadLimits::default())
            .and_then(|policy| policy.session())
            .expect("session")
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
                loader
                    .load(&root.0, request("part.adoc"))
                    .expect("loaded source")
            };
            let (source_id, source, provenance) = loaded.into_parts();
            assert_eq!(source_id.as_str(), "include:part.adoc");
            assert_eq!(source.as_ref(), "part\n");
            assert_eq!(provenance.logical_target, "part.adoc");
        }
        assert_eq!(session.budget().files(), 1);
    }

    #[test]
    fn failed_common_read_cannot_produce_a_loaded_source() {
        let root = TestDirectory::new();
        let mut session = session(&root.0);
        let result = IncludeLoader::new(&mut session).load(&root.0, request("missing.adoc"));

        assert!(result.is_err());
        assert_eq!(session.budget().files(), 0);
    }

    #[test]
    fn missing_dependency_candidates_stay_inside_root() {
        let root = TestDirectory::new();
        assert_eq!(
            dependency_candidates(&root.0, "chapters/new.adoc"),
            BTreeSet::from([
                root.0.clone(),
                root.0.join("chapters"),
                root.0.join("chapters/new.adoc")
            ])
        );
        assert!(dependency_candidates(&root.0, "../secret.adoc").is_empty());
        assert!(dependency_candidates(&root.0, "/etc/passwd").is_empty());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn existing_session_keeps_local_validation_namespace_after_root_replacement() {
        let parent = TestDirectory::new();
        let root = parent.0.join("workspace");
        fs::create_dir(&root).expect("workspace");
        fs::write(root.join("root.adoc"), "image::asset.png[]\n").expect("document");
        let mut filesystem = session(&root);
        let displaced = parent.0.join("retained-workspace");
        fs::rename(&root, &displaced).expect("displace workspace");
        fs::create_dir(&root).expect("replacement workspace");
        fs::write(root.join("asset.png"), "outside").expect("replacement target");

        let mut prepared = prepare_local_with_session(
            "image::asset.png[]\n",
            root.join("root.adoc").to_string_lossy().into_owned(),
            &root,
            &root,
            &root,
            &PreprocessOptions::default(),
            &mut filesystem,
        )
        .expect("prepared input");
        let error = prepared
            .validation
            .as_mut()
            .expect("validation context")
            .session_mut()
            .inspect(&root, "asset.png")
            .expect_err("replacement target must remain outside the retained namespace");
        assert!(matches!(error, LocalTargetError::Missing(_)));
        fs::remove_dir_all(&root).expect("remove replacement workspace");
        fs::rename(displaced, &root).expect("restore workspace");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn existing_session_does_not_expand_an_allowed_root_after_replacement() {
        use std::os::unix::fs::symlink;

        let parent = TestDirectory::new();
        let workspace = parent.0.join("workspace");
        let allowed = workspace.join("public");
        fs::create_dir_all(&allowed).expect("allowed root");
        fs::write(workspace.join("secret.adoc"), "secret\n").expect("secret");
        let mut filesystem = LocalFilesystemPolicy::new(
            [workspace.clone(), allowed.clone()],
            FilesystemReadLimits::default(),
        )
        .and_then(|policy| policy.session())
        .expect("session");
        let retained = workspace.join("retained-public");
        fs::rename(&allowed, &retained).expect("retain original allowed root");
        symlink(&workspace, &allowed).expect("replace allowed root");

        let result = prepare_with_session(
            "include::secret.adoc[]\n",
            Some(workspace.join("root.adoc").to_string_lossy().into_owned()),
            &workspace,
            std::slice::from_ref(&allowed),
            &PreprocessOptions::default(),
            &mut filesystem,
        );
        let Err(error) = result else {
            panic!("replacement must not broaden the retained allowed root");
        };
        assert!(matches!(error, LocalIncludeError::OutsideRoot(_)));
    }

    #[test]
    fn common_file_limit_keeps_the_configured_limit_in_cli_diagnostics() {
        assert_eq!(
            include_target_error(ResourceError::FileLimit { limit: 7 }),
            LocalTargetError::LimitExceeded { limit: 7 }
        );
    }

    #[test]
    fn common_driver_preserves_strategy_specific_context() {
        let root = TestDirectory::new();
        fs::write(root.0.join("part.adoc"), "part\n").expect("fixture");
        let source = "include::part.adoc[]\n";
        let source_id = root.0.join("root.adoc").to_string_lossy().into_owned();

        let regular = prepare(
            source,
            Some(source_id.clone()),
            &root.0,
            &[],
            FilesystemReadLimits::default(),
            &PreprocessOptions::default(),
        )
        .expect("regular preparation");
        #[derive(Default)]
        struct RecordingObserver {
            paths: BTreeSet<PathBuf>,
            loaded_lengths: Vec<usize>,
        }
        impl DependencyObserver for RecordingObserver {
            fn observe_path(&mut self, path: &Path) {
                self.paths.insert(path.to_owned());
            }

            fn observe_loaded(&mut self, path: &Path, source: &str) {
                self.paths.insert(path.to_owned());
                self.loaded_lengths.push(source.len());
            }
        }

        let mut observer = RecordingObserver::default();
        let local = prepare_local_tracking(
            source,
            source_id,
            &root.0,
            &root.0,
            &root.0,
            FilesystemReadLimits::default(),
            &PreprocessOptions::default(),
            &mut observer,
        )
        .expect("local preparation");

        assert_eq!(
            regular.projection().document().source,
            local.projection().document().source
        );
        assert!(regular.validation.is_none());
        assert!(local.validation.is_some());
        assert_eq!(observer.loaded_lengths, [5]);
        assert!(observer.paths.contains(&root.0.join("part.adoc")));
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
            loader
                .load(&root.0, request("part.adoc"))
                .expect("direct load")
        };
        let alias = {
            let mut loader = IncludeLoader::new(&mut session);
            loader
                .load(&root.0, request("alias.adoc"))
                .expect("alias load")
        };
        let (direct_id, _, direct_provenance) = direct.into_parts();
        let (alias_id, _, alias_provenance) = alias.into_parts();

        assert_eq!(direct_id.as_str(), "include:part.adoc");
        assert_eq!(alias_id.as_str(), "include:alias.adoc");
        assert_ne!(direct_id, alias_id);
        assert_eq!(
            direct_provenance.canonical_path,
            alias_provenance.canonical_path
        );
        assert_eq!(session.budget().files(), 1);
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
            .load(&root.0, request("escape.adoc"))
            .expect_err("symlink escape");

        assert!(matches!(error, ResourceError::OutsideRoots(_)));
        assert_eq!(session.budget().files(), 0);
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
        assert!(dependencies.contains(&root.0.join("current/part.adoc")));
    }
}
