//! Explicit, bounded local resource provider owned by the CLI binary.

use std::collections::BTreeMap;
use std::error::Error;
use std::fmt;
use std::path::{Component, Path, PathBuf};

use adocweave::SourceId;
use adocweave::preprocess::{
    PreprocessError, PreprocessErrorKind, PreprocessOptions, PreprocessedDocument,
    ResourceDocument, ResourceSnapshot, preprocess,
};
use adocweave_host::{
    LocalResourcePolicy, LocalTargetPolicy, LocalTargetSession, ResourceBudget, ResourceError,
    ResourceLimits,
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
                match session.read_utf8(&root, &target) {
                    Ok(loaded) => {
                        let (canonical, text) = loaded.into_parts();
                        sources.insert(resource_id.clone(), text.clone());
                        source_bases.insert(
                            resource_id.clone(),
                            canonical
                                .parent()
                                .unwrap_or_else(|| Path::new(""))
                                .to_owned(),
                        );
                        include_bases.insert(
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
    })
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
