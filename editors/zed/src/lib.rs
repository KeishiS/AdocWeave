mod install;

use install::{MANIFEST_NAME, REPOSITORY};
use std::{
    fs::{self, OpenOptions},
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
    directory: PathBuf,
    owner: PathBuf,
}

impl InstallLock {
    fn acquire(path: &Path) -> Result<Self, String> {
        let owner = path.join(format!(
            "owner-{}-{}",
            unix_timestamp()?,
            unique_operation_id()
        ));
        for attempt in 0..2 {
            match fs::create_dir(path) {
                Ok(()) => {
                    // The owner is recorded by creating its final name. This
                    // avoids publishing an empty or partially written owner
                    // record that another process could mistake for stale.
                    if let Err(error) = OpenOptions::new().write(true).create_new(true).open(&owner)
                    {
                        // The directory still prevents a successor from
                        // acquiring the lock, so removing it cannot target a
                        // successor created after this failed initialization.
                        let _ = fs::remove_dir(path);
                        return Err(format!(
                            "failed to initialize the LSP installation lock: {error}"
                        ));
                    }
                    return Ok(Self {
                        directory: path.to_owned(),
                        owner: owner.clone(),
                    });
                }
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists && attempt == 0 => {
                    if !recover_stale_lock(path)? {
                        return Err(
                            "another AdocWeave LSP installation is already in progress".to_owned()
                        );
                    }
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
        // Only the process that removes its exact owner record may remove the
        // directory. A stale recovery that already removed this record makes
        // this a no-op, so a successor's directory cannot be deleted.
        let _ = remove_owned_lock(&self.directory, &self.owner);
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

fn owner_timestamp(path: &Path) -> Option<u64> {
    let name = path.file_name()?.to_str()?;
    let (created, operation) = name.strip_prefix("owner-")?.split_once('-')?;
    if operation.is_empty() {
        return None;
    }
    created.parse().ok()
}

fn recover_stale_lock(directory: &Path) -> Result<bool, String> {
    let metadata = fs::metadata(directory)
        .map_err(|error| format!("failed to inspect the LSP installation lock: {error}"))?;
    // Earlier versions used a file at this path. It may still belong to a
    // running process, and there is no atomic way to compare and remove that
    // format, so leave it for its owner to release.
    if !metadata.is_dir() {
        return Ok(false);
    }

    let mut entries = fs::read_dir(directory)
        .map_err(|error| format!("failed to inspect the LSP installation lock: {error}"))?;
    let first = entries
        .next()
        .transpose()
        .map_err(|error| format!("failed to inspect the LSP installation lock: {error}"))?;
    let Some(owner) = first else {
        // A fresh empty directory is an owner that is still being initialized.
        // Only a directory left empty for the full stale interval is recoverable.
        let Ok(modified) = metadata.modified() else {
            return Ok(false);
        };
        let Ok(age) = SystemTime::now().duration_since(modified) else {
            return Ok(false);
        };
        if age < STALE_LOCK_AGE {
            return Ok(false);
        }
        return remove_empty_lock_directory(directory);
    };
    if entries.next().is_some() {
        return Ok(false);
    }
    let owner = owner.path();
    let Some(created) = owner_timestamp(&owner) else {
        return Ok(false);
    };
    if unix_timestamp()?.saturating_sub(created) < STALE_LOCK_AGE.as_secs() {
        return Ok(false);
    }

    match fs::remove_file(&owner) {
        Ok(()) => remove_empty_lock_directory(directory),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(format!(
            "failed to recover the stale LSP installation lock: {error}"
        )),
    }
}

fn remove_empty_lock_directory(directory: &Path) -> Result<bool, String> {
    match fs::remove_dir(directory) {
        Ok(()) => Ok(true),
        Err(error)
            if matches!(
                error.kind(),
                std::io::ErrorKind::NotFound | std::io::ErrorKind::DirectoryNotEmpty
            ) =>
        {
            Ok(false)
        }
        Err(error) => Err(format!(
            "failed to recover the stale LSP installation lock: {error}"
        )),
    }
}

fn remove_owned_lock(directory: &Path, owner: &Path) -> Result<(), String> {
    match fs::remove_file(owner) {
        Ok(()) => match fs::remove_dir(directory) {
            Ok(()) => Ok(()),
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::NotFound | std::io::ErrorKind::DirectoryNotEmpty
                ) =>
            {
                Ok(())
            }
            Err(error) => Err(format!(
                "failed to release the LSP installation lock: {error}"
            )),
        },
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(format!(
            "failed to release the LSP installation lock: {error}"
        )),
    }
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
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc, Barrier,
    };
    use std::thread;

    fn concurrent_acquisition_count(path: PathBuf) -> usize {
        const WORKERS: usize = 16;

        let path = Arc::new(path);
        let start = Arc::new(Barrier::new(WORKERS));
        let attempted = Arc::new(AtomicUsize::new(0));
        let acquired = Arc::new(AtomicUsize::new(0));
        let mut workers = Vec::new();
        for _ in 0..WORKERS {
            let path = Arc::clone(&path);
            let start = Arc::clone(&start);
            let attempted = Arc::clone(&attempted);
            let acquired = Arc::clone(&acquired);
            workers.push(thread::spawn(move || {
                start.wait();
                let lock = InstallLock::acquire(&path).ok();
                if lock.is_some() {
                    acquired.fetch_add(1, Ordering::SeqCst);
                }
                attempted.fetch_add(1, Ordering::SeqCst);
                while attempted.load(Ordering::SeqCst) != WORKERS {
                    thread::yield_now();
                }
                drop(lock);
            }));
        }
        for worker in workers {
            worker.join().unwrap();
        }

        acquired.load(Ordering::SeqCst)
    }

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
        assert!(path.is_dir());
        assert_eq!(fs::read_dir(&path).unwrap().count(), 1);
        assert!(first.owner.is_file());
        assert!(InstallLock::acquire(&path).is_err());
        drop(first);
        assert!(!path.exists());
        drop(InstallLock::acquire(&path).unwrap());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn concurrent_installations_have_exactly_one_owner() {
        let root =
            std::env::temp_dir().join(format!("adocweave-zed-race-{}", unique_operation_id()));
        fs::create_dir_all(&root).unwrap();
        let path = root.join("install.lock");

        assert_eq!(concurrent_acquisition_count(path.clone()), 1);
        assert!(!path.exists());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn concurrent_stale_recovery_has_exactly_one_owner() {
        let root = std::env::temp_dir().join(format!(
            "adocweave-zed-stale-race-{}",
            unique_operation_id()
        ));
        fs::create_dir_all(&root).unwrap();
        let path = root.join("install.lock");
        fs::create_dir(&path).unwrap();
        fs::write(path.join("owner-0-abandoned"), []).unwrap();

        assert_eq!(concurrent_acquisition_count(path.clone()), 1);
        assert!(!path.exists());
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
        fs::create_dir(&path).unwrap();
        fs::write(path.join("owner-0-abandoned"), []).unwrap();
        let lock = InstallLock::acquire(&path).unwrap();
        assert!(path.exists());
        drop(lock);
        assert!(!path.exists());
        fs::remove_dir_all(root).unwrap();
    }

    /// A download that outlives the stale age must not delete its successor's lock.
    #[test]
    fn releasing_an_overtaken_lock_leaves_the_new_owner_holding_it() {
        let root =
            std::env::temp_dir().join(format!("adocweave-zed-owner-{}", unique_operation_id()));
        fs::create_dir_all(&root).unwrap();
        let path = root.join("install.lock");

        // The first installation runs past the stale age, so the second takes
        // the lock over.
        fs::create_dir(&path).unwrap();
        let old_owner = path.join("owner-0-overtaken");
        fs::write(&old_owner, []).unwrap();
        let overtaken = InstallLock {
            directory: path.clone(),
            owner: old_owner,
        };
        let successor = InstallLock::acquire(&path).unwrap();

        // The first one finishing must not remove the lock it no longer owns.
        drop(overtaken);
        assert!(path.exists(), "the successor still holds its lock");
        assert!(
            InstallLock::acquire(&path).is_err(),
            "a third installation must not start alongside the successor"
        );

        drop(successor);
        assert!(!path.exists());
        fs::remove_dir_all(root).unwrap();
    }

    /// Removing an old owner must not remove a successor's directory.
    #[test]
    fn an_overtaken_owner_cannot_remove_its_successor() {
        let root =
            std::env::temp_dir().join(format!("adocweave-zed-token-{}", unique_operation_id()));
        fs::create_dir_all(&root).unwrap();
        let path = root.join("install.lock");
        fs::create_dir(&path).unwrap();
        let successor = path.join(format!("owner-{}-successor", unix_timestamp().unwrap()));
        fs::write(&successor, []).unwrap();

        remove_owned_lock(&path, &path.join("owner-0-predecessor")).unwrap();
        assert!(path.exists());
        assert!(successor.exists());

        remove_owned_lock(&path, &successor).unwrap();
        assert!(!path.exists());
        fs::remove_dir_all(root).unwrap();
    }
}

zed::register_extension!(AdocWeaveExtension);
