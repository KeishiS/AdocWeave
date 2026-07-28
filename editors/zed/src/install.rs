use sha2::{Digest, Sha256};
use std::{
    collections::HashSet,
    fs::{self, File},
    io::{self, BufReader, BufWriter, Read},
    path::{Path, PathBuf},
    sync::OnceLock,
};

pub const REPOSITORY: &str = "KeishiS/adocweave";
pub const MANIFEST_NAME: &str = "adocweave-dist-manifest.json";
const PLATFORM_CONTRACT: &str = include_str!("../platforms.json");
const MAX_DECOMPRESSED_ARCHIVE_BYTES: u64 = 128 * 1024 * 1024;
const MAX_BINARY_BYTES: u64 = 64 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReleaseAsset {
    pub name: String,
    pub sha256: String,
    pub byte_size: u64,
    pub archive: &'static str,
    pub executable: &'static str,
    pub source_commit: String,
}

pub fn target_for_platform(
    os: zed_extension_api::Os,
    arch: zed_extension_api::Architecture,
) -> Result<&'static str, String> {
    use zed_extension_api::{Architecture, Os};

    let (os, architecture) = match (os, arch) {
        (Os::Linux, Architecture::X8664) => ("linux", "x64"),
        (Os::Linux, Architecture::Aarch64) => ("linux", "arm64"),
        (Os::Mac, Architecture::X8664) => ("darwin", "x64"),
        (Os::Mac, Architecture::Aarch64) => ("darwin", "arm64"),
        (Os::Windows, Architecture::X8664) => ("win32", "x64"),
        (Os::Linux, Architecture::X86) => ("linux", "ia32"),
        (Os::Mac, Architecture::X86) => ("darwin", "ia32"),
        (Os::Windows, Architecture::X86) => ("win32", "ia32"),
        (Os::Windows, Architecture::Aarch64) => ("win32", "arm64"),
    };
    platform_contract()?
        .iter()
        .find(|entry| entry.os == os && entry.architecture == architecture)
        .map(|entry| entry.target.as_str())
        .ok_or_else(|| format!("AdocWeave LSP does not support {os} {architecture}"))
}

fn target_asset_contract(target: &str) -> Result<(&'static str, &'static str), String> {
    platform_contract()?
        .iter()
        .find(|entry| entry.target == target)
        .map(|entry| (entry.archive.as_str(), entry.executable.as_str()))
        .ok_or_else(|| format!("unsupported AdocWeave distribution target: {target}"))
}

struct PlatformContract {
    architecture: String,
    archive: String,
    executable: String,
    _minimum_os_version: Option<String>,
    os: String,
    target: String,
}

fn platform_contract() -> Result<&'static [PlatformContract], String> {
    static CONTRACT: OnceLock<Result<Vec<PlatformContract>, String>> = OnceLock::new();
    match CONTRACT.get_or_init(parse_platform_contract) {
        Ok(entries) => Ok(entries),
        Err(error) => Err(error.clone()),
    }
}

fn parse_platform_contract() -> Result<Vec<PlatformContract>, String> {
    let root: zed_extension_api::serde_json::Value =
        zed_extension_api::serde_json::from_str(PLATFORM_CONTRACT)
            .map_err(|error| format!("invalid platform contract: {error}"))?;
    let object = root
        .as_object()
        .ok_or_else(|| "platform contract root is not an object".to_owned())?;
    if object.keys().map(String::as_str).collect::<HashSet<_>>()
        != HashSet::from(["schemaVersion", "supported", "unsupported"])
        || root.get("schemaVersion").and_then(|value| value.as_u64()) != Some(1)
    {
        return Err("unsupported platform contract schema".to_owned());
    }
    let entries = root
        .get("supported")
        .and_then(|value| value.as_array())
        .ok_or_else(|| "platform contract has no supported list".to_owned())?
        .iter()
        .map(|entry| {
            let object = entry
                .as_object()
                .ok_or_else(|| "platform contract entry is not an object".to_owned())?;
            if object.keys().map(String::as_str).collect::<HashSet<_>>()
                != HashSet::from([
                    "architecture",
                    "archive",
                    "executable",
                    "minimumOsVersion",
                    "os",
                    "target",
                ])
            {
                return Err("platform contract entry has unsupported fields".to_owned());
            }
            let field = |name| {
                entry
                    .get(name)
                    .and_then(|value| value.as_str())
                    .map(str::to_owned)
                    .ok_or_else(|| format!("platform contract entry has no {name}"))
            };
            Ok(PlatformContract {
                architecture: field("architecture")?,
                archive: field("archive")?,
                executable: field("executable")?,
                _minimum_os_version: match entry.get("minimumOsVersion") {
                    Some(value) if value.is_null() => None,
                    Some(value) => Some(
                        value
                            .as_str()
                            .filter(|value| !value.is_empty())
                            .ok_or_else(|| {
                                "platform contract entry has invalid minimumOsVersion".to_owned()
                            })?
                            .to_owned(),
                    ),
                    None => {
                        return Err("platform contract entry has no minimumOsVersion".to_owned())
                    }
                },
                os: field("os")?,
                target: field("target")?,
            })
        })
        .collect::<Result<Vec<_>, String>>()?;
    let mut platform_keys = HashSet::new();
    let mut targets = HashSet::new();
    for entry in &entries {
        if !platform_keys.insert((&entry.os, &entry.architecture)) || !targets.insert(&entry.target)
        {
            return Err("platform contract contains duplicate entries".to_owned());
        }
    }
    Ok(entries)
}

pub fn select_lsp_asset(
    manifest: &str,
    version: &str,
    target: &str,
) -> Result<ReleaseAsset, String> {
    let root: zed_extension_api::serde_json::Value =
        zed_extension_api::serde_json::from_str(manifest)
            .map_err(|error| format!("invalid distribution manifest: {error}"))?;
    if root.get("schemaVersion").and_then(|value| value.as_u64()) != Some(2) {
        return Err("unsupported distribution manifest schema".to_owned());
    }
    if root.get("packageVersion").and_then(|value| value.as_str()) != Some(version) {
        return Err(format!(
            "distribution manifest does not describe AdocWeave {version}"
        ));
    }
    let source_commit = root
        .get("sourceCommit")
        .and_then(|value| value.as_str())
        .filter(|value| value.len() == 40 && value.bytes().all(|byte| byte.is_ascii_hexdigit()))
        .ok_or_else(|| "distribution manifest has no valid source commit".to_owned())?
        .to_ascii_lowercase();

    let (archive, executable) = target_asset_contract(target)?;
    let expected_name = format!("adocweave-lsp-{target}.{archive}");
    let matches = root
        .get("assets")
        .and_then(|value| value.as_array())
        .ok_or_else(|| "distribution manifest has no asset list".to_owned())?
        .iter()
        .filter(|asset| {
            asset.get("kind").and_then(|value| value.as_str()) == Some("lsp")
                && asset.get("target").and_then(|value| value.as_str()) == Some(target)
        })
        .collect::<Vec<_>>();
    let [asset] = matches.as_slice() else {
        return Err(format!(
            "distribution manifest must contain exactly one LSP asset for {target}"
        ));
    };
    if asset.get("name").and_then(|value| value.as_str()) != Some(expected_name.as_str())
        || asset.get("archive").and_then(|value| value.as_str()) != Some(archive)
        || asset.get("executable").and_then(|value| value.as_str()) != Some(executable)
    {
        return Err(format!("invalid LSP asset contract for {target}"));
    }
    let sha256 = asset
        .get("sha256")
        .and_then(|value| value.as_str())
        .filter(|value| value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit()))
        .ok_or_else(|| format!("invalid SHA-256 for {expected_name}"))?
        .to_ascii_lowercase();
    let byte_size = asset
        .get("byteSize")
        .and_then(|value| value.as_u64())
        .filter(|value| *value > 0)
        .ok_or_else(|| format!("invalid byte size for {expected_name}"))?;

    Ok(ReleaseAsset {
        name: expected_name,
        sha256,
        byte_size,
        archive,
        executable,
        source_commit,
    })
}

pub fn sha256_file(path: &Path) -> Result<String, String> {
    let file =
        File::open(path).map_err(|error| format!("failed to open {}: {error}", path.display()))?;
    let mut reader = BufReader::new(file);
    let mut digest = Sha256::new();
    let mut buffer = [0; 8192];
    loop {
        let read = reader
            .read(&mut buffer)
            .map_err(|error| format!("failed to hash {}: {error}", path.display()))?;
        if read == 0 {
            break;
        }
        digest.update(&buffer[..read]);
    }
    Ok(digest
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect())
}

pub fn verify_download(path: &Path, asset: &ReleaseAsset) -> Result<(), String> {
    let actual_size = fs::metadata(path)
        .map_err(|error| format!("failed to inspect {}: {error}", path.display()))?
        .len();
    if actual_size != asset.byte_size {
        return Err(format!(
            "downloaded {} has byte size {actual_size}, expected {}",
            asset.name, asset.byte_size
        ));
    }
    let actual_hash = sha256_file(path)?;
    if actual_hash != asset.sha256 {
        return Err(format!(
            "downloaded {} failed SHA-256 verification",
            asset.name
        ));
    }
    Ok(())
}

pub fn extract_binary(
    archive_path: &Path,
    binary_path: &Path,
    _target: &str,
    asset: &ReleaseAsset,
) -> Result<(), String> {
    if asset.archive != "zip" {
        return Err(format!("unsupported LSP archive format: {}", asset.archive));
    }
    extract_zip_binary(archive_path, binary_path, asset)
}

fn extract_zip_binary(
    archive_path: &Path,
    binary_path: &Path,
    asset: &ReleaseAsset,
) -> Result<(), String> {
    let input = File::open(archive_path)
        .map(BufReader::new)
        .map_err(|error| format!("failed to open {}: {error}", archive_path.display()))?;
    let mut archive =
        zip::ZipArchive::new(input).map_err(|error| format!("invalid LSP zip archive: {error}"))?;
    let expected = asset.executable;
    let mut found = false;
    let mut total = 0_u64;
    let mut names = HashSet::new();
    for index in 0..archive.len() {
        let mut entry = archive
            .by_index(index)
            .map_err(|error| format!("invalid LSP zip entry: {error}"))?;
        let name = entry.name().replace('\\', "/");
        let normalized = name.trim_end_matches('/');
        if name.starts_with('/')
            || name.contains(':')
            || normalized.is_empty()
            || normalized
                .split('/')
                .any(|component| component.is_empty() || component == "." || component == "..")
            || !names.insert(normalized.to_ascii_lowercase())
            || entry
                .unix_mode()
                .is_some_and(|mode| mode & 0o170000 == 0o120000)
        {
            return Err(format!("unsafe path in LSP archive: {name}"));
        }
        total = total
            .checked_add(entry.size())
            .filter(|value| *value <= MAX_DECOMPRESSED_ARCHIVE_BYTES)
            .ok_or_else(|| "decompressed archive exceeds size limit".to_owned())?;
        if name == expected {
            if found || entry.is_dir() || entry.size() == 0 || entry.size() > MAX_BINARY_BYTES {
                return Err("invalid adocweave-lsp entry in release archive".to_owned());
            }
            let output = File::create(binary_path)
                .map_err(|error| format!("failed to create {}: {error}", binary_path.display()))?;
            let copied = io::copy(&mut entry, &mut BufWriter::new(output))
                .map_err(|error| format!("failed to extract adocweave-lsp: {error}"))?;
            if copied != entry.size() {
                return Err("release archive contains an incomplete adocweave-lsp".to_owned());
            }
            found = true;
        }
    }
    if !found {
        return Err("release archive does not contain adocweave-lsp".to_owned());
    }
    Ok(())
}

pub fn cache_paths(version: &str, target: &str) -> CachePaths {
    let key = format!("adocweave-lsp-{version}-{target}");
    let executable = target_asset_contract(target)
        .map(|(_, executable)| executable)
        .unwrap_or("adocweave-lsp");
    CachePaths {
        binary: PathBuf::from(&key).join(executable),
        marker: PathBuf::from(&key).join("verified.json"),
        directory: PathBuf::from(key),
    }
}

#[derive(Debug)]
pub struct CachePaths {
    pub directory: PathBuf,
    pub binary: PathBuf,
    pub marker: PathBuf,
}

pub fn write_marker(
    path: &Path,
    version: &str,
    target: &str,
    asset: &ReleaseAsset,
    binary_hash: &str,
) -> Result<(), String> {
    let marker = zed_extension_api::serde_json::json!({
        "schemaVersion": 1,
        "packageVersion": version,
        "target": target,
        "asset": asset.name,
        "assetByteSize": asset.byte_size,
        "assetSha256": asset.sha256,
        "binarySha256": binary_hash,
        "sourceCommit": asset.source_commit,
    });
    fs::write(path, marker.to_string())
        .map_err(|error| format!("failed to write cache marker: {error}"))
}

pub fn verified_cache(paths: &CachePaths, version: &str, target: &str) -> bool {
    let Ok(marker) = fs::read_to_string(&paths.marker) else {
        return false;
    };
    let Ok(marker) =
        zed_extension_api::serde_json::from_str::<zed_extension_api::serde_json::Value>(&marker)
    else {
        return false;
    };
    let Ok(binary_hash) = sha256_file(&paths.binary) else {
        return false;
    };
    let expected_asset = format!("adocweave-lsp-{target}.zip");
    marker.as_object().is_some_and(|fields| fields.len() == 8)
        && marker.get("schemaVersion").and_then(|value| value.as_u64()) == Some(1)
        && marker
            .get("packageVersion")
            .and_then(|value| value.as_str())
            == Some(version)
        && marker.get("target").and_then(|value| value.as_str()) == Some(target)
        && marker.get("asset").and_then(|value| value.as_str()) == Some(expected_asset.as_str())
        && marker
            .get("assetByteSize")
            .and_then(|value| value.as_u64())
            .is_some_and(|value| value > 0)
        && marker
            .get("assetSha256")
            .and_then(|value| value.as_str())
            .is_some_and(|value| {
                value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
            })
        && marker
            .get("sourceCommit")
            .and_then(|value| value.as_str())
            .is_some_and(|value| {
                value.len() == 40 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
            })
        && marker.get("binarySha256").and_then(|value| value.as_str()) == Some(binary_hash.as_str())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;
    use std::io::Write;

    fn manifest(sha256: &str, byte_size: u64) -> String {
        format!(
            r#"{{"schemaVersion":2,"packageVersion":"0.1.0-rc.1","sourceCommit":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","assets":[{{"archive":"zip","byteSize":{byte_size},"executable":"adocweave-lsp","kind":"lsp","name":"adocweave-lsp-x86_64-unknown-linux-musl.zip","sha256":"{sha256}","target":"x86_64-unknown-linux-musl"}}]}}"#
        )
    }

    #[test]
    fn every_supported_zed_platform_has_a_distribution_target() {
        use zed_extension_api::{Architecture, Os};

        for (os, arch, target) in [
            (
                Os::Linux,
                Architecture::Aarch64,
                "aarch64-unknown-linux-musl",
            ),
            (Os::Linux, Architecture::X8664, "x86_64-unknown-linux-musl"),
            (Os::Mac, Architecture::Aarch64, "aarch64-apple-darwin"),
            (Os::Mac, Architecture::X8664, "x86_64-apple-darwin"),
            (Os::Windows, Architecture::X8664, "x86_64-pc-windows-msvc"),
        ] {
            assert_eq!(target_for_platform(os, arch).unwrap(), target);
            assert!(target_asset_contract(target).is_ok());
        }
        assert!(target_for_platform(Os::Windows, Architecture::Aarch64).is_err());
    }

    #[test]
    fn manifest_selection_requires_the_exact_release_contract() {
        let hash = "a".repeat(64);
        let asset = select_lsp_asset(
            &manifest(&hash, 42),
            "0.1.0-rc.1",
            "x86_64-unknown-linux-musl",
        )
        .unwrap();
        assert_eq!(asset.name, "adocweave-lsp-x86_64-unknown-linux-musl.zip");
        assert_eq!(asset.sha256, hash);
        assert!(select_lsp_asset(
            &manifest(&"b".repeat(63), 42),
            "0.1.0-rc.1",
            "x86_64-unknown-linux-musl"
        )
        .is_err());
        assert!(select_lsp_asset(
            &manifest(&"b".repeat(64), 0),
            "0.1.0-rc.1",
            "x86_64-unknown-linux-musl"
        )
        .is_err());
        assert!(select_lsp_asset(
            &manifest(&"b".repeat(64), 42),
            "0.2.0",
            "x86_64-unknown-linux-musl"
        )
        .is_err());
    }

    #[test]
    fn hash_mismatch_is_rejected_before_extraction() {
        let root = std::env::temp_dir().join(format!("adocweave-zed-hash-{}", std::process::id()));
        let _ = fs::remove_file(&root);
        fs::write(&root, b"archive").unwrap();
        let asset = ReleaseAsset {
            name: "asset.zip".to_owned(),
            sha256: "0".repeat(64),
            byte_size: 7,
            archive: "zip",
            executable: "adocweave-lsp",
            source_commit: "a".repeat(40),
        };
        assert!(verify_download(&root, &asset)
            .unwrap_err()
            .contains("SHA-256"));
        fs::remove_file(root).unwrap();
    }

    #[test]
    fn hash_empty_file_has_the_standard_sha256_encoding() {
        let root =
            std::env::temp_dir().join(format!("adocweave-zed-empty-hash-{}", std::process::id()));
        let _ = fs::remove_file(&root);
        fs::write(&root, []).unwrap();
        assert_eq!(
            sha256_file(&root).unwrap(),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        fs::remove_file(root).unwrap();
    }

    #[test]
    fn cache_requires_an_untampered_binary() {
        let root = std::env::temp_dir().join(format!("adocweave-zed-cache-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        let paths = CachePaths {
            directory: root.clone(),
            binary: root.join("adocweave-lsp"),
            marker: root.join("verified.json"),
        };
        fs::write(&paths.binary, b"binary").unwrap();
        let asset = ReleaseAsset {
            name: "adocweave-lsp-x86_64-unknown-linux-musl.zip".to_owned(),
            sha256: "a".repeat(64),
            byte_size: 1,
            archive: "zip",
            executable: "adocweave-lsp",
            source_commit: "a".repeat(40),
        };
        let hash = sha256_file(&paths.binary).unwrap();
        write_marker(
            &paths.marker,
            "0.1.0-rc.1",
            "x86_64-unknown-linux-musl",
            &asset,
            &hash,
        )
        .unwrap();
        assert!(verified_cache(
            &paths,
            "0.1.0-rc.1",
            "x86_64-unknown-linux-musl"
        ));
        fs::write(&paths.binary, b"tampered").unwrap();
        assert!(!verified_cache(
            &paths,
            "0.1.0-rc.1",
            "x86_64-unknown-linux-musl"
        ));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn zip_extraction_selects_the_windows_binary() {
        let root =
            std::env::temp_dir().join(format!("adocweave-zed-zip-extract-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        let archive_path = root.join("archive.zip");
        let mut bytes = Cursor::new(Vec::new());
        {
            let mut archive = zip::ZipWriter::new(&mut bytes);
            archive
                .start_file(
                    "adocweave-lsp.exe",
                    zip::write::SimpleFileOptions::default(),
                )
                .unwrap();
            archive.write_all(b"windows-lsp").unwrap();
            archive.finish().unwrap();
        }
        fs::write(&archive_path, bytes.into_inner()).unwrap();
        let binary = root.join("adocweave-lsp.exe");
        let asset = ReleaseAsset {
            name: "adocweave-lsp-x86_64-pc-windows-msvc.zip".to_owned(),
            sha256: "a".repeat(64),
            byte_size: fs::metadata(&archive_path).unwrap().len(),
            archive: "zip",
            executable: "adocweave-lsp.exe",
            source_commit: "a".repeat(40),
        };
        extract_binary(&archive_path, &binary, "x86_64-pc-windows-msvc", &asset).unwrap();
        assert_eq!(fs::read(&binary).unwrap(), b"windows-lsp");
        fs::remove_dir_all(root).unwrap();
    }
}
