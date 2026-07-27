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
    request["products"] = json!({
        "syntax": true,
        "canonicalAst": true,
        "html": true,
        "attributeOccurrences": true,
        "resourceQueries": true,
        "diagnostics": true,
        "symbols": true,
        "projection": true
    });
    request["analysisOptions"] = json!({
        "syntax": { "limits": {} },
        "diagnostics": {
            "rules": { "example": {} },
            "authoredUrls": {}
        }
    });
    request["renderPolicy"] = json!({
        "activeUrls": {},
        "externalLinks": {},
        "sourceLanguages": {},
        "resources": {},
        "mathLanguages": ["latex"],
        "stylesheets": [{ "kind": "inline", "css": "p {}" }]
    });
    request["renderInputs"] = json!({
        "references": [{
            "sourceStart": 0,
            "sourceEnd": 1,
            "outcome": { "status": "failed", "kind": "missing-target" }
        }],
        "resources": [{
            "sourceStart": 0,
            "sourceEnd": 1,
            "outcome": { "status": "failed", "kind": "missing" }
        }]
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
    assert_schema_defaults(
        &serde_json::to_value(&request.analysis_options).expect("analysis defaults"),
        "AnalysisOptions",
        &schema,
    );
    assert_schema_defaults(
        &serde_json::to_value(&request.render_policy).expect("render defaults"),
        "RenderPolicy",
        &schema,
    );
    assert_schema_defaults(
        &serde_json::to_value(request.output_limits).expect("output defaults"),
        "OutputLimits",
        &schema,
    );

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

fn assert_schema_defaults(value: &Value, name: &str, schema: &Value) {
    let contract = schema["settings"]
        .get(name)
        .or_else(|| schema["definitions"].get(name))
        .unwrap_or_else(|| panic!("default contract {name}"));
    for field in contract["fields"].as_array().expect("default fields") {
        let field_name = field["json"].as_str().expect("default field name");
        let default = &field["default"];
        assert!(
            !value[field_name].is_null() || default.is_null(),
            "{name}.{field_name}"
        );
        if default == "core-default" || default == "browser-default" {
            continue;
        }
        let nested_type = field["type"].as_str().expect("nested default type");
        let has_nested_contract = schema["settings"].get(nested_type).is_some()
            || schema["definitions"].get(nested_type).is_some();
        if default.as_object().is_some_and(serde_json::Map::is_empty) && has_nested_contract {
            assert_schema_defaults(&value[field_name], nested_type, schema);
        } else {
            assert_eq!(&value[field_name], default, "{name}.{field_name} default");
        }
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
            apply_setup(&mut request, case);
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
        apply_setup(&mut request, case);
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

fn apply_setup(request: &mut Value, case: &Value) {
    for entry in case
        .get("setup")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        set_pointer(
            request,
            entry["path"].as_str().expect("setup path"),
            entry["value"].clone(),
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
fn every_request_object_enforces_schema_fields_recursively() {
    let (schema, corpus) = documents();
    for case in corpus["objectCases"].as_array().expect("object cases") {
        let name = case["object"].as_str().expect("object name");
        let path = case["path"].as_str().expect("object path");
        let contract = if name == "WasmRequest" {
            &schema["request"]
        } else if name == "ProductSet" {
            &schema["productSet"]
        } else {
            schema["settings"]
                .get(name)
                .or_else(|| schema["definitions"].get(name))
                .unwrap_or_else(|| panic!("unknown request object {name}"))
        };

        let mut unknown = expanded_request(&corpus);
        let object = unknown
            .pointer_mut(path)
            .unwrap_or_else(|| panic!("missing object path {path}"))
            .as_object_mut()
            .unwrap_or_else(|| panic!("{name} must be an object"));
        object.insert("unexpected".to_owned(), Value::Bool(true));
        assert!(
            serde_json::from_value::<WasmRequest>(unknown).is_err(),
            "{name} accepted an unknown field"
        );

        for field in contract["fields"].as_array().expect("object fields") {
            if field["required"].as_bool() != Some(true) {
                continue;
            }
            let field_name = field["json"].as_str().expect("field name");
            let mut missing = expanded_request(&corpus);
            missing
                .pointer_mut(path)
                .expect("object path")
                .as_object_mut()
                .expect("object")
                .remove(field_name);
            assert!(
                serde_json::from_value::<WasmRequest>(missing).is_err(),
                "{name} accepted missing required field {field_name}"
            );
        }
    }
}

#[test]
fn every_request_union_enforces_tags_fields_and_unknown_rejection() {
    let (schema, corpus) = documents();
    for case in corpus["unionCases"].as_array().expect("union cases") {
        let name = case["union"].as_str().expect("union name");
        let path = case["path"].as_str().expect("union path");
        let contract = &schema["taggedUnions"][name];
        let tag = contract["tag"].as_str().expect("union tag");
        for (variant, fields) in contract["variants"].as_object().expect("variants") {
            let template = case["variants"][variant].clone();
            let mut valid = expanded_request(&corpus);
            set_pointer(&mut valid, path, template.clone());
            serde_json::from_value::<WasmRequest>(valid)
                .unwrap_or_else(|error| panic!("{name}.{variant} was rejected: {error}"));

            let mut unknown = expanded_request(&corpus);
            let mut value = template.clone();
            value["unexpected"] = Value::Bool(true);
            set_pointer(&mut unknown, path, value);
            assert!(
                serde_json::from_value::<WasmRequest>(unknown).is_err(),
                "{name}.{variant} accepted an unknown field"
            );

            for field in fields.as_array().expect("variant fields") {
                if field["required"].as_bool() != Some(true) {
                    continue;
                }
                let field_name = field["json"].as_str().expect("variant field");
                let mut missing = expanded_request(&corpus);
                let mut value = template.clone();
                value
                    .as_object_mut()
                    .expect("variant object")
                    .remove(field_name);
                set_pointer(&mut missing, path, value);
                assert!(
                    serde_json::from_value::<WasmRequest>(missing).is_err(),
                    "{name}.{variant} accepted missing field {field_name}"
                );
            }

            let mut missing_tag = expanded_request(&corpus);
            let mut value = template;
            value.as_object_mut().expect("variant object").remove(tag);
            set_pointer(&mut missing_tag, path, value);
            assert!(
                serde_json::from_value::<WasmRequest>(missing_tag).is_err(),
                "{name}.{variant} accepted a missing tag"
            );
        }
    }
}

#[test]
fn schema_names_match_the_serde_and_typescript_contracts() {
    let (schema, _) = documents();
    let typescript = include_str!("../../../web-worker/protocol.generated.d.mts");
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

    assert_wire_value(&response, "AdocWeaveWasmResponse", &schema);
    for path in [
        "/attributeOccurrences/0",
        "/resourceQueries/0",
        "/diagnostics/0",
        "/projection/title",
        "/projection/targets/0",
        "/projection/structure/headings/0",
    ] {
        assert!(
            response.pointer(path).is_some(),
            "response probe must cover {path}"
        );
    }
}

fn assert_wire_value(value: &Value, type_name: &str, schema: &Value) {
    if type_name == "string"
        || type_name == "number"
        || type_name == "boolean"
        || type_name == "unknown"
    {
        return;
    }
    if type_name.ends_with(" | null") {
        if value.is_null() {
            return;
        }
        return assert_wire_value(value, type_name.trim_end_matches(" | null"), schema);
    }
    if let Some(element) = type_name.strip_suffix("[]") {
        for value in value
            .as_array()
            .unwrap_or_else(|| panic!("{type_name} must be an array"))
        {
            assert_wire_value(value, element, schema);
        }
        return;
    }
    if type_name == "Required<ProductSet>" {
        let actual = value
            .as_object()
            .expect("products object")
            .keys()
            .map(String::as_str)
            .collect::<BTreeSet<_>>();
        let expected = schema["products"]
            .as_array()
            .expect("products")
            .iter()
            .map(|product| product["json"].as_str().expect("product name"))
            .collect::<BTreeSet<_>>();
        assert_eq!(actual, expected, "product fields");
        return;
    }
    if schema["enums"].get(type_name).is_some() {
        assert!(
            schema["enums"][type_name]
                .as_array()
                .expect("enum")
                .contains(value),
            "{type_name} value {value}"
        );
        return;
    }
    if let Some(union) = schema["taggedUnions"].get(type_name) {
        let tag = union["tag"].as_str().expect("union tag");
        let variant = value[tag].as_str().expect("union variant");
        let fields = union["variants"][variant]
            .as_array()
            .expect("variant fields");
        let mut tagged_fields = vec![json!({ "json": tag, "type": "string" })];
        tagged_fields.extend(fields.iter().cloned());
        return assert_object(value, &tagged_fields, schema, type_name);
    }
    let contract = if type_name == "AdocWeaveWasmResponse" {
        &schema["response"]
    } else {
        schema["definitions"]
            .get(type_name)
            .or_else(|| schema["dtos"].get(type_name))
            .unwrap_or_else(|| panic!("unknown schema type {type_name}"))
    };
    assert_object(
        value,
        contract["fields"].as_array().expect("contract fields"),
        schema,
        type_name,
    );
}

fn assert_object(value: &Value, schema_fields: &[Value], schema: &Value, name: &str) {
    let actual = value
        .as_object()
        .unwrap_or_else(|| panic!("{name} must be an object"))
        .keys()
        .map(String::as_str)
        .collect::<BTreeSet<_>>();
    let expected = schema_fields
        .iter()
        .map(|field| field["json"].as_str().expect("schema field name"))
        .collect::<BTreeSet<_>>();
    assert_eq!(actual, expected, "{name} fields");
    for field in schema_fields {
        let field_name = field["json"].as_str().expect("field name");
        let field_type = field["type"].as_str().expect("field type");
        assert_wire_value(&value[field_name], field_type, schema);
    }
}
