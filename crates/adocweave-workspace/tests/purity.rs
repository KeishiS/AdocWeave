//! The workspace state machine stays runtime- and I/O-independent: it must not
//! depend on the host crate or reach the filesystem, and the host keeps
//! providing the scan entry point the adapters build on.

use std::fs;

fn repository_root() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn collect_rust_files(directory: &std::path::Path, files: &mut Vec<std::path::PathBuf>) {
    if !directory.exists() {
        return;
    }
    for entry in fs::read_dir(directory).expect("Rust module directory") {
        let path = entry.expect("Rust module entry").path();
        if path.is_dir() {
            collect_rust_files(&path, files);
        } else if path.extension().is_some_and(|extension| extension == "rs") {
            files.push(path);
        }
    }
}

#[test]
fn workspace_state_has_no_filesystem_or_host_dependency() {
    let root = repository_root();
    let manifest = fs::read_to_string(root.join("crates/adocweave-workspace/Cargo.toml"))
        .expect("workspace manifest");
    let source =
        fs::read_to_string(root.join("crates/adocweave-workspace/src/lib.rs")).expect("workspace");
    let mut host_files = Vec::new();
    collect_rust_files(&root.join("crates/adocweave-host/src"), &mut host_files);
    host_files.sort();
    let host = host_files
        .iter()
        .map(|path| fs::read_to_string(path).expect("host source"))
        .collect::<Vec<_>>()
        .join("\n");

    assert!(!manifest.contains("adocweave-host"));
    for forbidden in [
        "std::fs",
        "LocalFilesystemPolicy",
        "LocalFilesystemSession",
        "LogicalSourceId",
    ] {
        assert!(!source.contains(forbidden), "{forbidden}");
    }
    assert!(host.contains("pub fn scan_utf8("));
}
