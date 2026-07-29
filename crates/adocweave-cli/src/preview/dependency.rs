use std::fs::{self, File};
use std::hash::{DefaultHasher, Hash, Hasher};
use std::io::{self, Read};
use std::path::Path;
use std::time::SystemTime;

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Fingerprint {
    modified: Option<SystemTime>,
    len: u64,
    exists: bool,
    kind: FileKind,
    content_hash: Option<u64>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum FileKind {
    Missing,
    Regular,
    Symlink,
    Directory,
    Other,
    Unreadable,
}

impl Fingerprint {
    pub(crate) fn read(path: &Path) -> Self {
        match fs::symlink_metadata(path) {
            Ok(metadata) if metadata.file_type().is_symlink() => Self {
                modified: metadata.modified().ok(),
                len: metadata.len(),
                exists: true,
                kind: FileKind::Symlink,
                content_hash: fs::read_link(path).ok().map(|target| hash(&target)),
            },
            Ok(metadata) if metadata.is_dir() => Self {
                modified: metadata.modified().ok(),
                len: metadata.len(),
                exists: true,
                kind: FileKind::Directory,
                content_hash: None,
            },
            Ok(metadata) if metadata.is_file() => match File::open(path) {
                Ok(mut file) => {
                    let opened = file.metadata().unwrap_or(metadata);
                    let mut bytes = Vec::new();
                    let content_hash = file.read_to_end(&mut bytes).ok().map(|_| hash(&bytes));
                    Self {
                        modified: opened.modified().ok(),
                        len: opened.len(),
                        exists: true,
                        kind: if content_hash.is_some() {
                            FileKind::Regular
                        } else {
                            FileKind::Unreadable
                        },
                        content_hash,
                    }
                }
                Err(_) => Self {
                    modified: metadata.modified().ok(),
                    len: metadata.len(),
                    exists: true,
                    kind: FileKind::Unreadable,
                    content_hash: None,
                },
            },
            Ok(metadata) => Self {
                modified: metadata.modified().ok(),
                len: metadata.len(),
                exists: true,
                kind: FileKind::Other,
                content_hash: None,
            },
            Err(_) => Self {
                modified: None,
                len: 0,
                exists: false,
                kind: FileKind::Missing,
                content_hash: None,
            },
        }
    }

    fn from_open_file(file: &File, bytes: &[u8]) -> io::Result<Self> {
        let metadata = file.metadata()?;
        Ok(Self {
            modified: metadata.modified().ok(),
            len: metadata.len(),
            exists: true,
            kind: FileKind::Regular,
            content_hash: Some(hash(&bytes)),
        })
    }

    pub(crate) fn from_loaded_bytes(path: &Path, bytes: &[u8]) -> Self {
        let metadata = fs::symlink_metadata(path)
            .ok()
            .filter(|metadata| metadata.is_file() && !metadata.file_type().is_symlink());
        Self {
            modified: metadata
                .as_ref()
                .and_then(|metadata| metadata.modified().ok()),
            len: u64::try_from(bytes.len()).unwrap_or(u64::MAX),
            exists: true,
            kind: FileKind::Regular,
            content_hash: Some(hash(&bytes)),
        }
    }

    /// Checks inexpensive filesystem metadata before reading file contents.
    ///
    /// A forced check still compares the content hash so edits which preserve
    /// both length and modification time are eventually detected.
    pub(crate) fn changed(&mut self, path: &Path, force_hash: bool) -> bool {
        self.changed_with(path, force_hash, || Self::read(path))
    }

    fn changed_with(&mut self, path: &Path, force_hash: bool, read: impl FnOnce() -> Self) -> bool {
        let observed = Self::metadata(path);
        if observed == self.metadata_fields() && !force_hash {
            return false;
        }
        let latest = read();
        let changed = !self.same_observed_content(&latest);
        *self = latest;
        changed
    }

    fn same_observed_content(&self, latest: &Self) -> bool {
        match (self.kind, latest.kind) {
            (FileKind::Regular, FileKind::Regular) | (FileKind::Symlink, FileKind::Symlink) => {
                self.content_hash == latest.content_hash
            }
            _ => self == latest,
        }
    }

    fn metadata(path: &Path) -> MetadataFingerprint {
        match fs::symlink_metadata(path) {
            Ok(metadata) => MetadataFingerprint {
                modified: metadata.modified().ok(),
                len: metadata.len(),
                exists: true,
                kind: if metadata.file_type().is_symlink() {
                    FileKind::Symlink
                } else if metadata.is_dir() {
                    FileKind::Directory
                } else if metadata.is_file() {
                    FileKind::Regular
                } else {
                    FileKind::Other
                },
            },
            Err(_) => MetadataFingerprint {
                modified: None,
                len: 0,
                exists: false,
                kind: FileKind::Missing,
            },
        }
    }

    fn metadata_fields(&self) -> MetadataFingerprint {
        MetadataFingerprint {
            modified: self.modified,
            len: self.len,
            exists: self.exists,
            kind: if self.kind == FileKind::Unreadable {
                FileKind::Regular
            } else {
                self.kind
            },
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct MetadataFingerprint {
    modified: Option<SystemTime>,
    len: u64,
    exists: bool,
    kind: FileKind,
}

fn hash(value: &impl Hash) -> u64 {
    let mut hasher = DefaultHasher::new();
    value.hash(&mut hasher);
    hasher.finish()
}

pub(crate) fn read_dependency(path: &Path) -> io::Result<(Vec<u8>, Fingerprint)> {
    let metadata = fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "dependency is not a regular non-symlink file",
        ));
    }
    let mut file = File::open(path)?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)?;
    let fingerprint = Fingerprint::from_open_file(&file, &bytes)?;
    Ok((bytes, fingerprint))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fingerprints_detect_create_change_and_delete() {
        let directory = tempfile::tempdir().expect("tempdir");
        let path = directory.path().join("dependency.adoc");
        let missing = Fingerprint::read(&path);
        fs::write(&path, "one").expect("create");
        let created = Fingerprint::read(&path);
        fs::write(&path, "two").expect("same-length change");
        let changed = Fingerprint::read(&path);
        fs::remove_file(&path).expect("delete");
        let deleted = Fingerprint::read(&path);
        assert_ne!(missing, created);
        assert_ne!(created, changed);
        assert_ne!(changed, deleted);
    }

    #[test]
    fn dependency_snapshot_is_captured_when_the_file_is_read() {
        let directory = tempfile::tempdir().expect("tempdir");
        let dependency = directory.path().join("new.adoc");
        fs::write(&dependency, "old").expect("dependency");
        let (bytes, fingerprint) = read_dependency(&dependency).expect("read dependency");
        assert_eq!(bytes, b"old");

        fs::write(&dependency, "new").expect("replace after read");
        assert_ne!(fingerprint, Fingerprint::read(&dependency));
    }

    #[test]
    fn unchanged_metadata_avoids_content_reads_until_forced() {
        let directory = tempfile::tempdir().expect("tempdir");
        let dependency = directory.path().join("dependency.adoc");
        fs::write(&dependency, "new").expect("dependency");
        let current = Fingerprint::read(&dependency);
        let mut previous = current.clone();
        previous.content_hash = Some(hash(&b"old".as_slice()));

        let reads = std::cell::Cell::new(0);
        assert!(!previous.changed_with(&dependency, false, || {
            reads.set(reads.get() + 1);
            current.clone()
        }));
        assert_eq!(reads.get(), 0);
        assert!(previous.changed_with(&dependency, true, || {
            reads.set(reads.get() + 1);
            current.clone()
        }));
        assert_eq!(reads.get(), 1);
    }

    #[test]
    fn metadata_only_change_updates_cache_after_one_content_read() {
        let directory = tempfile::tempdir().expect("tempdir");
        let dependency = directory.path().join("dependency.adoc");
        fs::write(&dependency, "same").expect("dependency");
        let current = Fingerprint::read(&dependency);
        let mut previous = current.clone();
        previous.len = previous.len.saturating_add(1);
        let reads = std::cell::Cell::new(0);

        assert!(!previous.changed_with(&dependency, false, || {
            reads.set(reads.get() + 1);
            current.clone()
        }));
        assert_eq!(reads.get(), 1);
        assert!(!previous.changed_with(&dependency, false, || {
            panic!("updated metadata must avoid another content read")
        }));
        assert_eq!(reads.get(), 1);
    }

    #[cfg(unix)]
    #[test]
    fn permission_fingerprint_recovers_after_read_access_returns() {
        use std::os::unix::fs::PermissionsExt;

        let directory = tempfile::tempdir().expect("tempdir");
        let path = directory.path().join("dependency.adoc");
        fs::write(&path, "content").expect("fixture");
        fs::set_permissions(&path, fs::Permissions::from_mode(0o000)).expect("deny");
        let denied = Fingerprint::read(&path);
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).expect("restore");
        let restored = Fingerprint::read(&path);
        if denied.content_hash.is_none() {
            assert_ne!(denied, restored);
        }
    }

    #[cfg(unix)]
    #[test]
    fn symlink_fingerprint_never_reads_the_target_body() {
        use std::os::unix::fs::symlink;

        let root = tempfile::tempdir().expect("root");
        let outside = tempfile::tempdir().expect("outside");
        let secret = outside.path().join("secret.adoc");
        fs::write(&secret, "EXTERNAL_SECRET_BODY").expect("secret");
        let watched = root.path().join("dependency");
        symlink(&secret, &watched).expect("symlink");

        let fingerprint = Fingerprint::read(&watched);
        assert_eq!(fingerprint.kind, FileKind::Symlink);
        assert_eq!(
            fingerprint.content_hash,
            Some(hash(&fs::read_link(&watched).expect("link target")))
        );
        assert_ne!(
            fingerprint.content_hash,
            Some(hash(&b"EXTERNAL_SECRET_BODY".as_slice()))
        );
    }
}
