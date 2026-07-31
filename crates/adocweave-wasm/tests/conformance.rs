use std::fs;
use std::path::{Path, PathBuf};

use adocweave::NeverCancel;
use adocweave_wasm::{WasmRequest, process_request};
use serde::Deserialize;
use serde_json::{Value, json};

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ConformanceConsumers {
    manifest: PathBuf,
    fixture_root: PathBuf,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ReleaseManifest {
    schema_version: u16,
    package_version: String,
    rust_version: String,
}

#[test]
fn native_adapter_accepts_every_shared_conformance_case() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let consumers: ConformanceConsumers = serde_json::from_str(
        &fs::read_to_string(root.join("fixtures/conformance/consumers.json"))
            .expect("conformance consumers"),
    )
    .expect("valid conformance consumers");
    let fixtures = root.join(consumers.fixture_root);
    let manifest_path = root.join(consumers.manifest);
    let manifest: Value =
        serde_json::from_str(&fs::read_to_string(manifest_path).expect("conformance manifest"))
            .expect("valid conformance manifest");
    assert_eq!(manifest["packageVersion"], adocweave::VERSION);

    for entry in manifest["cases"].as_array().expect("cases") {
        let name = entry["name"].as_str().expect("case name");
        assert!(entry["compatibility"].is_string(), "{name}: compatibility");
        assert!(entry["rationale"].is_string(), "{name}: rationale");
        assert!(
            entry["contractImpact"].is_string(),
            "{name}: contract impact"
        );
        let request = request_for(entry, &fixtures);
        let result = process_request(request, &NeverCancel);

        if let Some(code) = entry["expectedErrorCode"].as_str() {
            assert_eq!(result.expect_err(name).code, code, "{name}");
            continue;
        }
        let response = result.expect(name);
        assert_eq!(response.package_version, adocweave::VERSION, "{name}");
        assert!(!response.syntax.is_empty(), "{name}: syntax tree");
        assert!(!response.ast.is_empty(), "{name}: AST");
        if name == "position-dependent-attribute-queries-with-include-origin" {
            let included_bindings = response
                .attribute_queries
                .bindings
                .iter()
                .filter(|binding| binding.occurrence.name == "name")
                .collect::<Vec<_>>();
            assert_eq!(included_bindings.len(), 3, "{name}: included bindings");
            assert!(
                included_bindings
                    .iter()
                    .all(|binding| binding.source_id.as_deref() == Some("included:part.adoc")),
                "{name}: binding provenance"
            );
            let included_references = response
                .attribute_queries
                .references
                .iter()
                .filter(|reference| reference.name == "name")
                .collect::<Vec<_>>();
            assert_eq!(included_references.len(), 3, "{name}: included references");
            assert!(
                included_references
                    .iter()
                    .all(|reference| reference.source_id.as_deref() == Some("included:part.adoc")),
                "{name}: reference provenance"
            );
            let forward = response
                .attribute_queries
                .references
                .iter()
                .find(|reference| reference.name == "later")
                .expect("forward reference");
            assert_eq!(forward.binding_id, None, "{name}: forward binding");
            let header = response
                .attribute_queries
                .references
                .iter()
                .find(|reference| reference.name == "header-only")
                .expect("header reference");
            assert_eq!(
                header.effective_value.as_deref(),
                Some("root"),
                "{name}: header value"
            );
            let locked = response
                .attribute_queries
                .references
                .iter()
                .find(|reference| reference.name == "locked")
                .expect("locked reference");
            assert_eq!(locked.binding_id, None, "{name}: external binding");
            assert_eq!(
                locked.effective_value.as_deref(),
                Some("host"),
                "{name}: external value"
            );
            assert!(
                response
                    .attribute_queries
                    .bindings
                    .iter()
                    .all(|binding| binding.occurrence.name != "locked"
                        && binding.occurrence.name != "absent"),
                "{name}: rejected authored bindings"
            );
            let multiline = response
                .attribute_queries
                .bindings
                .iter()
                .find(|binding| binding.occurrence.name == "multi")
                .expect("multiline binding");
            let included_source = entry["preprocess"]["resources"]["part.adoc"]["source"]
                .as_str()
                .expect("included source");
            let second_line = &multiline.occurrence.value.lines[1];
            assert_eq!(
                &included_source[second_line.content_range.start as usize
                    ..second_line.content_range.end as usize],
                "second",
                "{name}: multiline line projection"
            );
        }
        if let Some(file) = entry["expectedHtmlFile"].as_str() {
            assert_eq!(
                response.html,
                fs::read_to_string(resolve(&fixtures, file)).expect("expected HTML"),
                "{name}"
            );
        }
        if let Some(file) = entry["expectedAstFile"].as_str() {
            assert_eq!(
                response.ast,
                fs::read_to_string(resolve(&fixtures, file))
                    .expect("expected AST")
                    .trim_end(),
                "{name}: AST golden"
            );
        }
        for (field, actual) in [
            (
                "expectedDiagnosticsFile",
                serde_json::to_value(&response.diagnostics).expect("diagnostics JSON"),
            ),
            (
                "expectedRenderDiagnosticsFile",
                serde_json::to_value(&response.render_diagnostics)
                    .expect("render diagnostics JSON"),
            ),
            (
                "expectedProjectionFile",
                serde_json::to_value(&response.projection).expect("projection JSON"),
            ),
            (
                "expectedSymbolsFile",
                serde_json::to_value(&response.symbols).expect("symbols JSON"),
            ),
        ] {
            if let Some(file) = entry[field].as_str() {
                let expected: Value = serde_json::from_str(
                    &fs::read_to_string(resolve(&fixtures, file)).expect("expected JSON product"),
                )
                .expect("valid expected JSON product");
                assert_eq!(actual, expected, "{name}: {field}");
            }
        }
    }
}

#[test]
fn release_package_version_is_explicit() {
    let manifest: ReleaseManifest =
        serde_json::from_str(include_str!("../../../release-manifest.json"))
            .expect("valid release manifest");
    assert_eq!(manifest.schema_version, 3);
    assert_eq!(manifest.package_version, env!("CARGO_PKG_VERSION"));
    assert_eq!(manifest.rust_version, env!("CARGO_PKG_RUST_VERSION"));
}

/// Builds the expected file contents of one case from the current implementation.
///
/// Regeneration and the cleanliness check share this function so a fixture can
/// never disagree with the command that writes it. Cases that expect an error,
/// and products a case does not name, contribute nothing.
///
/// Some shared cases reuse a file from the public conformance set. Those files
/// belong to `public_conformance_fixture`, which writes them in the byte form
/// the core crate emits, so they are left out here: one file cannot answer to
/// two regeneration commands.
fn expected_products(entry: &Value, fixtures: &Path) -> Vec<(PathBuf, String)> {
    if entry["expectedErrorCode"].is_string() {
        return Vec::new();
    }
    let response = process_request(request_for(entry, fixtures), &NeverCancel)
        .unwrap_or_else(|error| panic!("{}: {}", entry["name"], error.code));
    let mut products = Vec::new();
    let mut text = |field: &str, content: String| {
        if let Some(file) = entry[field].as_str()
            && !file.starts_with("../")
        {
            products.push((resolve(fixtures, file), content));
        }
    };
    // The HTML fixture holds the rendered document exactly; every other product
    // is JSON, written sorted and indented so a review reads the difference
    // rather than one long line.
    text("expectedHtmlFile", response.html);
    text("expectedAstFile", format!("{}\n", response.ast));
    for (field, value) in [
        (
            "expectedDiagnosticsFile",
            serde_json::to_value(&response.diagnostics),
        ),
        (
            "expectedRenderDiagnosticsFile",
            serde_json::to_value(&response.render_diagnostics),
        ),
        (
            "expectedProjectionFile",
            serde_json::to_value(&response.projection),
        ),
        (
            "expectedSymbolsFile",
            serde_json::to_value(&response.symbols),
        ),
    ] {
        let value = value.expect("product serializes as JSON");
        text(field, format!("{}\n", canonical_json(&value)));
    }
    products
}

/// Renders one product as the bytes a fixture stores.
fn canonical_json(value: &Value) -> String {
    serde_json::to_string_pretty(value).expect("JSON product formats")
}

fn shared_cases() -> (Vec<Value>, PathBuf) {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let consumers: ConformanceConsumers = serde_json::from_str(
        &fs::read_to_string(root.join("fixtures/conformance/consumers.json"))
            .expect("conformance consumers"),
    )
    .expect("valid conformance consumers");
    let manifest: Value = serde_json::from_str(
        &fs::read_to_string(root.join(consumers.manifest)).expect("conformance manifest"),
    )
    .expect("valid conformance manifest");
    let cases = manifest["cases"].as_array().expect("cases").clone();
    (cases, root.join(consumers.fixture_root))
}

#[test]
fn shared_fixture_regeneration_is_clean() {
    let (cases, fixtures) = shared_cases();
    for entry in &cases {
        for (path, expected) in expected_products(entry, &fixtures) {
            assert_eq!(
                fs::read_to_string(&path).unwrap_or_else(|error| panic!("{path:?}: {error}")),
                expected,
                "{}: {} differs from regeneration",
                entry["name"],
                path.display()
            );
        }
    }
}

/// Rewrites every shared fixture from the current implementation.
///
/// Run this after a deliberate contract change, then read each difference. The
/// command is not part of any gate: a golden fixture exists to show what a
/// change did to the published contract, and overwriting it unread discards
/// exactly that.
#[test]
#[ignore = "fixture maintainer command"]
fn regenerate_shared_fixture_products() {
    let (cases, fixtures) = shared_cases();
    for entry in &cases {
        for (path, content) in expected_products(entry, &fixtures) {
            fs::write(&path, content).unwrap_or_else(|error| panic!("{path:?}: {error}"));
        }
    }
}

fn request_for(entry: &Value, fixtures: &Path) -> WasmRequest {
    let source = entry["sourceFile"].as_str().map_or_else(
        || entry["source"].as_str().expect("inline source").to_owned(),
        |file| fs::read_to_string(resolve(fixtures, file)).expect("fixture source"),
    );
    let analysis_options = entry
        .get("analysisOptions")
        .cloned()
        .unwrap_or_else(|| json!({}));
    let render_policy = entry
        .get("renderPolicy")
        .cloned()
        .unwrap_or_else(|| json!({}));
    let output_limits = entry
        .get("outputLimits")
        .cloned()
        .unwrap_or_else(|| json!({}));
    let render_inputs = entry
        .get("renderInputs")
        .cloned()
        .unwrap_or_else(|| json!({}));
    let preprocess = entry.get("preprocess").cloned().unwrap_or(Value::Null);
    serde_json::from_value(json!({
        "packageVersion": adocweave::VERSION,
        "sourceId": entry["sourceId"].as_str().map_or_else(
            || format!("conformance:{}", entry["name"].as_str().expect("name")),
            str::to_owned,
        ),
        "version": 1,
        "generation": 1,
        "source": source,
        "preprocess": preprocess,
        "products": {
            "syntax": true,
            "canonicalAst": true,
            "html": true,
            "attributeOccurrences": true,
            "attributeQueries": true,
            "resourceQueries": true,
            "diagnostics": true,
            "symbols": true,
            "projection": true,
        },
        "renderInputs": render_inputs,
        "analysisOptions": analysis_options,
        "renderPolicy": render_policy,
        "outputLimits": output_limits,
    }))
    .expect("manifest produces a valid WASM request")
}

fn resolve(base: &Path, path: &str) -> PathBuf {
    base.join(path)
}
