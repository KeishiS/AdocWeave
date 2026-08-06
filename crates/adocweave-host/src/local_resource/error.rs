use std::error::Error;
use std::fmt;
use std::path::PathBuf;

use crate::filesystem_job::FilesystemJobError;
use crate::local_target::LocalTargetError;

/// An error raised while creating, mutating, or committing a filesystem draft.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum FilesystemDraftError {
    SessionRevisionExhausted,
    BindingGenerationExhausted,
    DraftBusy,
    InvalidDraft,
    PoisonedDraft,
    ForeignBinding,
    Job(FilesystemJobError),
    Resource(ResourceError),
}

impl fmt::Display for FilesystemDraftError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::SessionRevisionExhausted => {
                formatter.write_str("filesystem session revision space is exhausted")
            }
            Self::BindingGenerationExhausted => {
                formatter.write_str("filesystem binding generation space is exhausted")
            }
            Self::DraftBusy => {
                formatter.write_str("filesystem session already has an active draft")
            }
            Self::InvalidDraft => {
                formatter.write_str("filesystem draft is stale or belongs to another session")
            }
            Self::PoisonedDraft => {
                formatter.write_str("filesystem draft contains a failed operation")
            }
            Self::ForeignBinding => {
                formatter.write_str("filesystem binding belongs to another session")
            }
            Self::Job(source) => source.fmt(formatter),
            Self::Resource(source) => source.fmt(formatter),
        }
    }
}

impl Error for FilesystemDraftError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Job(source) => Some(source),
            Self::Resource(source) => Some(source),
            _ => None,
        }
    }
}

impl From<FilesystemJobError> for FilesystemDraftError {
    fn from(source: FilesystemJobError) -> Self {
        Self::Job(source)
    }
}

impl From<ResourceError> for FilesystemDraftError {
    fn from(source: ResourceError) -> Self {
        match source {
            ResourceError::Job(source) => Self::Job(source),
            source => Self::Resource(source),
        }
    }
}

impl From<FilesystemDraftError> for ResourceError {
    fn from(error: FilesystemDraftError) -> Self {
        match error {
            FilesystemDraftError::Job(source) => Self::Job(source),
            FilesystemDraftError::Resource(source) => source,
            lifecycle => Self::Unverifiable(lifecycle.to_string()),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ResourceError {
    NoRoots,
    InvalidRoot,
    InvalidSourceId,
    SessionIdentityExhausted,
    InvalidRollback,
    Missing(PathBuf),
    PermissionDenied(PathBuf),
    PathNotAbsolute(PathBuf),
    OutsideRoots(PathBuf),
    NotRegularFile(PathBuf),
    Inspect { path: PathBuf, source: String },
    Read { path: PathBuf, source: String },
    InvalidUtf8 { path: PathBuf, source: String },
    ResourceTooLarge(PathBuf),
    RootLimit { limit: usize },
    FileLimit { limit: usize },
    ScanEntryLimit { limit: usize },
    ByteLimit,
    Job(FilesystemJobError),
    Unverifiable(String),
}

impl fmt::Display for ResourceError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoRoots => formatter.write_str("no local resource roots were configured"),
            Self::InvalidRoot => formatter.write_str("local resource root is not a directory"),
            Self::InvalidSourceId => formatter.write_str("local source ID is invalid"),
            Self::SessionIdentityExhausted => {
                formatter.write_str("filesystem session identity space is exhausted")
            }
            Self::InvalidRollback => formatter
                .write_str("filesystem reread rollback is stale or belongs to another session"),
            Self::Missing(path) => {
                write!(formatter, "local resource is missing: {}", path.display())
            }
            Self::PermissionDenied(path) => {
                write!(formatter, "permission denied reading {}", path.display())
            }
            Self::PathNotAbsolute(path) => write!(
                formatter,
                "local resource path is not absolute: {}",
                path.display()
            ),
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
            Self::RootLimit { limit } => {
                write!(formatter, "local resource root limit exceeded: {limit}")
            }
            Self::FileLimit { limit } => {
                write!(formatter, "local resource file limit exceeded: {limit}")
            }
            Self::ScanEntryLimit { limit } => {
                write!(
                    formatter,
                    "local filesystem scan entry limit exceeded: {limit}"
                )
            }
            Self::ByteLimit => formatter.write_str("local resource byte limit exceeded"),
            Self::Job(source) => source.fmt(formatter),
            Self::Unverifiable(reason) => {
                write!(formatter, "local resource cannot be verified: {reason}")
            }
        }
    }
}

impl Error for ResourceError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Job(source) => Some(source),
            _ => None,
        }
    }
}

impl From<FilesystemJobError> for ResourceError {
    fn from(source: FilesystemJobError) -> Self {
        Self::Job(source)
    }
}

impl From<LocalTargetError> for ResourceError {
    fn from(error: LocalTargetError) -> Self {
        match error {
            LocalTargetError::Missing(path) => Self::Missing(path),
            LocalTargetError::OutsideRoot(path) => Self::OutsideRoots(path),
            LocalTargetError::NotFile(path) | LocalTargetError::NotDirectory(path) => {
                Self::NotRegularFile(path)
            }
            LocalTargetError::PermissionDenied(path) => Self::PermissionDenied(path),
            LocalTargetError::InvalidUtf8(path) => Self::InvalidUtf8 {
                path,
                source: "input is not valid UTF-8".to_owned(),
            },
            LocalTargetError::Unverifiable(source) => Self::Unverifiable(source),
            LocalTargetError::LimitExceeded { limit } => Self::FileLimit { limit },
            LocalTargetError::ResourceTooLarge(path) => Self::ResourceTooLarge(path),
            LocalTargetError::ReadLimitExceeded => Self::ByteLimit,
        }
    }
}
