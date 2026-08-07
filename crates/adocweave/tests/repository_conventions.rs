//! Conventions this crate promises to the rest of the repository: lint modules
//! stay behind the diagnostic sink, every tracked document parses losslessly,
//! and the published rule catalog matches what the crate re-exports.

use std::collections::BTreeSet;
use std::fs;
use std::process::Command;

use adocweave::{AnalysisOptions, Engine};
use serde::Deserialize;

#[derive(Deserialize)]
struct CorpusManifest {
    normative: Vec<NormativeCase>,
    abnormal: Vec<AbnormalCase>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct NormativeCase {
    path: String,
    ignored_codes: BTreeSet<String>,
}

#[derive(Deserialize)]
struct AbnormalCase {
    path: String,
    codes: Vec<String>,
}

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

fn lint_implementation_files() -> Vec<std::path::PathBuf> {
    let source_root = repository_root().join("crates/adocweave/src");
    let mut files = vec![source_root.join("lint.rs")];
    collect_rust_files(&source_root.join("lint"), &mut files);
    files.sort();
    files
}

#[test]
fn lint_modules_construct_diagnostics_only_inside_the_sink() {
    let source_root = repository_root().join("crates/adocweave/src");
    let mut diagnostic_constructions = Vec::new();
    for path in lint_implementation_files() {
        let source = fs::read_to_string(&path).expect("lint implementation");
        for _ in source.match_indices(concat!("Diagnostic", " {")) {
            diagnostic_constructions.push(path.clone());
        }
    }
    assert_eq!(
        diagnostic_constructions,
        [source_root.join("lint.rs")],
        "Lint rule modules must emit through LintDiagnosticSink"
    );
}

#[test]
fn lint_modules_use_only_interruptible_semantic_traversal() {
    let mut violations = Vec::new();
    for path in lint_implementation_files() {
        let source = fs::read_to_string(&path).expect("lint implementation");
        for forbidden in ["walk_ast", "walk_block_slice"] {
            for (offset, _) in source.match_indices(forbidden) {
                let is_identifier_character =
                    |character: char| character.is_ascii_alphanumeric() || character == '_';
                let starts_identifier = source[..offset]
                    .chars()
                    .next_back()
                    .is_none_or(|character| !is_identifier_character(character));
                let end = offset + forbidden.len();
                let ends_identifier = source[end..]
                    .chars()
                    .next()
                    .is_none_or(|character| !is_identifier_character(character));
                if starts_identifier && ends_identifier {
                    let line = source[..offset]
                        .bytes()
                        .filter(|byte| *byte == b'\n')
                        .count()
                        + 1;
                    violations.push(format!("{}:{line}: {forbidden}", path.display()));
                }
            }
        }
    }
    assert!(
        violations.is_empty(),
        "Lint semantic passes must use interruptible traversal: {violations:?}"
    );
}

fn analyze(path: &str) -> adocweave::Analysis {
    let source = fs::read_to_string(repository_root().join(path))
        .unwrap_or_else(|error| panic!("{path}: {error}"));
    Engine::new(AnalysisOptions::default())
        .analyze(&source)
        .unwrap_or_else(|error| panic!("{path}: {error}"))
}

fn manifest() -> CorpusManifest {
    serde_json::from_str(
        &fs::read_to_string(repository_root().join("fixtures/corpus.json"))
            .expect("corpus manifest"),
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
    for case in manifest().normative {
        let analysis = analyze(&case.path);
        let ignored: BTreeSet<_> = analysis
            .diagnostics()
            .iter()
            .filter(|diagnostic| case.ignored_codes.contains(diagnostic.code.as_str()))
            .map(|diagnostic| diagnostic.code.as_str().to_owned())
            .collect();
        assert_eq!(
            ignored, case.ignored_codes,
            "{}: stale diagnostic allowlist",
            case.path
        );
        let diagnostics: Vec<_> = analysis
            .diagnostics()
            .iter()
            .filter(|diagnostic| !case.ignored_codes.contains(diagnostic.code.as_str()))
            .collect();
        assert!(diagnostics.is_empty(), "{}: {diagnostics:?}", case.path);
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
fn every_lint_rule_constant_is_reexported() {
    // A rule reaches consumers through two hand-written places: the catalog
    // macro that defines the constant, and the re-export list in lib.rs. A rule
    // added to only the first still produces diagnostics, so nothing fails
    // until someone writes `use ...::THE_RULE` and cannot compile. A released
    // rule has already reached consumers that way, with no constant to name.
    let source = fs::read_to_string(repository_root().join("crates/adocweave/src/lib.rs"))
        .expect("crate root");
    let start = source
        .find("pub use crate::lint::{")
        .expect("lint re-export list");
    let end = source[start..].find("};").expect("end of re-export list") + start;
    let exported = &source[start..end];

    let missing = adocweave::output::diagnostics::LINT_RULES
        .iter()
        .map(|rule| rule.id.as_str().replace('-', "_").to_uppercase())
        .filter(|constant| {
            !exported
                .match_indices(constant.as_str())
                .any(|(offset, _)| {
                    let after = exported[offset + constant.len()..].chars().next();
                    // `INVALID_ATTRIBUTE` must not be satisfied by a longer name.
                    after.is_none_or(|character| !(character.is_alphanumeric() || character == '_'))
                })
        })
        .collect::<Vec<_>>();

    assert!(
        missing.is_empty(),
        "lint rule constants missing from the lib.rs re-export: {missing:?}"
    );
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
