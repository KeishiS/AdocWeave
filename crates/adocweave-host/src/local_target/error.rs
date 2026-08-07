//! Failures reported by the confined local-target boundary.

use std::error::Error;
use std::fmt;
use std::path::PathBuf;

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum LocalTargetError {
    Missing(PathBuf),
    OutsideRoot(PathBuf),
    NotFile(PathBuf),
    NotDirectory(PathBuf),
    PermissionDenied(PathBuf),
    InvalidUtf8(PathBuf),
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
            Self::InvalidUtf8(_) | Self::Unverifiable(_) => "local-target-unverifiable",
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
            Self::InvalidUtf8(path) => {
                write!(
                    formatter,
                    "local target is not valid UTF-8: {}",
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
