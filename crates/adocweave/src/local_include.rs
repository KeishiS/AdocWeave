//! Explicit, bounded local resource provider owned by the CLI binary.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::error::Error;
use std::fmt;
use std::fs;
use std::path::{Component, Path, PathBuf};

use adocweave::SourceId;
use adocweave::preprocessor::{
    PreprocessError, PreprocessOptions, PreprocessedDocument, ResourceDocument, ResourceSnapshot,
    discover_includes, preprocess,
};

const MAX_FILES: usize = 10_000;
const MAX_TOTAL_BYTES: u64 = 50 * 1024 * 1024;
const MAX_RESOURCE_BYTES: u64 = 10 * 1024 * 1024;

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
    InvalidTarget(String),
    OutsideRoot(PathBuf),
    Read {
        path: PathBuf,
        source: std::io::Error,
    },
    ResourceTooLarge(PathBuf),
    FileLimit,
    ByteLimit,
    Position(adocweave::source::PositionError),
    Preprocess(PreprocessError),
    Analysis(String),
    MissingSource(String),
}

pub struct PreparedInput {
    pub document: PreprocessedDocument,
    pub sources: BTreeMap<String, String>,
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
            Self::InvalidTarget(target) => write!(formatter, "unsafe include target: {target}"),
            Self::OutsideRoot(path) => {
                write!(
                    formatter,
                    "include target is outside allowed roots: {}",
                    path.display()
                )
            }
            Self::Read { path, source } => {
                write!(
                    formatter,
                    "could not read include {}: {source}",
                    path.display()
                )
            }
            Self::ResourceTooLarge(path) => {
                write!(
                    formatter,
                    "include resource is too large: {}",
                    path.display()
                )
            }
            Self::FileLimit => formatter.write_str("include resource file limit exceeded"),
            Self::ByteLimit => formatter.write_str("include resource byte limit exceeded"),
            Self::Position(error) => error.fmt(formatter),
            Self::Preprocess(error) => error.fmt(formatter),
            Self::Analysis(error) => formatter.write_str(error),
            Self::MissingSource(source_id) => {
                write!(formatter, "projected source is missing: {source_id}")
            }
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

    let mut snapshot = ResourceSnapshot::default();
    let mut sources = BTreeMap::new();
    if let Some(source_id) = &source_id {
        sources.insert(source_id.clone(), source.to_owned());
    }
    let mut pending = VecDeque::new();
    enqueue(source, Path::new(""), &mut pending)?;
    let mut visited = BTreeSet::new();
    let mut total_bytes = 0_u64;
    while let Some(target) = pending.pop_front() {
        if !visited.insert(target.clone()) {
            continue;
        }
        if visited.len() > MAX_FILES {
            return Err(LocalIncludeError::FileLimit);
        }
        let path = base_dir.join(&target);
        let canonical = path
            .canonicalize()
            .map_err(|source| LocalIncludeError::Read {
                path: path.clone(),
                source,
            })?;
        if !roots.iter().any(|root| canonical.starts_with(root)) {
            return Err(LocalIncludeError::OutsideRoot(canonical));
        }
        let metadata = fs::metadata(&canonical).map_err(|source| LocalIncludeError::Read {
            path: canonical.clone(),
            source,
        })?;
        if metadata.len() > MAX_RESOURCE_BYTES {
            return Err(LocalIncludeError::ResourceTooLarge(canonical));
        }
        total_bytes = total_bytes.saturating_add(metadata.len());
        if total_bytes > MAX_TOTAL_BYTES {
            return Err(LocalIncludeError::ByteLimit);
        }
        let text = fs::read_to_string(&canonical).map_err(|source| LocalIncludeError::Read {
            path: canonical.clone(),
            source,
        })?;
        let parent = target.parent().unwrap_or_else(|| Path::new(""));
        enqueue(&text, parent, &mut pending)?;
        let source_id = canonical.to_string_lossy().into_owned();
        sources.insert(source_id.clone(), text.clone());
        snapshot.insert(
            logical_key(&target),
            ResourceDocument {
                source_id: SourceId::new(source_id),
                source: text,
            },
        );
    }

    let document = preprocess(
        source,
        &snapshot,
        &PreprocessOptions {
            source_id: source_id.map(SourceId::new),
            ..PreprocessOptions::default()
        },
    )
    .map_err(LocalIncludeError::Preprocess)?;
    Ok(PreparedInput { document, sources })
}

fn enqueue(
    source: &str,
    parent: &Path,
    pending: &mut VecDeque<PathBuf>,
) -> Result<(), LocalIncludeError> {
    for include in discover_includes(source).map_err(LocalIncludeError::Position)? {
        let relative = safe_relative(&include.target)?;
        pending.push_back(parent.join(relative));
    }
    Ok(())
}

fn safe_relative(target: &str) -> Result<PathBuf, LocalIncludeError> {
    if target.is_empty() || target.contains(':') || target.chars().any(char::is_control) {
        return Err(LocalIncludeError::InvalidTarget(target.to_owned()));
    }
    let path = Path::new(target);
    let mut safe = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Normal(value) => safe.push(value),
            Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                return Err(LocalIncludeError::InvalidTarget(target.to_owned()));
            }
        }
    }
    if safe.as_os_str().is_empty() {
        Err(LocalIncludeError::InvalidTarget(target.to_owned()))
    } else {
        Ok(safe)
    }
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
