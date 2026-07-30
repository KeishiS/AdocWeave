mod install;

use install::{MANIFEST_NAME, REPOSITORY};
use std::{
    fs::{self, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use zed_extension_api as zed;

const VERSION: &str = env!("CARGO_PKG_VERSION");
const SERVER_NAME: &str = "adocweave";

struct AdocWeaveExtension;

impl AdocWeaveExtension {
    fn managed_binary(&self, language_server_id: &zed::LanguageServerId) -> Result<String, String> {
        let (os, architecture) = zed::current_platform();
        let target = install::target_for_platform(os, architecture)?;
        let cache = install::cache_paths(VERSION, target);
        if install::verified_cache(&cache, VERSION, target) {
            return Ok(path_string(&cache.binary));
        }
        let _lock = InstallLock::acquire(&lock_path(&cache.directory))?;
        if install::verified_cache(&cache, VERSION, target) {
            return Ok(path_string(&cache.binary));
        }

        zed::set_language_server_installation_status(
            language_server_id,
            &zed::LanguageServerInstallationStatus::CheckingForUpdate,
        );
        let tag = format!("v{VERSION}");
        let release = zed::github_release_by_tag_name(REPOSITORY, &tag)
            .map_err(|error| format!("failed to resolve AdocWeave release {tag}: {error}"))?;
        let manifest_asset = release
            .assets
            .iter()
            .find(|asset| asset.name == MANIFEST_NAME)
            .ok_or_else(|| format!("AdocWeave release {tag} has no {MANIFEST_NAME}"))?;

        let operation = unique_operation_id();
        let manifest_temp = format!(".adocweave-{VERSION}-{target}-{operation}-manifest.tmp");
        let archive_temp = format!(".adocweave-{VERSION}-{target}-{operation}-archive.tmp");
        let staging = format!(".adocweave-{VERSION}-{target}-{operation}-install.tmp");

        let result: Result<String, String> = (|| {
            zed::set_language_server_installation_status(
                language_server_id,
                &zed::LanguageServerInstallationStatus::Downloading,
            );
            zed::download_file(
                &manifest_asset.download_url,
                &manifest_temp,
                zed::DownloadedFileType::Uncompressed,
            )
            .map_err(|error| format!("failed to download {MANIFEST_NAME}: {error}"))?;
            let manifest = fs::read_to_string(&manifest_temp)
                .map_err(|error| format!("failed to read {MANIFEST_NAME}: {error}"))?;
            let selected = install::select_lsp_asset(&manifest, VERSION, target)?;
            let archive_asset = release
                .assets
                .iter()
                .find(|asset| asset.name == selected.name)
                .ok_or_else(|| format!("AdocWeave release {tag} has no {}", selected.name))?;
            zed::download_file(
                &archive_asset.download_url,
                &archive_temp,
                zed::DownloadedFileType::Uncompressed,
            )
            .map_err(|error| format!("failed to download {}: {error}", selected.name))?;
            install::verify_download(Path::new(&archive_temp), &selected)?;

            fs::create_dir(&staging)
                .map_err(|error| format!("failed to create LSP staging directory: {error}"))?;
            let staging_binary = Path::new(&staging).join(selected.executable);
            install::extract_binary(Path::new(&archive_temp), &staging_binary, target, &selected)?;
            zed::make_file_executable(&path_string(&staging_binary))
                .map_err(|error| format!("failed to make adocweave-lsp executable: {error}"))?;
            let binary_hash = install::sha256_file(&staging_binary)?;
            install::write_marker(
                &Path::new(&staging).join("verified.json"),
                VERSION,
                target,
                &selected,
                &binary_hash,
            )?;
            commit_staging(Path::new(&staging), &cache.directory)?;
            Ok(path_string(&cache.binary))
        })();

        cleanup_file(&manifest_temp);
        cleanup_file(&archive_temp);
        cleanup_directory(&staging);
        match result {
            Ok(binary) => {
                zed::set_language_server_installation_status(
                    language_server_id,
                    &zed::LanguageServerInstallationStatus::None,
                );
                Ok(binary)
            }
            Err(error) => {
                zed::set_language_server_installation_status(
                    language_server_id,
                    &zed::LanguageServerInstallationStatus::Failed(error.clone()),
                );
                Err(error)
            }
        }
    }
}

impl zed::Extension for AdocWeaveExtension {
    fn new() -> Self {
        Self
    }

    fn language_server_command(
        &mut self,
        language_server_id: &zed::LanguageServerId,
        worktree: &zed::Worktree,
    ) -> zed::Result<zed::Command> {
        let settings = zed::settings::LspSettings::for_worktree(SERVER_NAME, worktree)?;
        if let Some(binary) = settings.binary.filter(|binary| binary.path.is_some()) {
            return Ok(zed::Command {
                command: binary.path.expect("filtered above"),
                args: binary.arguments.unwrap_or_default(),
                env: binary.env.unwrap_or_default().into_iter().collect(),
            });
        }
        if let Some(command) = worktree.which("adocweave-lsp") {
            return Ok(zed::Command {
                command,
                args: Vec::new(),
                env: worktree.shell_env(),
            });
        }
        Ok(zed::Command {
            command: self.managed_binary(language_server_id)?,
            args: Vec::new(),
            env: worktree.shell_env(),
        })
    }
}

fn commit_staging(staging: &Path, destination: &Path) -> Result<(), String> {
    let backup = destination.with_extension(format!("previous-{}", unique_operation_id()));
    let had_previous = destination.exists();
    if had_previous {
        fs::rename(destination, &backup)
            .map_err(|error| format!("failed to preserve the previous LSP cache: {error}"))?;
    }
    if let Err(error) = fs::rename(staging, destination) {
        if had_previous {
            let _ = fs::rename(&backup, destination);
        }
        return Err(format!("failed to commit the verified LSP cache: {error}"));
    }
    cleanup_directory(&backup);
    Ok(())
}

static OPERATION_COUNTER: AtomicU64 = AtomicU64::new(0);

fn unique_operation_id() -> String {
    // Zed runs the extension on WASI, which does not provide a process ID.
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or_default();
    format!(
        "{timestamp}-{}",
        OPERATION_COUNTER.fetch_add(1, Ordering::Relaxed)
    )
}

struct InstallLock {
    path: PathBuf,
}

impl InstallLock {
    fn acquire(path: &Path) -> Result<Self, String> {
        for attempt in 0..2 {
            match OpenOptions::new().write(true).create_new(true).open(path) {
                Ok(mut file) => {
                    let created = unix_timestamp()?;
                    if let Err(error) = writeln!(file, "{created}") {
                        drop(file);
                        cleanup_file(path);
                        return Err(format!(
                            "failed to initialize the LSP installation lock: {error}"
                        ));
                    }
                    return Ok(Self {
                        path: path.to_owned(),
                    });
                }
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists && attempt == 0 => {
                    if !stale_lock(path)? {
                        return Err(
                            "another AdocWeave LSP installation is already in progress".to_owned()
                        );
                    }
                    fs::remove_file(path).map_err(|error| {
                        format!("failed to recover a stale LSP installation lock: {error}")
                    })?;
                }
                Err(error) => {
                    return Err(format!(
                        "failed to acquire the LSP installation lock: {error}"
                    ))
                }
            }
        }
        Err("failed to acquire the LSP installation lock".to_owned())
    }
}

impl Drop for InstallLock {
    fn drop(&mut self) {
        cleanup_file(&self.path);
    }
}

const STALE_LOCK_AGE: Duration = Duration::from_secs(15 * 60);

fn lock_path(directory: &Path) -> PathBuf {
    let mut path = directory.as_os_str().to_os_string();
    path.push(".lock");
    PathBuf::from(path)
}

fn unix_timestamp() -> Result<u64, String> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .map_err(|error| format!("system clock is before the Unix epoch: {error}"))
}

fn stale_lock(path: &Path) -> Result<bool, String> {
    let content = fs::read_to_string(path)
        .map_err(|error| format!("failed to inspect the LSP installation lock: {error}"))?;
    let Ok(created) = content.trim().parse::<u64>() else {
        return Ok(true);
    };
    Ok(unix_timestamp()?.saturating_sub(created) >= STALE_LOCK_AGE.as_secs())
}

fn cleanup_file(path: impl AsRef<Path>) {
    match fs::remove_file(path) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(_) => {}
    }
}

fn cleanup_directory(path: impl AsRef<Path>) {
    match fs::remove_dir_all(path) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(_) => {}
    }
}

fn path_string(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn failed_cache_commit_restores_the_previous_verified_directory() {
        let root =
            std::env::temp_dir().join(format!("adocweave-zed-rollback-{}", unique_operation_id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        let destination = root.join("current");
        fs::create_dir(&destination).unwrap();
        fs::write(destination.join("adocweave-lsp"), b"previous").unwrap();
        let missing_staging = root.join("missing-staging");

        assert!(commit_staging(&missing_staging, &destination).is_err());
        assert_eq!(
            fs::read(destination.join("adocweave-lsp")).unwrap(),
            b"previous"
        );
        assert_eq!(fs::read_dir(&root).unwrap().count(), 1);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn installation_lock_has_single_owner_and_is_released() {
        let root =
            std::env::temp_dir().join(format!("adocweave-zed-lock-{}", unique_operation_id()));
        fs::create_dir_all(&root).unwrap();
        let path = root.join("install.lock");
        let first = InstallLock::acquire(&path).unwrap();
        assert!(InstallLock::acquire(&path).is_err());
        drop(first);
        assert!(InstallLock::acquire(&path).is_ok());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn stale_installation_lock_is_recovered_without_colliding_with_other_targets() {
        let root =
            std::env::temp_dir().join(format!("adocweave-zed-stale-{}", unique_operation_id()));
        fs::create_dir_all(&root).unwrap();
        let cache = root.join("adocweave-lsp-0.16.0-x86_64-unknown-linux-musl");
        let path = lock_path(&cache);
        assert!(path.ends_with("adocweave-lsp-0.16.0-x86_64-unknown-linux-musl.lock"));
        fs::write(&path, "0\n").unwrap();
        let lock = InstallLock::acquire(&path).unwrap();
        assert!(path.exists());
        drop(lock);
        assert!(!path.exists());
        fs::remove_dir_all(root).unwrap();
    }
}

zed::register_extension!(AdocWeaveExtension);
