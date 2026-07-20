use std::fs;
use std::path::{Path, PathBuf};

use adocweave::NeverCancel;
use adocweave_wasm::{WASM_API_VERSION, WasmRequest, process_request};
use serde_json::{Value, json};

#[test]
fn native_adapter_accepts_every_shared_conformance_case() {
    let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/conformance");
    let manifest: Value = serde_json::from_str(
        &fs::read_to_string(fixtures.join("cases.json")).expect("conformance manifest"),
    )
    .expect("valid conformance manifest");
    assert_eq!(manifest["contractVersion"], 1);

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
        assert_eq!(response.api_version, WASM_API_VERSION, "{name}");
        assert!(!response.cst.is_empty(), "{name}: CST");
        assert!(!response.ast.is_empty(), "{name}: AST");
        if let Some(file) = entry["expectedHtmlFile"].as_str() {
            assert_eq!(
                response.html,
                fs::read_to_string(resolve(&fixtures, file)).expect("expected HTML"),
                "{name}"
            );
        }
    }
}

#[test]
fn release_contract_versions_are_explicit_and_independent() {
    assert_eq!(adocweave::CORE_PROFILE_VERSION, 1);
    assert_eq!(adocweave::CORE_API_VERSION, 6);
    assert_eq!(adocweave::html::HTML_CONTRACT_VERSION, 2);
    assert_eq!(adocweave::projection::PROJECTION_CONTRACT_VERSION, 1);
    assert_eq!(adocweave::conformance::CONFORMANCE_CONTRACT_VERSION, 2);
    assert_eq!(WASM_API_VERSION, 2);
}

fn request_for(entry: &Value, fixtures: &Path) -> WasmRequest {
    let source = entry["sourceFile"].as_str().map_or_else(
        || entry["source"].as_str().expect("inline source").to_owned(),
        |file| fs::read_to_string(resolve(fixtures, file)).expect("fixture source"),
    );
    let options = entry.get("options").cloned().unwrap_or_else(|| json!({}));
    serde_json::from_value(json!({
        "apiVersion": WASM_API_VERSION,
        "sourceId": format!("conformance:{}", entry["name"].as_str().expect("name")),
        "version": 1,
        "generation": 1,
        "source": source,
        "options": options,
    }))
    .expect("manifest produces a valid WASM request")
}

fn resolve(base: &Path, path: &str) -> PathBuf {
    base.join(path)
}
