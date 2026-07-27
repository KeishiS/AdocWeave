use adocweave::NeverCancel;
use adocweave_wasm::{WasmRequest, process_request};
use serde_json::{Value, json};
use std::collections::BTreeSet;

const SCHEMA: &str = include_str!("../../../protocol/public-api.json");
const CORPUS: &str = include_str!("../../../fixtures/protocol/request-corpus.json");

fn documents() -> (Value, Value) {
    (
        serde_json::from_str(SCHEMA).expect("valid protocol schema"),
        serde_json::from_str(CORPUS).expect("valid protocol corpus"),
    )
}

fn base_request(corpus: &Value) -> Value {
    let mut request = corpus["defaultRequest"].clone();
    request["packageVersion"] = Value::String(adocweave::VERSION.to_owned());
    request
}

fn expanded_request(corpus: &Value) -> Value {
    let mut request = base_request(corpus);
    request["analysisOptions"] = json!({
        "syntax": {},
        "diagnostics": { "rules": { "example": {} } }
    });
    request["renderPolicy"] = json!({
        "sourceLanguages": {},
        "mathLanguages": ["latex"],
        "stylesheets": [{ "kind": "inline", "css": "p {}" }]
    });
    request["outputLimits"] = json!({});
    request
}

fn set_pointer(document: &mut Value, pointer: &str, value: Value) {
    let (parent, field) = pointer.rsplit_once('/').expect("non-root JSON pointer");
    let parent = document
        .pointer_mut(parent)
        .unwrap_or_else(|| panic!("corpus path has no parent: {pointer}"));
    match parent {
        Value::Object(object) => {
            object.insert(field.to_owned(), value);
        }
        Value::Array(array) => {
            array[field.parse::<usize>().expect("array index")] = value;
        }
        _ => panic!("corpus path parent is not a container: {pointer}"),
    }
}

#[test]
fn default_request_uses_every_schema_default() {
    let (schema, corpus) = documents();
    let request: WasmRequest =
        serde_json::from_value(base_request(&corpus)).expect("default request is accepted");

    assert_eq!(request.products, Default::default());
    assert_eq!(request.analysis_options, Default::default());
    assert_eq!(request.render_policy, Default::default());
    assert_eq!(request.output_limits, Default::default());

    for field in schema["request"]["fields"]
        .as_array()
        .expect("request fields")
    {
        let name = field["json"].as_str().expect("field name");
        assert!(
            field["required"].as_bool() == Some(true) || field.get("default").is_some(),
            "{name} must be required or have an explicit default"
        );
    }
}

#[test]
fn request_accepts_every_input_enum_value_and_rejects_unknown_values() {
    let (schema, corpus) = documents();
    for case in corpus["enumCases"].as_array().expect("enum cases") {
        let name = case["enum"].as_str().expect("enum name");
        let path = case["path"].as_str().expect("enum path");
        let values = schema["enums"][name].as_array().expect("schema enum");
        assert!(!values.is_empty(), "{name}");

        for value in values {
            let mut request = expanded_request(&corpus);
            if let Some(template) = case
                .get("templates")
                .and_then(|templates| templates.get(value.as_str().expect("enum string")))
            {
                let parent = path.rsplit_once('/').expect("template path").0;
                set_pointer(&mut request, parent, template.clone());
            } else {
                set_pointer(&mut request, path, value.clone());
            }
            serde_json::from_value::<WasmRequest>(request)
                .unwrap_or_else(|error| panic!("{name} value {value} was rejected: {error}"));
        }

        let mut request = expanded_request(&corpus);
        set_pointer(
            &mut request,
            path,
            Value::String("not-a-protocol-value".to_owned()),
        );
        assert!(
            serde_json::from_value::<WasmRequest>(request).is_err(),
            "{name} accepted an unknown value"
        );
    }
}

#[test]
fn request_rejects_unknown_and_missing_fields_and_old_versions() {
    let (schema, corpus) = documents();
    for case in corpus["unknownFieldCases"]
        .as_array()
        .expect("unknown field cases")
    {
        let path = case["path"].as_str().expect("unknown field path");
        let mut request = expanded_request(&corpus);
        set_pointer(&mut request, path, case["value"].clone());
        assert!(
            serde_json::from_value::<WasmRequest>(request).is_err(),
            "unknown field was accepted at {path}"
        );
    }

    for field in schema["request"]["fields"]
        .as_array()
        .expect("request fields")
    {
        if field["required"].as_bool() != Some(true) {
            continue;
        }
        let name = field["json"].as_str().expect("field name");
        let mut request = base_request(&corpus);
        request
            .as_object_mut()
            .unwrap_or_else(|| panic!("request must be an object"))
            .remove(name);
        assert!(
            serde_json::from_value::<WasmRequest>(request).is_err(),
            "missing required field was accepted: {name}"
        );
    }

    let mut request: WasmRequest =
        serde_json::from_value(base_request(&corpus)).expect("valid request");
    request.package_version = corpus["oldVersion"]
        .as_str()
        .expect("old package version")
        .to_owned();
    let error = process_request(request, &NeverCancel).expect_err("old version is rejected");
    assert_eq!(error.code, "unsupported-api-version");
}

#[test]
fn schema_names_match_the_serde_and_typescript_contracts() {
    let (schema, _) = documents();
    let typescript = include_str!("../../../web-worker/index.d.mts");
    for field in schema["request"]["fields"]
        .as_array()
        .expect("request fields")
    {
        let name = field["json"].as_str().expect("request field");
        if name == "packageVersion" || name == "generation" {
            continue;
        }
        assert!(typescript.contains(name), "TypeScript request field {name}");
    }
    for values in schema["enums"].as_object().expect("enums").values() {
        for value in values.as_array().expect("enum values") {
            let value = value.as_str().expect("enum string");
            assert!(
                typescript.contains(&format!("\"{value}\"")),
                "TypeScript enum value {value}"
            );
        }
    }
}

#[test]
fn response_and_projection_fields_match_the_schema() {
    let (schema, corpus) = documents();
    let mut value = base_request(&corpus);
    value["source"] = corpus["responseProbe"]["source"].clone();
    value["products"] = json!({
        "syntax": true,
        "canonicalAst": true,
        "html": true,
        "attributeOccurrences": true,
        "resourceQueries": true,
        "diagnostics": true,
        "symbols": true,
        "projection": true
    });
    let request: WasmRequest = serde_json::from_value(value).expect("response probe request");
    let response =
        serde_json::to_value(process_request(request, &NeverCancel).expect("response probe"))
            .expect("serializable response");

    assert_fields(&response, &schema["response"]["fields"], "response");
    assert_fields(
        &response["projection"],
        &schema["dtos"]["DocumentProjection"]["fields"],
        "projection",
    );
    assert_fields(
        &response["attributeOccurrences"][0],
        &schema["dtos"]["DocumentAttributeOccurrence"]["fields"],
        "attribute occurrence",
    );
    assert_fields(
        &response["resourceQueries"][0],
        &schema["dtos"]["ResourceQuery"]["fields"],
        "resource query",
    );
    assert_fields(
        &response["diagnostics"][0],
        &schema["dtos"]["Diagnostic"]["fields"],
        "diagnostic",
    );
}

fn assert_fields(value: &Value, schema_fields: &Value, name: &str) {
    let actual = value
        .as_object()
        .unwrap_or_else(|| panic!("{name} must be an object"))
        .keys()
        .map(String::as_str)
        .collect::<BTreeSet<_>>();
    let expected = schema_fields
        .as_array()
        .unwrap_or_else(|| panic!("{name} schema fields"))
        .iter()
        .map(|field| field["json"].as_str().expect("schema field name"))
        .collect::<BTreeSet<_>>();
    assert_eq!(actual, expected, "{name} fields");
}
