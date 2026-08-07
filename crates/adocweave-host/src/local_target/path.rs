//! Confinement of a candidate path below its root.
//!
//! These functions decide only whether a path may be used, from the path and
//! what the filesystem reports about it. They hold no session or policy state,
//! so the rules they enforce can be read without following the readers that
//! apply them.

#[cfg(not(target_os = "linux"))]
use std::collections::BTreeSet;
#[cfg(not(target_os = "linux"))]
use std::fs;
use std::path::{Component, Path, PathBuf};

use super::error::LocalTargetError;

#[cfg(not(target_os = "linux"))]
pub(super) fn reject_symlink_components(
    root: &Path,
    candidate: &Path,
) -> Result<(), LocalTargetError> {
    let relative = candidate
        .strip_prefix(root)
        .map_err(|_| LocalTargetError::OutsideRoot(candidate.to_owned()))?;
    let mut current = root.to_owned();
    for component in relative.components() {
        let Component::Normal(name) = component else {
            return Err(LocalTargetError::Unverifiable(
                candidate.to_string_lossy().into_owned(),
            ));
        };
        current.push(name);
        match fs::symlink_metadata(&current) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err(LocalTargetError::OutsideRoot(current));
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => break,
            Err(source) => return Err(classify_io(current, source)),
        }
    }
    Ok(())
}

#[cfg(target_os = "linux")]
pub(super) fn classify_errno(path: &Path, source: rustix::io::Errno) -> LocalTargetError {
    if source == rustix::io::Errno::XDEV {
        return LocalTargetError::OutsideRoot(path.to_owned());
    }
    if source == rustix::io::Errno::NOTDIR {
        return LocalTargetError::NotDirectory(path.to_owned());
    }
    classify_io(
        path.to_owned(),
        std::io::Error::from_raw_os_error(source.raw_os_error()),
    )
}

pub(super) fn decode_relative_path(target: &str) -> Result<PathBuf, LocalTargetError> {
    if target.is_empty()
        || target.starts_with(['/', '\\'])
        || target.contains('\\')
        || target.contains(':')
        || target.chars().any(char::is_control)
    {
        return Err(LocalTargetError::Unverifiable(target.to_owned()));
    }
    let bytes = target.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] != b'%' {
            decoded.push(bytes[index]);
            index += 1;
            continue;
        }
        if index + 2 >= bytes.len() {
            return Err(LocalTargetError::Unverifiable(target.to_owned()));
        }
        let (Some(high), Some(low)) = (hex(bytes[index + 1]), hex(bytes[index + 2])) else {
            return Err(LocalTargetError::Unverifiable(target.to_owned()));
        };
        decoded.push(high * 16 + low);
        index += 3;
    }
    let decoded = String::from_utf8(decoded)
        .map_err(|_| LocalTargetError::Unverifiable(target.to_owned()))?;
    if decoded.contains(':') || decoded.contains('\\') || decoded.chars().any(char::is_control) {
        return Err(LocalTargetError::Unverifiable(target.to_owned()));
    }
    Ok(PathBuf::from(decoded))
}

pub(super) fn normalize_below_root(
    root: &Path,
    base: &Path,
    relative: &Path,
) -> Result<PathBuf, LocalTargetError> {
    let mut candidate = base.to_owned();
    for component in relative.components() {
        match component {
            Component::Normal(value) => candidate.push(value),
            Component::CurDir => {}
            Component::ParentDir => {
                if candidate == root || !candidate.pop() || !candidate.starts_with(root) {
                    return Err(LocalTargetError::OutsideRoot(candidate));
                }
            }
            Component::RootDir | Component::Prefix(_) => {
                return Err(LocalTargetError::OutsideRoot(candidate));
            }
        }
    }
    if candidate == base && relative.as_os_str().is_empty() {
        return Err(LocalTargetError::Unverifiable(
            relative.to_string_lossy().into_owned(),
        ));
    }
    Ok(candidate)
}

#[cfg(not(target_os = "linux"))]
pub(super) fn reject_dangling_symlink_escape(
    root: &Path,
    candidate: &Path,
) -> Result<(), LocalTargetError> {
    reject_dangling_symlink_escape_inner(root, candidate, &mut BTreeSet::new(), 0)
}

#[cfg(not(target_os = "linux"))]
fn reject_dangling_symlink_escape_inner(
    root: &Path,
    candidate: &Path,
    visited: &mut BTreeSet<PathBuf>,
    depth: usize,
) -> Result<(), LocalTargetError> {
    const MAX_SYMLINK_DEPTH: usize = 64;
    if depth > MAX_SYMLINK_DEPTH {
        return Err(LocalTargetError::Unverifiable(format!(
            "local target symlink depth exceeds {MAX_SYMLINK_DEPTH}: {}",
            candidate.display()
        )));
    }
    if !candidate.starts_with(root) {
        return Err(LocalTargetError::OutsideRoot(candidate.to_owned()));
    }
    let metadata = match fs::symlink_metadata(candidate) {
        Ok(metadata) => metadata,
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => {
            return candidate.parent().map_or(Ok(()), |parent| {
                reject_dangling_symlink_escape_inner(root, parent, visited, depth)
            });
        }
        Err(source) => return Err(classify_io(candidate.to_owned(), source)),
    };
    if !metadata.file_type().is_symlink() {
        return Ok(());
    }
    if !visited.insert(candidate.to_owned()) {
        return Err(LocalTargetError::Unverifiable(format!(
            "local target symlink cycle: {}",
            candidate.display()
        )));
    }
    let destination =
        fs::read_link(candidate).map_err(|source| classify_io(candidate.to_owned(), source))?;
    let resolved = if destination.is_absolute() {
        destination
    } else {
        candidate
            .parent()
            .unwrap_or_else(|| Path::new(""))
            .join(destination)
    };
    let normalized = normalize_absolute(&resolved);
    reject_dangling_symlink_escape_inner(root, &normalized, visited, depth + 1)
}

#[cfg(not(target_os = "linux"))]
pub(super) fn normalize_absolute(path: &Path) -> PathBuf {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Prefix(prefix) => normalized.push(prefix.as_os_str()),
            Component::RootDir => normalized.push(component.as_os_str()),
            Component::CurDir => {}
            Component::ParentDir => {
                normalized.pop();
            }
            Component::Normal(value) => normalized.push(value),
        }
    }
    normalized
}

#[cfg(not(target_os = "linux"))]
pub(super) fn ensure_existing_ancestor_is_inside(
    root: &Path,
    candidate: &Path,
) -> Result<(), LocalTargetError> {
    let mut ancestor = candidate.parent();
    while let Some(path) = ancestor {
        reject_dangling_symlink_escape(root, path)?;
        match path.canonicalize() {
            Ok(canonical) => {
                return if canonical.starts_with(root) {
                    Ok(())
                } else {
                    Err(LocalTargetError::OutsideRoot(canonical))
                };
            }
            Err(source) if source.kind() == std::io::ErrorKind::NotFound => {
                ancestor = path.parent();
            }
            Err(source) => return Err(classify_io(path.to_owned(), source)),
        }
    }
    Err(LocalTargetError::Unverifiable(
        candidate.to_string_lossy().into_owned(),
    ))
}

const fn hex(value: u8) -> Option<u8> {
    match value {
        b'0'..=b'9' => Some(value - b'0'),
        b'a'..=b'f' => Some(value - b'a' + 10),
        b'A'..=b'F' => Some(value - b'A' + 10),
        _ => None,
    }
}

pub(super) fn classify_io(path: PathBuf, source: std::io::Error) -> LocalTargetError {
    match source.kind() {
        std::io::ErrorKind::NotFound => LocalTargetError::Missing(path),
        std::io::ErrorKind::PermissionDenied => LocalTargetError::PermissionDenied(path),
        _ => LocalTargetError::Unverifiable(format!("{}: {source}", path.display())),
    }
}
