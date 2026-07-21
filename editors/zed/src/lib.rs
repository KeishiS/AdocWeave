mod install;

use install::{MANIFEST_NAME, REPOSITORY};
use std::{fs, path::Path};
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

        let manifest_temp = format!(".adocweave-{VERSION}-{target}-manifest.tmp");
        let archive_temp = format!(".adocweave-{VERSION}-{target}-archive.tmp");
        let tar_temp = format!(".adocweave-{VERSION}-{target}-archive.tar.tmp");
        let staging = format!(".adocweave-{VERSION}-{target}-install.tmp");
        cleanup_file(&manifest_temp);
        cleanup_file(&archive_temp);
        cleanup_file(&tar_temp);
        cleanup_directory(&staging);

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
            let staging_binary = Path::new(&staging).join("adocweave-lsp");
            install::extract_binary(
                Path::new(&archive_temp),
                Path::new(&tar_temp),
                &staging_binary,
                target,
            )?;
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
        cleanup_file(&tar_temp);
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
    let backup = destination.with_extension("previous");
    cleanup_directory(&backup);
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
            std::env::temp_dir().join(format!("adocweave-zed-rollback-{}", std::process::id()));
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
        assert!(!destination.with_extension("previous").exists());
        fs::remove_dir_all(root).unwrap();
    }
}

zed::register_extension!(AdocWeaveExtension);
