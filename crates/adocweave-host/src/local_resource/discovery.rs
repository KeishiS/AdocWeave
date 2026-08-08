//! Directory discovery below the configured roots.
//!
//! Walking a workspace is a separate concern from reading one resource: it
//! decides which files exist and stops at the configured bounds, without
//! knowing what any of them contain.

use std::collections::VecDeque;
#[cfg(not(target_os = "linux"))]
use std::fs;
use std::path::{Path, PathBuf};

use super::error::ResourceError;
use super::{FilesystemJobCoordinator, LocalFilesystemSessionId, LocalFilesystemState};

pub(super) struct LocalFilesystemView<'a> {
    pub(super) state: &'a LocalFilesystemState,
    pub(super) job: Option<(LocalFilesystemSessionId, &'a FilesystemJobCoordinator)>,
}

impl LocalFilesystemView<'_> {
    pub(super) fn discover_adoc_paths_with_control(
        &self,
        scan_entry_limit: usize,
        exclude_directory: impl FnMut(&Path, &Path) -> bool,
        is_cancelled: impl FnMut() -> bool,
    ) -> Result<Vec<PathBuf>, ResourceError> {
        #[cfg(target_os = "linux")]
        {
            self.discover_adoc_paths_with_limit_handle_relative(
                scan_entry_limit,
                exclude_directory,
                is_cancelled,
            )
        }
        #[cfg(not(target_os = "linux"))]
        {
            let mut exclude_directory = exclude_directory;
            let mut is_cancelled = is_cancelled;
            let mut paths = Vec::new();
            let mut scanned_entries = 0_usize;
            for root in &self.state.roots {
                let mut pending = VecDeque::from([root.clone()]);
                while let Some(path) = pending.pop_front() {
                    if is_cancelled() {
                        return Err(ResourceError::Unverifiable(
                            "local filesystem scan was cancelled".to_owned(),
                        ));
                    }
                    let metadata =
                        fs::symlink_metadata(&path).map_err(|source| ResourceError::Inspect {
                            path: path.clone(),
                            source: source.to_string(),
                        })?;
                    if metadata.file_type().is_symlink() {
                        continue;
                    }
                    if metadata.is_dir() {
                        if path != *root
                            && let Ok(relative) = path.strip_prefix(root)
                            && exclude_directory(root, relative)
                        {
                            continue;
                        }
                        let mut job_permit = self
                            .job
                            .map(|(session, job)| job.begin_directory_read(session))
                            .transpose()
                            .map_err(ResourceError::from)?;
                        let mut children = Vec::new();
                        let mut directory =
                            fs::read_dir(&path).map_err(|source| ResourceError::Inspect {
                                path: path.clone(),
                                source: source.to_string(),
                            })?;
                        loop {
                            if is_cancelled() {
                                return Err(ResourceError::Unverifiable(
                                    "local filesystem scan was cancelled".to_owned(),
                                ));
                            }
                            let reservation = job_permit
                                .as_mut()
                                .map(|permit| {
                                    permit.reserve_entry_with_cancellation(&mut is_cancelled)
                                })
                                .transpose()
                                .map_err(ResourceError::from)?;
                            let Some(child) = directory.next() else {
                                if let Some(reservation) = reservation {
                                    reservation.commit(0).map_err(ResourceError::from)?;
                                }
                                break;
                            };
                            if let Some(reservation) = reservation {
                                reservation.commit(1).map_err(ResourceError::from)?;
                            }
                            let child = child.map_err(|source| ResourceError::Inspect {
                                path: path.clone(),
                                source: source.to_string(),
                            })?;
                            if is_cancelled() {
                                return Err(ResourceError::Unverifiable(
                                    "local filesystem scan was cancelled".to_owned(),
                                ));
                            }
                            children.push(child);
                            scanned_entries += 1;
                            if scanned_entries > scan_entry_limit {
                                return Err(ResourceError::ScanEntryLimit {
                                    limit: scan_entry_limit,
                                });
                            }
                        }
                        children.sort_by_key(fs::DirEntry::file_name);
                        pending.extend(children.into_iter().map(|entry| entry.path()));
                    } else if path.extension().and_then(|value| value.to_str()) == Some("adoc") {
                        paths.push(path);
                    }
                }
            }
            paths.sort();
            paths.dedup();
            Ok(paths)
        }
    }

    #[cfg(target_os = "linux")]
    fn discover_adoc_paths_with_limit_handle_relative(
        &self,
        scan_entry_limit: usize,
        mut exclude_directory: impl FnMut(&Path, &Path) -> bool,
        mut is_cancelled: impl FnMut() -> bool,
    ) -> Result<Vec<PathBuf>, ResourceError> {
        use std::ffi::OsString;
        use std::os::unix::ffi::OsStringExt;

        use rustix::fs::{AtFlags, Dir, FileType, statat};

        let mut paths = Vec::new();
        let mut scanned_entries = 0_usize;
        for (root, session) in self.state.roots.iter().zip(&self.state.sessions) {
            let policy = session.policy();
            let mut pending = VecDeque::from([root.clone()]);
            while let Some(path) = pending.pop_front() {
                if is_cancelled() {
                    return Err(ResourceError::Unverifiable(
                        "local filesystem scan was cancelled".to_owned(),
                    ));
                }
                if path != *root
                    && let Ok(relative) = path.strip_prefix(root)
                    && exclude_directory(root, relative)
                {
                    continue;
                }
                let mut job_permit = self
                    .job
                    .map(|(session, job)| job.begin_directory_read(session))
                    .transpose()
                    .map_err(ResourceError::from)?;
                let directory = policy
                    .open_directory_no_symlinks(&path)
                    .map_err(ResourceError::from)?;
                let mut entries =
                    Dir::read_from(&directory).map_err(|source| ResourceError::Inspect {
                        path: path.clone(),
                        source: source.to_string(),
                    })?;
                let mut children = Vec::<(OsString, FileType)>::new();
                loop {
                    if is_cancelled() {
                        return Err(ResourceError::Unverifiable(
                            "local filesystem scan was cancelled".to_owned(),
                        ));
                    }
                    let reservation = job_permit
                        .as_mut()
                        .map(|permit| permit.reserve_entry_with_cancellation(&mut is_cancelled))
                        .transpose()
                        .map_err(ResourceError::from)?;
                    let Some(child) = entries.next() else {
                        if let Some(reservation) = reservation {
                            reservation.commit(0).map_err(ResourceError::from)?;
                        }
                        break;
                    };
                    let child = match child {
                        Ok(child) => child,
                        Err(source) => {
                            if let Some(reservation) = reservation {
                                reservation.commit(1).map_err(ResourceError::from)?;
                            }
                            return Err(ResourceError::Inspect {
                                path: path.clone(),
                                source: source.to_string(),
                            });
                        }
                    };
                    let name = child.file_name();
                    let implicit = name.to_bytes() == b"." || name.to_bytes() == b"..";
                    if let Some(reservation) = reservation {
                        reservation
                            .commit(u64::from(!implicit))
                            .map_err(ResourceError::from)?;
                    }
                    if is_cancelled() {
                        return Err(ResourceError::Unverifiable(
                            "local filesystem scan was cancelled".to_owned(),
                        ));
                    }
                    if implicit {
                        continue;
                    }
                    let name = OsString::from_vec(name.to_bytes().to_vec());
                    let child_path = path.join(&name);
                    let metadata =
                        statat(&directory, &name, AtFlags::SYMLINK_NOFOLLOW).map_err(|source| {
                            ResourceError::Inspect {
                                path: child_path,
                                source: source.to_string(),
                            }
                        })?;
                    let file_type = FileType::from_raw_mode(metadata.st_mode);
                    children.push((name, file_type));
                    scanned_entries = scanned_entries.saturating_add(1);
                    if scanned_entries > scan_entry_limit {
                        return Err(ResourceError::ScanEntryLimit {
                            limit: scan_entry_limit,
                        });
                    }
                }
                children.sort_by(|left, right| left.0.cmp(&right.0));
                for (name, file_type) in children {
                    let child = path.join(name);
                    if file_type == FileType::Symlink {
                        continue;
                    }
                    if file_type == FileType::Directory {
                        pending.push_back(child);
                    } else if file_type == FileType::RegularFile
                        && child.extension().and_then(|value| value.to_str()) == Some("adoc")
                    {
                        paths.push(child);
                    }
                }
            }
        }
        paths.sort();
        paths.dedup();
        Ok(paths)
    }
}
