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
    contract_version: u16,
}

#[test]
fn native_adapter_accepts_every_shared_conformance_case() {
    let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/conformance");
    let manifest: Value = serde_json::from_str(
        &fs::read_to_string(fixtures.join("cases.json")).expect("conformance manifest"),
    )
    .expect("valid conformance manifest");
    assert_eq!(manifest["contractVersion"], 2);

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
        assert_eq!(response.api_version, adocweave::CONTRACT_VERSION, "{name}");
        assert!(!response.syntax.is_empty(), "{name}: syntax tree");
        assert!(!response.ast.is_empty(), "{name}: AST");
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
fn release_contract_version_is_explicit() {
    let manifest: ReleaseManifest =
        serde_json::from_str(include_str!("../../../release-manifest.json"))
            .expect("valid release manifest");
    assert_eq!(manifest.schema_version, 2);
    assert_eq!(manifest.package_version, env!("CARGO_PKG_VERSION"));
    assert_eq!(manifest.contract_version, adocweave::CONTRACT_VERSION);
}

fn request_for(entry: &Value, fixtures: &Path) -> WasmRequest {
    let source = entry["sourceFile"].as_str().map_or_else(
        || entry["source"].as_str().expect("inline source").to_owned(),
        |file| fs::read_to_string(resolve(fixtures, file)).expect("fixture source"),
    );
    let options = entry.get("options").cloned().unwrap_or_else(|| json!({}));
    let render_inputs = entry
        .get("renderInputs")
        .cloned()
        .unwrap_or_else(|| json!({}));
    serde_json::from_value(json!({
        "apiVersion": adocweave::CONTRACT_VERSION,
        "sourceId": format!("conformance:{}", entry["name"].as_str().expect("name")),
        "version": 1,
        "generation": 1,
        "source": source,
        "renderInputs": render_inputs,
        "options": options,
    }))
    .expect("manifest produces a valid WASM request")
}

fn resolve(base: &Path, path: &str) -> PathBuf {
    base.join(path)
}
