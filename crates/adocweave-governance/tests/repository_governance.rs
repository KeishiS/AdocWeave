use std::collections::{BTreeMap, BTreeSet};
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

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ReleaseManifest {
    package_version: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct JapaneseTerminology {
    schema_version: u8,
    forbidden_terms: Vec<ForbiddenJapaneseTerm>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ForbiddenJapaneseTerm {
    id: String,
    term: String,
    r#match: ForbiddenTermMatch,
    message: String,
    documentation: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "kebab-case")]
enum ForbiddenTermMatch {
    Substring,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct SyntaxSupportManifest {
    schema_version: u8,
    features: Vec<SyntaxSupportFeature>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SyntaxSupportFeature {
    name: String,
    status: String,
    syntax_needle: String,
    compatibility_needle: String,
    grammar_needle: String,
    fixture: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct AttributeContractManifest {
    schema_version: u8,
    contracts: Vec<AttributeContract>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct AttributeContract {
    name: String,
    documentation: String,
    documentation_needle: String,
    fixtures: Vec<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ConformanceConsumers {
    schema_version: u8,
    manifest: String,
    fixture_root: String,
    consumers: Vec<ConformanceConsumer>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ConformanceConsumer {
    name: String,
    path: String,
    capabilities: BTreeSet<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Html5Manifest {
    schema_version: u8,
    validator: Html5Validator,
    template: Html5Template,
    cases: Vec<Html5Case>,
    negative_fixtures: Vec<Html5NegativeFixture>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Html5Validator {
    package: String,
    version: String,
    options: Vec<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Html5Template {
    path: String,
    marker: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Html5Case {
    name: String,
    kind: String,
    source: Option<String>,
    case: Option<String>,
    args: Option<Vec<String>>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Html5NegativeFixture {
    path: String,
    r#type: String,
    message_pattern: String,
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
fn syntax_support_manifest_keeps_docs_and_fixtures_in_sync() {
    let root = repository_root();
    let source = fs::read_to_string(root.join("fixtures/syntax-support.json"))
        .expect("syntax support manifest");
    let manifest: SyntaxSupportManifest =
        serde_json::from_str(&source).expect("valid syntax support manifest");
    assert_eq!(manifest.schema_version, 2);
    let legacy = source.replacen(
        "\"features\"",
        "\"issue\": \"136\", \"issueStatus\": \"completed\", \"features\"",
        1,
    );
    assert!(
        serde_json::from_str::<SyntaxSupportManifest>(&legacy).is_err(),
        "schema v2 must reject retired local Issue fields"
    );
    let syntax = fs::read_to_string(root.join("docs/user-guide/syntax-support.adoc"))
        .expect("syntax support");
    let compatibility =
        fs::read_to_string(root.join("docs/user-guide/compatibility.adoc")).expect("compatibility");
    let grammar =
        fs::read_to_string(root.join("docs/developer-guide/grammar.adoc")).expect("grammar");
    for feature in manifest.features {
        assert_eq!(feature.status, "supported", "{}", feature.name);
        assert!(
            syntax.contains(&feature.syntax_needle),
            "syntax: {}",
            feature.name
        );
        assert!(
            compatibility.contains(&feature.compatibility_needle),
            "compatibility: {}",
            feature.name
        );
        assert!(
            grammar.contains(&feature.grammar_needle),
            "grammar: {}",
            feature.name
        );
        assert!(
            root.join(&feature.fixture).is_file(),
            "fixture: {}",
            feature.name
        );
    }
}

#[test]
fn attribute_contract_manifest_keeps_docs_and_fixtures_in_sync() {
    let root = repository_root();
    let source = fs::read_to_string(root.join("fixtures/attributes/manifest.json"))
        .expect("attribute contract manifest");
    let manifest: AttributeContractManifest =
        serde_json::from_str(&source).expect("valid attribute contract manifest");
    assert_eq!(manifest.schema_version, 1);

    let mut listed = BTreeSet::new();
    let mut contract_names = BTreeSet::new();
    for contract in manifest.contracts {
        assert!(
            contract_names.insert(contract.name.clone()),
            "duplicate attribute contract: {}",
            contract.name
        );
        let documentation = fs::read_to_string(root.join(&contract.documentation))
            .unwrap_or_else(|error| panic!("{}: {error}", contract.documentation));
        assert!(
            documentation.contains(&contract.documentation_needle),
            "{}: missing documentation needle for {}",
            contract.documentation,
            contract.name
        );
        assert!(
            !contract.fixtures.is_empty(),
            "{}: contract has no fixtures",
            contract.name
        );
        for fixture in contract.fixtures {
            assert!(
                listed.insert(fixture.clone()),
                "attribute fixture listed more than once: {fixture}"
            );
            assert!(
                root.join("fixtures/attributes").join(&fixture).is_file(),
                "missing attribute fixture: {fixture}"
            );
        }
    }

    let actual: BTreeSet<_> = fs::read_dir(root.join("fixtures/attributes"))
        .expect("attribute fixture directory")
        .map(|entry| entry.expect("attribute fixture entry").path())
        .filter(|path| {
            path.extension()
                .is_some_and(|extension| extension == "adoc")
        })
        .map(|path| {
            path.file_name()
                .expect("attribute fixture name")
                .to_string_lossy()
                .into_owned()
        })
        .collect();
    assert_eq!(listed, actual, "attribute fixture manifest is stale");
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

/// The document that defines the rule quotes the words it forbids.
const TERMINOLOGY_DOCUMENT: &str = "docs/developer-guide/terminology.adoc";

fn japanese_terminology() -> JapaneseTerminology {
    serde_json::from_str(
        &fs::read_to_string(repository_root().join("config/japanese-terminology.json"))
            .expect("Japanese terminology catalog"),
    )
    .expect("valid Japanese terminology catalog")
}

#[test]
fn japanese_terminology_catalog_is_valid_and_documented() {
    let catalog = japanese_terminology();
    assert_eq!(catalog.schema_version, 1, "unsupported terminology schema");
    assert!(
        !catalog.forbidden_terms.is_empty(),
        "terminology catalog has no forbidden terms"
    );

    let root = repository_root();
    let mut ids = BTreeSet::new();
    let mut terms = BTreeSet::new();
    for entry in catalog.forbidden_terms {
        assert!(!entry.id.trim().is_empty(), "terminology ID is empty");
        assert!(!entry.term.trim().is_empty(), "terminology term is empty");
        assert!(
            !entry.message.trim().is_empty(),
            "terminology message is empty"
        );
        assert!(
            ids.insert(entry.id.clone()),
            "duplicate terminology ID: {}",
            entry.id
        );
        assert!(
            terms.insert(entry.term.clone()),
            "duplicate forbidden term: {}",
            entry.term
        );
        assert!(
            matches!(entry.r#match, ForbiddenTermMatch::Substring),
            "unsupported terminology match"
        );

        let (path, anchor) = entry
            .documentation
            .split_once('#')
            .expect("terminology documentation must contain an anchor");
        assert!(!path.is_empty(), "terminology documentation path is empty");
        assert!(
            !anchor.is_empty(),
            "terminology documentation anchor is empty"
        );
        let documentation = fs::read_to_string(root.join(path))
            .unwrap_or_else(|error| panic!("cannot read {path}: {error}"));
        assert!(
            documentation.contains(&format!("[#{anchor}]")),
            "{} does not define [#{}]",
            entry.documentation,
            anchor
        );
    }
}

#[test]
fn authored_documents_avoid_the_forbidden_japanese_words() {
    let terminology = japanese_terminology();
    let output = Command::new("git")
        .args(["ls-files", "-z", "*.adoc"])
        .current_dir(repository_root())
        .output()
        .expect("git ls-files");
    assert!(output.status.success());
    let mut found = Vec::new();
    for path in output
        .stdout
        .split(|byte| *byte == 0)
        .filter(|path| !path.is_empty())
    {
        let path = std::str::from_utf8(path).expect("UTF-8 repository path");
        if path == TERMINOLOGY_DOCUMENT {
            continue;
        }
        let source = fs::read_to_string(repository_root().join(path)).expect("authored document");
        for (number, line) in source.lines().enumerate() {
            for entry in &terminology.forbidden_terms {
                if line.contains(&entry.term) {
                    found.push(format!("{path}:{}: {}", number + 1, entry.term));
                }
            }
        }
    }
    assert!(
        found.is_empty(),
        "{TERMINOLOGY_DOCUMENT}が使わないと定めた語があります。\
         何と書くかは、その箇所が何を指しているかによります。\n{}",
        found.join("\n")
    );
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
fn local_issue_documents_are_not_tracked() {
    let output = Command::new("git")
        .args(["ls-files", "issues"])
        .current_dir(repository_root())
        .output()
        .expect("git ls-files issues");
    assert!(output.status.success());
    assert!(
        output.stdout.is_empty(),
        "implementation plans and work records belong in GitHub Issues"
    );
}

#[test]
fn adr_index_lists_every_record_once() {
    let root = repository_root();
    let directory = root.join("docs/developer-guide/adr");
    let index = fs::read_to_string(directory.join("index.adoc")).expect("ADR index");
    let records = fs::read_dir(&directory)
        .expect("ADR directory")
        .map(|entry| entry.expect("ADR entry").path())
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name != "index.adoc" && name.ends_with(".adoc"))
        })
        .map(|path| {
            path.file_name()
                .expect("ADR file name")
                .to_string_lossy()
                .into_owned()
        })
        .collect::<BTreeSet<_>>();
    let listed = index
        .lines()
        .filter_map(|line| line.strip_prefix("* xref:"))
        .map(|reference| {
            reference
                .split_once('[')
                .expect("ADR xref has a label")
                .0
                .to_owned()
        })
        .collect::<Vec<_>>();
    let unique = listed.iter().cloned().collect::<BTreeSet<_>>();

    assert_eq!(listed.len(), unique.len(), "ADR index contains duplicates");
    assert_eq!(unique, records, "ADR index and records differ");
}

#[test]
fn roadmap_uses_unique_github_issue_urls() {
    let source = fs::read_to_string(repository_root().join("docs/developer-guide/roadmap.adoc"))
        .expect("roadmap");
    let prefix = "https://github.com/KeishiS/adocweave/issues/";
    let mut numbers = BTreeSet::new();
    for line in source.lines().filter(|line| line.contains(prefix)) {
        let suffix = line.split_once(prefix).expect("GitHub Issue URL").1;
        let number = suffix.split_once('[').expect("AsciiDoc link label").0;
        assert!(
            !number.is_empty() && number.bytes().all(|byte| byte.is_ascii_digit()),
            "invalid GitHub Issue URL: {line}"
        );
        assert!(
            numbers.insert(number.to_owned()),
            "duplicate GitHub Issue URL: {line}"
        );
    }
    assert!(!numbers.is_empty(), "roadmap has no GitHub Issues");

    // This list and the roadmap are both written by hand, so a closed Issue can
    // sit in the roadmap until someone updates both. The check catches an Issue
    // added to one and not the other; it cannot tell that an Issue is closed,
    // because a test may not reach GitHub. Update this list in the same change
    // that closes an Issue.
    let expected = ["33", "34", "82", "83", "84", "86", "384"]
        .into_iter()
        .map(str::to_owned)
        .collect();
    assert_eq!(
        numbers, expected,
        "roadmap must list the current major open Issues and no closed Issues"
    );
}

#[test]
fn pull_request_template_covers_change_review_and_verification() {
    let source = fs::read_to_string(repository_root().join(".github/pull_request_template.md"))
        .expect("pull request template");
    for heading in [
        "## 目的",
        "## 現在の動作・結果",
        "## 期待する動作・結果",
        "## 変更内容",
        "## 影響範囲",
        "## 互換性と安全性",
        "## 検証",
        "## ドキュメント",
        "## 関連Issue",
    ] {
        assert!(source.contains(heading), "missing PR section: {heading}");
    }
    assert!(
        source.contains("`cargo make verify`"),
        "missing PR guidance: `cargo make verify`"
    );
}

#[test]
fn contract_tables_are_not_nested_or_unclosed() {
    for path in [
        "docs/README.adoc",
        "docs/user-guide/syntax-support.adoc",
        "docs/user-guide/compatibility.adoc",
        "docs/developer-guide/grammar.adoc",
        "docs/developer-guide/html-contract.adoc",
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
fn release_manifest_is_the_single_release_identity_catalog() {
    let root = repository_root();
    let manifest: ReleaseManifest = serde_json::from_str(
        &fs::read_to_string(root.join("release-manifest.json")).expect("release manifest"),
    )
    .expect("valid release manifest");
    assert_eq!(manifest.package_version, adocweave::VERSION);

    for path in ["docs/developer-guide/core-profile.adoc", "docs/README.adoc"] {
        let documentation =
            fs::read_to_string(root.join(path)).expect("release identity documentation");
        assert!(
            documentation.contains("release-manifest.json[release manifest]"),
            "{path} must reference the release manifest"
        );
        assert!(
            !documentation.contains(&manifest.package_version),
            "{path} must not duplicate the package version"
        );
    }
}

#[test]
fn project_config_schema_lists_every_lint_rule() {
    let root = repository_root();
    let schema: serde_json::Value = serde_json::from_str(
        &fs::read_to_string(root.join("config/adocweave.schema.json"))
            .expect("project configuration schema"),
    )
    .expect("valid project configuration schema");
    let schema_rules = schema["properties"]["lint"]["properties"]["rules"]["propertyNames"]["enum"]
        .as_array()
        .expect("lint rule name enum")
        .iter()
        .map(|value| value.as_str().expect("lint rule name"))
        .collect::<Vec<_>>();
    let mut catalog_rules = adocweave::output::diagnostics::LINT_RULES
        .iter()
        .map(|rule| rule.id.as_str())
        .collect::<Vec<_>>();
    catalog_rules.sort_unstable();

    assert!(schema_rules.windows(2).all(|pair| pair[0] < pair[1]));
    assert_eq!(schema_rules, catalog_rules);
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
fn conformance_fixture_has_every_declared_consumer() {
    let root = repository_root();
    let manifest: ConformanceConsumers = serde_json::from_str(
        &fs::read_to_string(root.join("fixtures/conformance/consumers.json"))
            .expect("conformance consumer manifest"),
    )
    .expect("valid conformance consumer manifest");
    assert_eq!(manifest.schema_version, 2);
    assert!(root.join(&manifest.manifest).is_file());
    assert!(root.join(&manifest.fixture_root).is_dir());
    let capabilities = |values: &[&str]| {
        values
            .iter()
            .map(|value| (*value).to_owned())
            .collect::<BTreeSet<_>>()
    };
    let expected = BTreeMap::from([
        (
            "browser",
            (
                "web-worker/cross-runtime.test.mjs",
                capabilities(&["fixture-root", "manifest"]),
            ),
        ),
        (
            "html5",
            (
                "tools/html5-check.mjs",
                capabilities(&["fixture-root", "manifest"]),
            ),
        ),
        (
            "native-core",
            (
                "crates/adocweave/src/conformance.rs",
                capabilities(&["manifest"]),
            ),
        ),
        (
            "public-conformance",
            (
                "crates/adocweave/tests/public_conformance_fixture.rs",
                capabilities(&["fixture-root", "manifest"]),
            ),
        ),
        (
            "repository-governance",
            (
                "crates/adocweave-governance/tests/repository_governance.rs",
                capabilities(&["fixture-root", "manifest"]),
            ),
        ),
        (
            "wasm",
            (
                "crates/adocweave-wasm/tests/conformance.rs",
                capabilities(&["fixture-root", "manifest"]),
            ),
        ),
    ]);
    let mut names = BTreeSet::new();
    for consumer in manifest.consumers {
        assert!(root.join(&consumer.path).is_file(), "{}", consumer.path);
        let (path, capabilities) = expected
            .get(consumer.name.as_str())
            .unwrap_or_else(|| panic!("unexpected conformance consumer: {}", consumer.name));
        assert_eq!(&consumer.path, path, "{}", consumer.name);
        assert_eq!(&consumer.capabilities, capabilities, "{}", consumer.name);
        assert!(
            names.insert(consumer.name.clone()),
            "duplicate conformance consumer: {}",
            consumer.name
        );
    }
    assert_eq!(
        names,
        expected.keys().map(|name| (*name).to_owned()).collect()
    );
}

#[test]
fn core_source_package_contains_conformance_manifest() {
    let root = repository_root();
    let output = Command::new("cargo")
        .args([
            "package",
            "--list",
            "-p",
            "adocweave",
            "--allow-dirty",
            "--offline",
        ])
        .current_dir(&root)
        .output()
        .expect("cargo package --list");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let files = String::from_utf8(output.stdout).expect("UTF-8 package file list");
    assert!(
        files.lines().any(|path| path == "conformance/cases.json"),
        "adocweave source package does not contain conformance/cases.json"
    );
}

#[test]
fn html5_validation_manifest_has_fixed_tools_and_complete_inputs() {
    let root = repository_root();
    let consumers: ConformanceConsumers = serde_json::from_str(
        &fs::read_to_string(root.join("fixtures/conformance/consumers.json"))
            .expect("conformance consumer manifest"),
    )
    .expect("valid conformance consumer manifest");
    let manifest: Html5Manifest = serde_json::from_str(
        &fs::read_to_string(root.join("fixtures/html/validation.json"))
            .expect("HTML5 validation manifest"),
    )
    .expect("valid HTML5 validation manifest");
    assert_eq!(manifest.schema_version, 1);
    assert_eq!(manifest.validator.package, "validator-nu");
    assert!(!manifest.validator.version.is_empty());
    assert_eq!(
        manifest.validator.options,
        ["--format", "json", "--Werror", "--no-langdetect"]
    );

    let template = fs::read_to_string(root.join(&manifest.template.path)).expect("HTML5 template");
    assert_eq!(template.matches(&manifest.template.marker).count(), 1);

    let conformance: serde_json::Value = serde_json::from_str(
        &fs::read_to_string(root.join(consumers.manifest)).expect("conformance manifest"),
    )
    .expect("valid conformance manifest");
    let conformance_modes = conformance["cases"]
        .as_array()
        .expect("conformance cases")
        .iter()
        .map(|case| {
            (
                case["name"]
                    .as_str()
                    .expect("conformance case name")
                    .to_owned(),
                case["renderPolicy"]["documentMode"]
                    .as_str()
                    .unwrap_or("fragment")
                    .to_owned(),
            )
        })
        .collect::<BTreeMap<_, _>>();

    let mut names = BTreeSet::new();
    for case in manifest.cases {
        assert!(
            names.insert(case.name.clone()),
            "duplicate case: {}",
            case.name
        );
        match case.kind.as_str() {
            "cli-fragment" | "cli-complete" => {
                assert!(
                    case.case.is_none(),
                    "CLI case {} has conformance input",
                    case.name
                );
                assert!(
                    root.join(case.source.expect("CLI source")).is_file(),
                    "missing CLI source for {}",
                    case.name
                );
            }
            "conformance-fragment" | "conformance-complete" => {
                assert!(
                    case.source.is_none(),
                    "conformance case {} has CLI source",
                    case.name
                );
                let reference = case.case.as_deref().expect("conformance input");
                let actual_mode = conformance_modes
                    .get(reference)
                    .unwrap_or_else(|| panic!("unknown conformance case: {reference}"));
                let expected_mode = if case.kind == "conformance-complete" {
                    "complete"
                } else {
                    "fragment"
                };
                assert_eq!(
                    actual_mode, expected_mode,
                    "document mode mismatch for {}",
                    case.name
                );
                assert!(
                    case.args.is_none(),
                    "conformance case {} has CLI args",
                    case.name
                );
            }
            other => panic!("unsupported HTML5 case kind: {other}"),
        }
    }

    let mut negative = BTreeSet::new();
    for fixture in manifest.negative_fixtures {
        assert!(
            negative.insert(fixture.path.clone()),
            "duplicate negative fixture: {}",
            fixture.path
        );
        assert!(
            root.join(&fixture.path).is_file(),
            "missing negative fixture"
        );
        assert_eq!(fixture.r#type, "error");
        assert!(!fixture.message_pattern.is_empty());
    }

    let makefile = fs::read_to_string(root.join("Makefile.toml")).expect("Makefile");
    assert!(makefile.contains("[tasks.html5-check]"));
    assert!(makefile.contains("\"html5-check\","));
    let flake = fs::read_to_string(root.join("flake.nix")).expect("flake");
    assert!(flake.contains("ADOCWEAVE_HTML_VALIDATOR"));
    assert!(flake.contains("pkgs.validator-nu"));
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

/// The governance crate holds checks, not implementation.
///
/// These checks read the repository's own file layout. Keeping them out of a
/// library is the point of the crate, so it carries no library at all: there is
/// nothing for another crate to depend on, and nothing for the SemVer gate to
/// compare.
#[test]
fn the_governance_crate_carries_no_implementation() {
    let root = repository_root();
    let package = root.join("crates/adocweave-governance");
    assert!(
        !package.join("src").exists(),
        "the governance crate must carry no library or binary"
    );

    let manifest = fs::read_to_string(package.join("Cargo.toml")).expect("governance manifest");
    assert!(
        !manifest.contains("\n[dependencies]\n"),
        "governance checks belong in dev-dependencies"
    );
    assert!(manifest.contains("\n[dev-dependencies]\n"));

    for crate_name in [
        "adocweave",
        "adocweave-cli",
        "adocweave-config",
        "adocweave-host",
        "adocweave-lsp",
        "adocweave-wasm",
        "adocweave-workspace",
    ] {
        let manifest = fs::read_to_string(root.join("crates").join(crate_name).join("Cargo.toml"))
            .expect("crate manifest");
        assert!(
            !manifest.contains("adocweave-governance"),
            "{crate_name} must not depend on the governance crate"
        );
    }
}

#[test]
fn workspace_state_has_no_filesystem_or_host_dependency() {
    let root = repository_root();
    let manifest = fs::read_to_string(root.join("crates/adocweave-workspace/Cargo.toml"))
        .expect("workspace manifest");
    let source =
        fs::read_to_string(root.join("crates/adocweave-workspace/src/lib.rs")).expect("workspace");
    let host =
        fs::read_to_string(root.join("crates/adocweave-host/src/local_resource.rs")).expect("host");

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

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ConfigCorpus {
    schema_version: u8,
    cases: Vec<ConfigCorpusCase>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ConfigCorpusCase {
    name: String,
    config: serde_json::Value,
    accepted: bool,
    #[serde(default)]
    schema_only_accepts: bool,
    #[serde(default)]
    reason: Option<String>,
}

#[test]
fn the_configuration_corpus_matches_the_implementation() {
    // The JSON Schema and the implementation are two hand-written answers to
    // the same question. `tools/config-schema.test.mjs` asks the schema; this
    // asks the implementation, so a constraint added to one and not the other
    // is reported instead of leaving an editor to accept what the run rejects.
    let root = repository_root();
    let corpus: ConfigCorpus = serde_json::from_str(
        &fs::read_to_string(root.join("fixtures/config/schema-corpus.json"))
            .expect("configuration corpus"),
    )
    .expect("valid configuration corpus");
    assert_eq!(corpus.schema_version, 1);
    assert!(!corpus.cases.is_empty());

    let mut disagreements = Vec::new();
    for case in &corpus.cases {
        let source = toml::to_string(&case.config).unwrap_or_else(|error| {
            panic!("{}: corpusの設定をTOMLへ変換できません: {error}", case.name)
        });
        let accepted = adocweave_config::ResolvedProjectConfig::parse(
            &source,
            std::path::Path::new("/workspace"),
        )
        .is_ok();
        if accepted != case.accepted {
            disagreements.push(format!(
                "{}: 実装={} corpus={}",
                case.name,
                if accepted { "受理" } else { "拒否" },
                if case.accepted { "受理" } else { "拒否" },
            ));
        }
        if case.schema_only_accepts {
            assert!(!case.accepted, "{}", case.name);
            assert!(case.reason.is_some(), "{}: 理由がありません", case.name);
        }
    }
    assert_eq!(disagreements, Vec::<String>::new());
}
