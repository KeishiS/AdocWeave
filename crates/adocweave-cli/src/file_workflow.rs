//! Guarded in-place writes and user-visible file differences.

use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use super::{CliError, ColorChoice};

pub(crate) struct PendingWrite {
    pub(crate) path: PathBuf,
    pub(crate) original: Vec<u8>,
    pub(crate) replacement: Vec<u8>,
}

pub(crate) fn atomic_write_all(writes: Vec<PendingWrite>) -> Result<(), CliError> {
    let mut prepared = Vec::new();
    for write in writes {
        let metadata = fs::symlink_metadata(&write.path).map_err(|source| CliError::Read {
            source_name: write.path.display().to_string(),
            source,
        })?;
        if metadata.file_type().is_symlink() || !metadata.is_file() {
            return Err(CliError::Path(format!(
                "write target is not a regular non-symlink file: {}",
                write.path.display()
            )));
        }
        ensure_unchanged(&write)?;
        let parent = write
            .path
            .parent()
            .ok_or_else(|| CliError::Path("write target has no parent directory".to_owned()))?;
        let mut temporary = tempfile::NamedTempFile::new_in(parent).map_err(CliError::Write)?;
        temporary
            .as_file()
            .set_permissions(metadata.permissions())
            .map_err(CliError::Write)?;
        temporary
            .write_all(&write.replacement)
            .map_err(CliError::Write)?;
        temporary.as_file().sync_all().map_err(CliError::Write)?;
        prepared.push((write, temporary));
    }
    for (write, temporary) in prepared {
        ensure_unchanged(&write)?;
        temporary
            .persist(&write.path)
            .map_err(|error| CliError::Write(error.error))?;
        sync_parent(&write.path)?;
    }
    Ok(())
}

fn ensure_unchanged(write: &PendingWrite) -> Result<(), CliError> {
    let current = fs::read(&write.path).map_err(|source| CliError::Read {
        source_name: write.path.display().to_string(),
        source,
    })?;
    if current == write.original {
        Ok(())
    } else {
        Err(CliError::ConcurrentModification(write.path.clone()))
    }
}

#[cfg(unix)]
fn sync_parent(path: &Path) -> Result<(), CliError> {
    if let Some(parent) = path.parent() {
        fs::File::open(parent)
            .and_then(|directory| directory.sync_all())
            .map_err(CliError::Write)?;
    }
    Ok(())
}

#[cfg(not(unix))]
fn sync_parent(_path: &Path) -> Result<(), CliError> {
    Ok(())
}

pub(crate) fn colorize_lines(output: &str, choice: ColorChoice) -> String {
    use std::io::IsTerminal as _;

    let enabled = match choice {
        ColorChoice::Always => true,
        ColorChoice::Never => false,
        ColorChoice::Auto => io::stdout().is_terminal(),
    };
    if !enabled {
        return output.to_owned();
    }
    let mut colored = String::new();
    for line in output.split_inclusive('\n') {
        let color = if line.starts_with('+') || line.contains(": hint[") {
            Some("\u{1b}[32m")
        } else if line.starts_with('-') || line.contains(": error[") {
            Some("\u{1b}[31m")
        } else if line.contains(": warning[") {
            Some("\u{1b}[33m")
        } else if line.contains(": information[") {
            Some("\u{1b}[36m")
        } else {
            None
        };
        if let Some(color) = color {
            colored.push_str(color);
            colored.push_str(line.trim_end_matches('\n'));
            colored.push_str("\u{1b}[0m");
            if line.ends_with('\n') {
                colored.push('\n');
            }
        } else {
            colored.push_str(line);
        }
    }
    colored
}

#[cfg(test)]
mod tests {
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::*;

    fn temporary_directory(name: &str) -> PathBuf {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let path = std::env::temp_dir().join(format!("adocweave-{name}-{unique}"));
        fs::create_dir(&path).expect("temporary directory");
        path
    }

    #[test]
    fn concurrent_content_change_is_never_replaced() {
        let root = temporary_directory("concurrent-write");
        let path = root.join("document.adoc");
        fs::write(&path, "original\n").expect("original");
        let pending = PendingWrite {
            path: path.clone(),
            original: b"original\n".to_vec(),
            replacement: b"formatted\n".to_vec(),
        };
        fs::write(&path, "concurrent\n").expect("concurrent update");

        assert!(matches!(
            atomic_write_all(vec![pending]),
            Err(CliError::ConcurrentModification(changed)) if changed == path
        ));
        assert_eq!(fs::read_to_string(&path).unwrap(), "concurrent\n");
        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn preflight_failure_leaves_every_existing_file_unchanged() {
        let root = temporary_directory("preflight-write");
        let first = root.join("first.adoc");
        let missing = root.join("missing.adoc");
        fs::write(&first, "first\n").expect("first");
        let writes = vec![
            PendingWrite {
                path: first.clone(),
                original: b"first\n".to_vec(),
                replacement: b"changed\n".to_vec(),
            },
            PendingWrite {
                path: missing,
                original: Vec::new(),
                replacement: b"created\n".to_vec(),
            },
        ];

        assert!(atomic_write_all(writes).is_err());
        assert_eq!(fs::read_to_string(&first).unwrap(), "first\n");
        fs::remove_dir_all(root).expect("cleanup");
    }
}
