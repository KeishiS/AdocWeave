use std::collections::BTreeSet;
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
struct IssueManifest {
    milestone: String,
    issues: Vec<IssueEntry>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct IssueEntry {
    id: String,
    status: String,
    depends_on: Vec<String>,
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
fn m12_issue_status_and_dependencies_are_consistent() {
    let manifest: IssueManifest = serde_json::from_str(
        &fs::read_to_string(repository_root().join("issues/m12.json")).expect("M12 manifest"),
    )
    .expect("valid M12 manifest");
    assert_eq!(manifest.milestone, "M12");
    let mut seen = BTreeSet::new();
    for issue in manifest.issues {
        assert!(
            seen.insert(issue.id.clone()),
            "duplicate issue {}",
            issue.id
        );
        assert!(matches!(issue.status.as_str(), "in-progress" | "completed"));
        for dependency in &issue.depends_on {
            assert!(
                seen.contains(dependency),
                "{} precedes dependency {dependency}",
                issue.id
            );
        }
        let prefix = format!("issues/{}-", issue.id);
        let path = fs::read_dir(repository_root().join("issues"))
            .expect("issues directory")
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .find(|path| path.to_string_lossy().contains(&prefix))
            .unwrap_or_else(|| panic!("missing issue {}", issue.id));
        let source = fs::read_to_string(path).expect("issue source");
        assert!(
            source.contains(&format!(":status: {}", issue.status)),
            "{} status",
            issue.id
        );
        let dependencies = if issue.depends_on.is_empty() {
            ":depends-on:".to_owned()
        } else {
            format!(":depends-on: {}", issue.depends_on.join(","))
        };
        assert!(source.contains(&dependencies), "{} dependencies", issue.id);
    }
}

#[test]
fn maintained_issue_headers_have_valid_status_and_dependencies() {
    let issue_dir = repository_root().join("issues");
    let mut issues = std::collections::BTreeMap::new();
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
        if id.parse::<u16>().is_ok_and(|id| id >= 52) {
            let id = id.to_owned();
            assert!(
                issues.insert(id.clone(), path).is_none(),
                "duplicate issue {id}"
            );
        }
    }
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
        let dependencies = source
            .lines()
            .find_map(|line| line.strip_prefix(":depends-on:"))
            .unwrap_or_else(|| panic!("issue {id} has no dependencies"));
        for dependency in dependencies
            .split(',')
            .map(str::trim)
            .filter(|item| !item.is_empty())
        {
            assert!(
                issue_dir
                    .read_dir()
                    .expect("issues directory")
                    .filter_map(Result::ok)
                    .any(|entry| entry
                        .file_name()
                        .to_string_lossy()
                        .starts_with(&format!("{dependency}-"))),
                "issue {id} depends on missing issue {dependency}"
            );
        }
    }
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
        let mut open = false;
        let mut previous_nonempty = "";
        for (line, text) in source.lines().enumerate() {
            if text.trim() != "|===" {
                if !text.trim().is_empty() {
                    previous_nonempty = text.trim();
                }
                continue;
            }
            let starts_table = previous_nonempty.starts_with("[cols=")
                || previous_nonempty.starts_with("[options=");
            if starts_table {
                assert!(!open, "{path}: nested table at line {}", line + 1);
                open = true;
            } else {
                assert!(open, "{path}: stray table close at line {}", line + 1);
                open = false;
            }
            previous_nonempty = text.trim();
        }
        assert!(!open, "{path}: unclosed table");
    }
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
