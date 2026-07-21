use std::collections::BTreeMap;
use std::fs;
use std::process::Command;

use adocweave::{Engine, ParseOptions};
use serde::Deserialize;

#[derive(Deserialize)]
struct CorpusManifest {
    normative: Vec<String>,
    abnormal: Vec<AbnormalCase>,
}

#[derive(Deserialize)]
struct AbnormalCase {
    path: String,
    codes: Vec<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ReleaseManifest {
    contracts: ContractCatalog,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ContractCatalog {
    core_profile: u16,
    core_api: u16,
    html: u16,
    projection: u16,
    conformance: u16,
    wasm_api: u16,
}

fn repository_root() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn analyze(path: &str) -> adocweave::Analysis {
    let source = fs::read_to_string(repository_root().join(path))
        .unwrap_or_else(|error| panic!("{path}: {error}"));
    Engine::new(ParseOptions::default())
        .analyze(&source)
        .unwrap_or_else(|error| panic!("{path}: {error}"))
}

fn manifest() -> CorpusManifest {
    serde_json::from_str(
        &fs::read_to_string(repository_root().join("docs/corpus.json")).expect("corpus manifest"),
    )
    .expect("valid corpus manifest")
}

fn validate_issue_dependencies(
    metadata: &BTreeMap<String, (String, Vec<String>)>,
) -> Result<(), String> {
    for (id, (status, dependencies)) in metadata {
        for dependency in dependencies {
            if !metadata.contains_key(dependency) {
                return Err(format!("issue {id} depends on missing issue {dependency}"));
            }
            if dependency >= id {
                return Err(format!(
                    "issue {id} has a cyclic or forward dependency {dependency}"
                ));
            }
            if status == "completed"
                && metadata.get(dependency).map(|value| value.0.as_str()) != Some("completed")
            {
                return Err(format!(
                    "completed issue {id} depends on unfinished issue {dependency}"
                ));
            }
        }
    }
    Ok(())
}

fn validate_table_delimiters(path: &str, source: &str) -> Result<(), String> {
    let mut open = false;
    let mut previous_nonempty = "";
    for (line, text) in source.lines().enumerate() {
        if text.trim() != "|===" {
            if !text.trim().is_empty() {
                previous_nonempty = text.trim();
            }
            continue;
        }
        let starts_table =
            previous_nonempty.starts_with("[cols=") || previous_nonempty.starts_with("[options=");
        if starts_table {
            if open {
                return Err(format!("{path}: nested table at line {}", line + 1));
            }
            open = true;
        } else if !open {
            return Err(format!("{path}: stray table close at line {}", line + 1));
        } else {
            open = false;
        }
        previous_nonempty = text.trim();
    }
    if open {
        Err(format!("{path}: unclosed table"))
    } else {
        Ok(())
    }
}

#[test]
fn tracked_adoc_corpus_is_lossless_and_has_valid_ranges() {
    let output = Command::new("git")
        .args(["ls-files", "-z", "*.adoc"])
        .current_dir(repository_root())
        .output()
        .expect("git ls-files");
    assert!(output.status.success());
    for path in output
        .stdout
        .split(|byte| *byte == 0)
        .filter(|path| !path.is_empty())
    {
        let path = std::str::from_utf8(path).expect("UTF-8 repository path");
        let analysis = analyze(path);
        assert_eq!(analysis.syntax().reconstruct(), analysis.source(), "{path}");
        for diagnostic in analysis.diagnostics() {
            let range = diagnostic.range;
            assert!(range.start() <= range.end(), "{path}: {range:?}");
            assert!(
                range.end().to_usize() <= analysis.source().len(),
                "{path}: {range:?}"
            );
            assert!(
                analysis.source().is_char_boundary(range.start().to_usize()),
                "{path}"
            );
            assert!(
                analysis.source().is_char_boundary(range.end().to_usize()),
                "{path}"
            );
        }
    }
}

#[test]
fn normative_documents_have_no_diagnostics() {
    for path in manifest().normative {
        let analysis = analyze(&path);
        assert!(
            analysis.diagnostics().is_empty(),
            "{path}: {:?}",
            analysis.diagnostics()
        );
    }
}

#[test]
fn abnormal_fixtures_match_their_diagnostic_manifest() {
    for case in manifest().abnormal {
        let actual: Vec<_> = analyze(&case.path)
            .diagnostics()
            .iter()
            .map(|diagnostic| diagnostic.code.as_str().to_owned())
            .collect();
        assert_eq!(actual, case.codes, "{}", case.path);
    }
}

#[test]
fn every_issue_header_status_dependency_and_roadmap_entry_is_consistent() {
    let issue_dir = repository_root().join("issues");
    let mut issues = BTreeMap::new();
    for path in fs::read_dir(&issue_dir)
        .expect("issues directory")
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.extension().and_then(|value| value.to_str()) == Some("adoc"))
    {
        let Some(file_name) = path.file_name().and_then(|value| value.to_str()) else {
            continue;
        };
        let Some((id, _)) = file_name.split_once('-') else {
            continue;
        };
        if id.parse::<u16>().is_ok() {
            let id = id.to_owned();
            assert!(
                issues.insert(id.clone(), path).is_none(),
                "duplicate issue {id}"
            );
        }
    }
    assert_eq!(
        issues.first_key_value().map(|(id, _)| id.as_str()),
        Some("001")
    );
    for (expected, (id, _)) in (1_u16..).zip(&issues) {
        assert_eq!(
            id,
            &format!("{expected:03}"),
            "issue IDs must be contiguous"
        );
    }
    let mut metadata = BTreeMap::new();
    for (id, path) in &issues {
        let source = fs::read_to_string(path).expect("issue source");
        let status = source
            .lines()
            .find_map(|line| line.strip_prefix(":status: "))
            .unwrap_or_else(|| panic!("issue {id} has no status"));
        assert!(
            matches!(status, "planned" | "in-progress" | "completed"),
            "issue {id} has invalid status {status}"
        );
        let dependencies: Vec<_> = source
            .lines()
            .find_map(|line| line.strip_prefix(":depends-on:"))
            .unwrap_or_else(|| panic!("issue {id} has no dependencies"))
            .split(',')
            .map(str::trim)
            .filter(|item| !item.is_empty())
            .map(str::to_owned)
            .collect();
        metadata.insert(id.clone(), (status.to_owned(), dependencies));
    }
    validate_issue_dependencies(&metadata).unwrap_or_else(|error| panic!("{error}"));

    let roadmap = fs::read_to_string(issue_dir.join("README.adoc")).expect("issue roadmap");
    for (id, (status, _)) in &metadata {
        let marker = format!("xref:{id}-");
        assert!(
            roadmap.contains(&marker),
            "issue {id} is absent from roadmap"
        );
        let expected = match status.as_str() {
            "planned" => "（計画）",
            "in-progress" => "（進行中）",
            "completed" => "（完了）",
            _ => unreachable!(),
        };
        for line in roadmap.lines().filter(|line| line.contains(&marker)) {
            if ["（計画）", "（進行中）", "（完了）"]
                .iter()
                .any(|marker| line.contains(marker))
            {
                assert!(
                    line.contains(expected),
                    "issue {id} roadmap status differs: {line}"
                );
            }
        }
    }
}

#[test]
fn issue_governance_validator_rejects_missing_forward_and_unfinished_dependencies() {
    let mut metadata = BTreeMap::from([
        ("001".to_owned(), ("completed".to_owned(), Vec::new())),
        (
            "002".to_owned(),
            ("completed".to_owned(), vec!["003".to_owned()]),
        ),
        ("003".to_owned(), ("planned".to_owned(), Vec::new())),
    ]);
    assert!(
        validate_issue_dependencies(&metadata)
            .expect_err("forward dependency")
            .contains("forward")
    );

    metadata.get_mut("002").expect("issue").1 = vec!["999".to_owned()];
    assert!(
        validate_issue_dependencies(&metadata)
            .expect_err("missing dependency")
            .contains("missing")
    );

    metadata.get_mut("002").expect("issue").1 = vec!["001".to_owned()];
    metadata.get_mut("001").expect("issue").0 = "planned".to_owned();
    assert!(
        validate_issue_dependencies(&metadata)
            .expect_err("unfinished dependency")
            .contains("unfinished")
    );
}

#[test]
fn contract_tables_are_not_nested_or_unclosed() {
    for path in [
        "docs/current-contract.adoc",
        "docs/syntax-support.adoc",
        "docs/compatibility.adoc",
        "docs/substitutions.adoc",
        "docs/html-contract.adoc",
    ] {
        let source = fs::read_to_string(repository_root().join(path)).expect("contract document");
        validate_table_delimiters(path, &source).unwrap_or_else(|error| panic!("{error}"));
    }
}

#[test]
fn table_governance_validator_rejects_nested_stray_and_unclosed_delimiters() {
    assert!(
        validate_table_delimiters("nested", "[cols=\"1\"]\n|===\n[cols=\"1\"]\n|===\n")
            .expect_err("nested")
            .contains("nested")
    );
    assert!(
        validate_table_delimiters("stray", "text\n|===\n")
            .expect_err("stray")
            .contains("stray")
    );
    assert!(
        validate_table_delimiters("unclosed", "[cols=\"1\"]\n|===\n|cell\n")
            .expect_err("unclosed")
            .contains("unclosed")
    );
}

#[test]
fn wasm_documentation_uses_the_release_manifest_version() {
    let manifest: serde_json::Value = serde_json::from_str(
        &fs::read_to_string(repository_root().join("release-manifest.json"))
            .expect("release manifest"),
    )
    .expect("valid release manifest");
    let version = manifest["contracts"]["wasmApi"]
        .as_u64()
        .expect("WASM API version");
    let documentation =
        fs::read_to_string(repository_root().join("docs/wasm-worker.adoc")).expect("WASM docs");
    assert!(
        documentation.contains(&format!("`WASM_API_VERSION`は{version}")),
        "WASM version prose"
    );
    assert!(
        documentation.contains(&format!("\"apiVersion\": {version}")),
        "WASM request example"
    );
    assert!(
        documentation.contains(&format!("expected {version}")),
        "WASM error example"
    );
}

#[test]
fn release_manifest_is_the_single_contract_version_catalog() {
    let root = repository_root();
    let manifest: ReleaseManifest = serde_json::from_str(
        &fs::read_to_string(root.join("release-manifest.json")).expect("release manifest"),
    )
    .expect("valid release manifest");
    let contracts = manifest.contracts;
    assert_eq!(contracts.core_profile, adocweave::CORE_PROFILE_VERSION);
    assert_eq!(contracts.core_api, adocweave::CORE_API_VERSION);
    assert_eq!(contracts.html, adocweave::html::HTML_CONTRACT_VERSION);
    assert_eq!(
        contracts.projection,
        adocweave::projection::PROJECTION_CONTRACT_VERSION
    );
    assert_eq!(
        contracts.conformance,
        adocweave::conformance::CONFORMANCE_CONTRACT_VERSION
    );

    let wasm_source =
        fs::read_to_string(root.join("crates/adocweave-wasm/src/lib.rs")).expect("WASM source");
    assert!(wasm_source.contains(&format!(
        "pub const WASM_API_VERSION: u16 = {};",
        contracts.wasm_api
    )));

    let documentation =
        fs::read_to_string(root.join("docs/core-profile.adoc")).expect("contract documentation");
    for expected in [
        format!("`CORE_PROFILE_VERSION = {}`", contracts.core_profile),
        format!("`CORE_API_VERSION = {}`", contracts.core_api),
        format!("`HTML_CONTRACT_VERSION = {}`", contracts.html),
        format!("`PROJECTION_CONTRACT_VERSION = {}`", contracts.projection),
        format!("`CONFORMANCE_CONTRACT_VERSION = {}`", contracts.conformance),
        format!("`WASM_API_VERSION = {}`", contracts.wasm_api),
    ] {
        assert!(documentation.contains(&expected), "missing {expected}");
    }

    let current_contract = fs::read_to_string(root.join("docs/current-contract.adoc"))
        .expect("current contract index");
    for expected in [
        format!("|Core profile |{}", contracts.core_profile),
        format!("|Rust core API |{}", contracts.core_api),
        format!("|HTML |{}", contracts.html),
        format!("|Projection |{}", contracts.projection),
        format!("|Conformance snapshot |{}", contracts.conformance),
        format!("|WASM API |{}", contracts.wasm_api),
    ] {
        assert!(current_contract.contains(&expected), "missing {expected}");
    }
}

#[test]
fn core_package_has_no_native_host_or_runtime_dependency() {
    let root = repository_root();
    let core = fs::read_to_string(root.join("crates/adocweave/Cargo.toml")).expect("core manifest");
    let cli =
        fs::read_to_string(root.join("crates/adocweave-cli/Cargo.toml")).expect("CLI manifest");
    assert!(!core.contains("adocweave-host"));
    assert!(!core.contains("tokio"));
    assert!(cli.contains("adocweave = { path = \"../adocweave\" }"));
    assert!(cli.contains("adocweave-host = { path = \"../adocweave-host\" }"));
}
