use std::fs;
use std::path::{Path, PathBuf};

use adocweave::NeverCancel;
use adocweave_wasm::{WasmRequest, process_request};
use serde::Deserialize;
use serde_json::{Value, json};

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ReleaseManifest {
    schema_version: u16,
    package_version: String,
    rust_version: String,
}

#[test]
fn native_adapter_accepts_every_shared_conformance_case() {
    let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/conformance");
    let manifest: Value = serde_json::from_str(
        &fs::read_to_string(fixtures.join("cases.json")).expect("conformance manifest"),
    )
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
            ("expectedDiagnosticsFile", &response.diagnostics),
            (
                "expectedRenderDiagnosticsFile",
                &response.render_diagnostics,
            ),
            ("expectedProjectionFile", &response.projection),
            ("expectedSymbolsFile", &response.symbols),
        ] {
            if let Some(file) = entry[field].as_str() {
                let expected: Value = serde_json::from_str(
                    &fs::read_to_string(resolve(&fixtures, file)).expect("expected JSON product"),
                )
                .expect("valid expected JSON product");
                assert_eq!(*actual, expected, "{name}: {field}");
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
        "sourceId": format!("conformance:{}", entry["name"].as_str().expect("name")),
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
