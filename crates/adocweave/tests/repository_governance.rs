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
