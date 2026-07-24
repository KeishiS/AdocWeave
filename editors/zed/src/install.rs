use sha2::{Digest, Sha256};
use std::{
    fs::{self, File},
    io::{self, BufReader, BufWriter, Read, Write},
    path::{Component, Path, PathBuf},
};

pub const REPOSITORY: &str = "KeishiS/AdocWeave";
pub const MANIFEST_NAME: &str = "adocweave-dist-manifest.json";
const MAX_DECOMPRESSED_ARCHIVE_BYTES: u64 = 128 * 1024 * 1024;
const MAX_BINARY_BYTES: u64 = 64 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReleaseAsset {
    pub name: String,
    pub sha256: String,
    pub byte_size: u64,
}

pub fn target_for_platform(
    os: zed_extension_api::Os,
    arch: zed_extension_api::Architecture,
) -> Result<&'static str, String> {
    use zed_extension_api::{Architecture, Os};

    match (os, arch) {
        (Os::Linux, Architecture::X8664) => Ok("x86_64-unknown-linux-musl"),
        (Os::Linux, Architecture::Aarch64) => Ok("aarch64-unknown-linux-musl"),
        (Os::Linux, Architecture::X86) => {
            Err("AdocWeave LSP does not support 32-bit Linux".to_owned())
        }
        (Os::Mac, _) => Err("AdocWeave LSP for macOS is not published yet".to_owned()),
        (Os::Windows, _) => Err("AdocWeave LSP for Windows is not published yet".to_owned()),
    }
}

pub fn select_lsp_asset(
    manifest: &str,
    version: &str,
    target: &str,
) -> Result<ReleaseAsset, String> {
    let root: zed_extension_api::serde_json::Value =
        zed_extension_api::serde_json::from_str(manifest)
            .map_err(|error| format!("invalid distribution manifest: {error}"))?;
    if root.get("schemaVersion").and_then(|value| value.as_u64()) != Some(1) {
        return Err("unsupported distribution manifest schema".to_owned());
    }
    if root.get("packageVersion").and_then(|value| value.as_str()) != Some(version) {
        return Err(format!(
            "distribution manifest does not describe AdocWeave {version}"
        ));
    }

    let expected_name = format!("adocweave-lsp-{target}.tar.xz");
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
        || asset.get("archive").and_then(|value| value.as_str()) != Some("tar.xz")
        || asset.get("executable").and_then(|value| value.as_str()) != Some("adocweave-lsp")
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
    tar_path: &Path,
    binary_path: &Path,
    target: &str,
) -> Result<(), String> {
    let input = File::open(archive_path)
        .map(BufReader::new)
        .map_err(|error| format!("failed to open {}: {error}", archive_path.display()))?;
    let output = File::create(tar_path)
        .map(BufWriter::new)
        .map_err(|error| format!("failed to create {}: {error}", tar_path.display()))?;
    let mut limited = LimitedWriter::new(output, MAX_DECOMPRESSED_ARCHIVE_BYTES);
    lzma_rs::xz_decompress(&mut BufReader::new(input), &mut limited)
        .map_err(|error| format!("failed to decompress {}: {error}", archive_path.display()))?;
    limited.finish()?;

    let expected = PathBuf::from(format!("adocweave-lsp-{target}/adocweave-lsp"));
    let tar_file =
        File::open(tar_path).map_err(|error| format!("failed to open extracted tar: {error}"))?;
    let mut archive = tar::Archive::new(BufReader::new(tar_file));
    let mut found = false;
    for entry in archive
        .entries()
        .map_err(|error| format!("invalid LSP tar archive: {error}"))?
    {
        let mut entry = entry.map_err(|error| format!("invalid LSP tar entry: {error}"))?;
        let path = entry
            .path()
            .map_err(|error| format!("invalid LSP tar path: {error}"))?;
        if path
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
        {
            return Err(format!("unsafe path in LSP archive: {}", path.display()));
        }
        if path == expected {
            if found || !entry.header().entry_type().is_file() || entry.size() > MAX_BINARY_BYTES {
                return Err("invalid adocweave-lsp entry in release archive".to_owned());
            }
            let output = File::create(binary_path)
                .map_err(|error| format!("failed to create {}: {error}", binary_path.display()))?;
            let copied = io::copy(&mut entry, &mut BufWriter::new(output))
                .map_err(|error| format!("failed to extract adocweave-lsp: {error}"))?;
            if copied == 0 || copied != entry.size() {
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
    CachePaths {
        binary: PathBuf::from(&key).join("adocweave-lsp"),
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
        "assetSha256": asset.sha256,
        "binarySha256": binary_hash,
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
    marker.get("schemaVersion").and_then(|value| value.as_u64()) == Some(1)
        && marker
            .get("packageVersion")
            .and_then(|value| value.as_str())
            == Some(version)
        && marker.get("target").and_then(|value| value.as_str()) == Some(target)
        && marker.get("binarySha256").and_then(|value| value.as_str()) == Some(binary_hash.as_str())
}

struct LimitedWriter<W> {
    inner: W,
    remaining: u64,
}

impl<W> LimitedWriter<W> {
    fn new(inner: W, limit: u64) -> Self {
        Self {
            inner,
            remaining: limit,
        }
    }
}

impl<W: Write> LimitedWriter<W> {
    fn finish(mut self) -> Result<(), String> {
        self.inner
            .flush()
            .map_err(|error| format!("failed to flush decompressed archive: {error}"))
    }
}

impl<W: Write> Write for LimitedWriter<W> {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        if buffer.len() as u64 > self.remaining {
            return Err(io::Error::new(
                io::ErrorKind::FileTooLarge,
                "decompressed archive exceeds size limit",
            ));
        }
        let written = self.inner.write(buffer)?;
        self.remaining -= written as u64;
        Ok(written)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn manifest(sha256: &str, byte_size: u64) -> String {
        format!(
            r#"{{"schemaVersion":1,"packageVersion":"0.1.0-rc.1","assets":[{{"archive":"tar.xz","byteSize":{byte_size},"executable":"adocweave-lsp","kind":"lsp","name":"adocweave-lsp-x86_64-unknown-linux-musl.tar.xz","sha256":"{sha256}","target":"x86_64-unknown-linux-musl"}}]}}"#
        )
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
        assert_eq!(asset.name, "adocweave-lsp-x86_64-unknown-linux-musl.tar.xz");
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
            name: "asset.tar.xz".to_owned(),
            sha256: "0".repeat(64),
            byte_size: 7,
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
            name: "asset.tar.xz".to_owned(),
            sha256: "a".repeat(64),
            byte_size: 1,
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
    fn xz_tar_extraction_selects_only_the_expected_binary() {
        let root =
            std::env::temp_dir().join(format!("adocweave-zed-extract-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        let mut tar_bytes = Vec::new();
        {
            let mut builder = tar::Builder::new(&mut tar_bytes);
            let body = b"lsp-binary";
            let mut header = tar::Header::new_gnu();
            header.set_size(body.len() as u64);
            header.set_mode(0o755);
            header.set_cksum();
            builder
                .append_data(
                    &mut header,
                    "adocweave-lsp-x86_64-unknown-linux-musl/adocweave-lsp",
                    &body[..],
                )
                .unwrap();
            builder.finish().unwrap();
        }
        let mut compressed = Vec::new();
        lzma_rs::xz_compress(&mut BufReader::new(tar_bytes.as_slice()), &mut compressed).unwrap();
        let archive = root.join("archive.tar.xz");
        let tar = root.join("archive.tar");
        let binary = root.join("adocweave-lsp");
        fs::write(&archive, compressed).unwrap();
        extract_binary(&archive, &tar, &binary, "x86_64-unknown-linux-musl").unwrap();
        assert_eq!(fs::read(&binary).unwrap(), b"lsp-binary");
        fs::remove_dir_all(root).unwrap();
    }
}
