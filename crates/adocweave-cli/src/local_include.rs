//! Explicit, bounded local resource provider owned by the CLI binary.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::error::Error;
use std::fmt;
use std::path::{Component, Path, PathBuf};

use adocweave::SourceId;
use adocweave::preprocess::{
    PreprocessError, PreprocessOptions, PreprocessedDocument, ResourceDocument, ResourceSnapshot,
    discover_includes, preprocess, resolve_include_target,
};
use adocweave_host::{
    LocalResourcePolicy, LocalTargetPolicy, LocalTargetSession, ResourceBudget, ResourceError,
    ResourceLimits, normalize_relative,
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
    let policy = LocalResourcePolicy::new(roots, ResourceLimits::default())
        .map_err(LocalIncludeError::Host)?;

    let mut snapshot_entries = Vec::new();
    let mut sources = BTreeMap::new();
    let mut source_bases = BTreeMap::new();
    if let Some(source_id) = &source_id {
        sources.insert(source_id.clone(), source.to_owned());
        source_bases.insert(source_id.clone(), base_dir.clone());
    }
    let mut pending = VecDeque::new();
    enqueue(source, Path::new(""), &mut pending)?;
    let mut visited = BTreeSet::new();
    let mut budget = ResourceBudget::default();
    while let Some(target) = pending.pop_front() {
        if !visited.insert(target.clone()) {
            continue;
        }
        let path = base_dir.join(&target);
        let (canonical, text) = policy
            .read_utf8(&mut budget, &path)
            .map_err(LocalIncludeError::Host)?;
        let parent = target.parent().unwrap_or_else(|| Path::new(""));
        enqueue(&text, parent, &mut pending)?;
        let source_id = canonical.to_string_lossy().into_owned();
        sources.insert(source_id.clone(), text.clone());
        source_bases.insert(
            source_id.clone(),
            canonical
                .parent()
                .unwrap_or_else(|| Path::new(""))
                .to_owned(),
        );
        snapshot_entries.push((
            logical_key(&target),
            ResourceDocument {
                source_id: SourceId::new(source_id),
                source: text,
            },
        ));
    }

    let snapshot: ResourceSnapshot = snapshot_entries.into_iter().collect();

    let document = preprocess(
        source,
        &snapshot,
        &PreprocessOptions {
            source_id: source_id.map(SourceId::new),
            ..PreprocessOptions::default()
        },
    )
    .map_err(LocalIncludeError::Preprocess)?;
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
    let mut session = LocalTargetSession::new(policy, 10_000, ResourceLimits::default());
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
    let mut snapshot_entries = Vec::new();
    let mut include_errors = BTreeMap::new();
    let mut pending = VecDeque::new();
    enqueue_local(source, &base_key, &mut pending)?;
    let mut visited = BTreeSet::new();
    while let Some((target, inspect)) = pending.pop_front() {
        if !visited.insert(target.clone()) {
            continue;
        }
        if !inspect {
            include_errors.insert(
                target.clone(),
                adocweave_host::LocalTargetError::Unverifiable(target.clone()),
            );
            snapshot_entries.push((
                target.clone(),
                ResourceDocument {
                    source_id: SourceId::new(format!("<unavailable:{target}>")),
                    source: String::new(),
                },
            ));
            continue;
        }
        match session.read_utf8(&root, &target) {
            Ok((canonical, text)) => {
                let parent = target.rsplit_once('/').map_or("", |(parent, _)| parent);
                enqueue_local(&text, parent, &mut pending)?;
                let resource_id = canonical.to_string_lossy().into_owned();
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
                snapshot_entries.push((
                    target,
                    ResourceDocument {
                        source_id: SourceId::new(resource_id),
                        source: text,
                    },
                ));
            }
            Err(error) => {
                include_errors.insert(target.clone(), error);
                snapshot_entries.push((
                    target.clone(),
                    ResourceDocument {
                        source_id: SourceId::new(format!("<unavailable:{target}>")),
                        source: String::new(),
                    },
                ));
            }
        }
    }
    let snapshot: ResourceSnapshot = snapshot_entries.into_iter().collect();
    let document = preprocess(
        source,
        &snapshot,
        &PreprocessOptions {
            source_id: Some(SourceId::new(source_id)),
            base_uri: (!base_key.is_empty()).then_some(base_key),
            ..PreprocessOptions::default()
        },
    )
    .map_err(LocalIncludeError::Preprocess)?;
    Ok(PreparedInput {
        document,
        sources,
        source_bases,
        include_bases,
        local_session: Some(session),
        include_errors,
    })
}

fn enqueue_local(
    source: &str,
    parent: &str,
    pending: &mut VecDeque<(String, bool)>,
) -> Result<(), LocalIncludeError> {
    for include in discover_includes(source).map_err(LocalIncludeError::Position)? {
        let Some(local) = include.local_target() else {
            continue;
        };
        pending.push_back((
            resolve_include_target(&include.target, Some(parent)),
            local.syntax == adocweave::LocalTargetSyntax::Candidate,
        ));
    }
    Ok(())
}

fn enqueue(
    source: &str,
    parent: &Path,
    pending: &mut VecDeque<PathBuf>,
) -> Result<(), LocalIncludeError> {
    for include in discover_includes(source).map_err(LocalIncludeError::Position)? {
        let relative = normalize_relative(&include.target).map_err(LocalIncludeError::Host)?;
        pending.push_back(parent.join(relative));
    }
    Ok(())
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
