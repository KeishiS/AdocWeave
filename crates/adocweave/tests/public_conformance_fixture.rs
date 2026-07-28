use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use adocweave::output::conformance::{ProductSet, products};
use adocweave::output::html::RenderPolicy;
use adocweave::resolution::RenderInputs;
use adocweave::{AnalysisOptions, Engine, SourceId};
use serde::Deserialize;
use serde_json::Value;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Manifest {
    schema_version: u16,
    output_contract_version: u16,
    package_version: String,
    license: String,
    cases: Vec<Case>,
    global_implementation_details: Vec<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Case {
    name: String,
    features: Vec<String>,
    profile: Profile,
    source_id: String,
    files: Files,
    stable_contract: StableContract,
    implementation_details: Vec<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Profile {
    analysis: String,
    render: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Files {
    source: String,
    projection: String,
    html: String,
    diagnostics: String,
    render_diagnostics: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct StableContract {
    projection_assertions: Vec<JsonAssertion>,
    html_contains: Vec<String>,
    diagnostic_codes: Vec<String>,
    render_diagnostic_codes: Vec<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct JsonAssertion {
    pointer: String,
    value: Value,
}

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn read(path: &str) -> String {
    fs::read_to_string(root().join(path)).unwrap_or_else(|error| panic!("{path}: {error}"))
}

fn manifest() -> Manifest {
    serde_json::from_str(&read("fixtures/public-conformance.json")).expect("valid manifest")
}

fn generated(case: &Case) -> adocweave::output::conformance::DocumentProducts {
    let analysis = Engine::new(AnalysisOptions::default())
        .analyze_with_source_id(
            Some(SourceId::new(&case.source_id)),
            &read(&case.files.source),
        )
        .expect("public fixture analyzes");
    products(
        &analysis,
        &RenderPolicy::default(),
        &RenderInputs::default(),
        ProductSet::all(),
    )
}

fn diagnostic_codes(value: &Value) -> Vec<&str> {
    value
        .as_array()
        .expect("diagnostics are an array")
        .iter()
        .filter_map(|diagnostic| diagnostic["code"].as_str())
        .collect()
}

#[test]
fn public_fixtures_match_declared_products_and_stable_contracts() {
    let manifest = manifest();
    assert_eq!(manifest.schema_version, 1);
    assert_eq!(manifest.output_contract_version, 1);
    assert_eq!(manifest.package_version, adocweave::VERSION);
    assert_eq!(manifest.license, "MIT OR Apache-2.0");
    assert_eq!(manifest.cases.len(), 5);
    assert!(!manifest.global_implementation_details.is_empty());
    let features: BTreeSet<&str> = manifest
        .cases
        .iter()
        .flat_map(|case| case.features.iter().map(String::as_str))
        .collect();
    for required in [
        "document-title",
        "toc",
        "section-numbers",
        "source-block",
        "block-title",
        "source-language-option",
        "inline-formula",
        "block-formula",
        "table",
        "quote",
        "unordered-list",
        "unsafe-url",
        "diagnostic",
    ] {
        assert!(features.contains(required), "missing feature: {required}");
    }
    assert!(
        manifest
            .global_implementation_details
            .iter()
            .any(|detail| detail.contains("Issue #108"))
    );
    assert!(
        manifest
            .global_implementation_details
            .iter()
            .any(|detail| detail.contains("Issue #109"))
    );

    for case in &manifest.cases {
        assert!(!case.name.is_empty());
        assert!(!case.features.is_empty(), "{}: features", case.name);
        assert_eq!(case.profile.analysis, "default", "{}", case.name);
        assert_eq!(case.profile.render, "default-fragment", "{}", case.name);
        assert!(
            !case.implementation_details.is_empty(),
            "{}: implementation details",
            case.name
        );
        assert!(
            !case.stable_contract.projection_assertions.is_empty(),
            "{}: projection contract",
            case.name
        );
        assert!(
            !case.stable_contract.html_contains.is_empty(),
            "{}: HTML contract",
            case.name
        );

        let actual = generated(case);
        let html = actual.html.as_deref().expect("HTML was requested");
        assert_eq!(html, read(&case.files.html), "{}: HTML", case.name);

        let expected_projection: Value =
            serde_json::from_str(&read(&case.files.projection)).expect("projection JSON");
        let actual_projection: Value = serde_json::from_str(
            actual
                .projection_json
                .as_deref()
                .expect("projection was requested"),
        )
        .expect("generated projection JSON");
        assert_eq!(
            actual_projection, expected_projection,
            "{}: projection",
            case.name
        );

        let expected_diagnostics: Value =
            serde_json::from_str(&read(&case.files.diagnostics)).expect("diagnostics JSON");
        let actual_diagnostics: Value = serde_json::from_str(
            actual
                .diagnostics_json
                .as_deref()
                .expect("diagnostics were requested"),
        )
        .expect("generated diagnostics JSON");
        assert_eq!(
            actual_diagnostics, expected_diagnostics,
            "{}: diagnostics",
            case.name
        );

        let expected_render_diagnostics: Value =
            serde_json::from_str(&read(&case.files.render_diagnostics))
                .expect("render diagnostics JSON");
        let actual_render_diagnostics: Value = serde_json::from_str(
            actual
                .render_diagnostics_json
                .as_deref()
                .expect("render diagnostics were requested"),
        )
        .expect("generated render diagnostics JSON");
        assert_eq!(
            actual_render_diagnostics, expected_render_diagnostics,
            "{}: render diagnostics",
            case.name
        );

        for assertion in &case.stable_contract.projection_assertions {
            assert_eq!(
                actual_projection.pointer(&assertion.pointer),
                Some(&assertion.value),
                "{}: projection pointer {}",
                case.name,
                assertion.pointer
            );
        }
        for fragment in &case.stable_contract.html_contains {
            assert!(
                html.contains(fragment),
                "{}: missing stable HTML fragment: {fragment}",
                case.name
            );
        }
        assert_eq!(
            diagnostic_codes(&actual_diagnostics),
            case.stable_contract.diagnostic_codes,
            "{}: diagnostic codes",
            case.name
        );
        assert_eq!(
            diagnostic_codes(&actual_render_diagnostics),
            case.stable_contract.render_diagnostic_codes,
            "{}: render diagnostic codes",
            case.name
        );
    }
}

#[test]
#[ignore = "fixture maintainer command"]
fn regenerate_public_fixture_products() {
    for case in &manifest().cases {
        let actual = generated(case);
        fs::write(
            root().join(&case.files.html),
            actual.html.expect("HTML was requested"),
        )
        .expect("write HTML fixture");
        fs::write(
            root().join(&case.files.projection),
            format!(
                "{}\n",
                actual.projection_json.expect("projection was requested")
            ),
        )
        .expect("write projection fixture");
        fs::write(
            root().join(&case.files.diagnostics),
            format!(
                "{}\n",
                actual.diagnostics_json.expect("diagnostics were requested")
            ),
        )
        .expect("write diagnostics fixture");
        fs::write(
            root().join(&case.files.render_diagnostics),
            format!(
                "{}\n",
                actual
                    .render_diagnostics_json
                    .expect("render diagnostics were requested")
            ),
        )
        .expect("write render diagnostics fixture");
    }
}
