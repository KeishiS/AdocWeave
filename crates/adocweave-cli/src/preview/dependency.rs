use std::hash::{DefaultHasher, Hash, Hasher};
use std::path::{Path, PathBuf};

/// Selects the retained filesystem authority used for one preview dependency.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(crate) enum DependencyAuthority {
    Workspace,
    Configuration,
    ExplicitStylesheet,
}

/// Stable logical identity of one file observed by live preview.
///
/// Authority is part of the key because the same path spelling can refer to a
/// different retained namespace for project settings and an explicit option.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(crate) struct Dependency {
    authority: DependencyAuthority,
    path: PathBuf,
}

impl Dependency {
    pub(crate) fn workspace(path: impl Into<PathBuf>) -> Self {
        Self {
            authority: DependencyAuthority::Workspace,
            path: path.into(),
        }
    }

    pub(crate) fn configuration(path: impl Into<PathBuf>) -> Self {
        Self {
            authority: DependencyAuthority::Configuration,
            path: path.into(),
        }
    }

    pub(crate) fn explicit_stylesheet(path: impl Into<PathBuf>) -> Self {
        Self {
            authority: DependencyAuthority::ExplicitStylesheet,
            path: path.into(),
        }
    }

    pub(crate) const fn authority(&self) -> DependencyAuthority {
        self.authority
    }

    pub(crate) fn path(&self) -> &Path {
        &self.path
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Fingerprint {
    state: FingerprintState,
    len: u64,
    content_hash: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum FingerprintState {
    Regular,
    Unavailable,
}

impl Fingerprint {
    /// Captures content which was read through the dependency's authority.
    pub(crate) fn from_loaded_bytes(bytes: &[u8]) -> Self {
        Self {
            state: FingerprintState::Regular,
            len: u64::try_from(bytes.len()).unwrap_or(u64::MAX),
            content_hash: hash(bytes),
        }
    }

    /// Captures a typed read failure without exposing filesystem paths as data.
    pub(crate) fn unavailable(reason: &str) -> Self {
        Self {
            state: FingerprintState::Unavailable,
            len: 0,
            content_hash: hash(reason.as_bytes()),
        }
    }
}

fn hash(value: &[u8]) -> u64 {
    let mut hasher = DefaultHasher::new();
    value.hash(&mut hasher);
    hasher.finish()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn content_fingerprints_detect_same_length_changes() {
        assert_ne!(
            Fingerprint::from_loaded_bytes(b"one"),
            Fingerprint::from_loaded_bytes(b"two")
        );
    }

    #[test]
    fn authority_is_part_of_dependency_identity() {
        let path = PathBuf::from("style.css");
        assert_ne!(
            Dependency::workspace(path.clone()),
            Dependency::configuration(path.clone())
        );
        assert_ne!(
            Dependency::configuration(path.clone()),
            Dependency::explicit_stylesheet(path)
        );
    }

    #[test]
    fn unavailable_reasons_have_stable_distinct_fingerprints() {
        assert_eq!(
            Fingerprint::unavailable("missing"),
            Fingerprint::unavailable("missing")
        );
        assert_ne!(
            Fingerprint::unavailable("missing"),
            Fingerprint::unavailable("permission-denied")
        );
    }
}
