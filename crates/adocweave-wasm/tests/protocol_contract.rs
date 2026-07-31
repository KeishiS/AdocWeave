use adocweave::NeverCancel;
use adocweave_wasm::{
    WasmActiveUrlPolicy, WasmAnalysisOptions, WasmAnalysisPreprocessInput, WasmAuthoredUrlPolicy,
    WasmDiagnosticProfile, WasmDocumentMode, WasmError, WasmExternalLinkPolicy, WasmLimits,
    WasmOutputLimits, WasmPreprocessOptions, WasmPreprocessRequest, WasmPreprocessResponse,
    WasmRenderInputs, WasmRenderPolicy, WasmRequest, WasmResolvedReference, WasmResolvedResource,
    WasmResource, WasmResourceCapabilities, WasmResourceOutcome, WasmRuleSettings, WasmSafeMode,
    WasmSourceLanguagePolicy, WasmSourceMapSegment, WasmSourceMapping, WasmStylesheet,
    WasmSyntaxMode, WasmSyntaxOptions, WasmUnknownSourceLanguage,
    WasmUnresolvedReferencePresentation, preprocess_request, process_request,
};
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

#[test]
fn latex_wire_value_remains_distinct_from_the_asciidoc_name() {
    let (schema, _) = documents();
    assert_eq!(schema["enums"]["MathLanguage"], json!(["latex", "typst"]));
    assert_ne!(schema["enums"]["MathLanguage"][0], "latexmath");
    assert_eq!(
        adocweave::semantic::MathLanguage::Latex.as_asciidoc_name(),
        "latexmath"
    );
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
        "attributeQueries": true,
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
    request["preprocess"] = json!({
        "resources": {},
        "options": {}
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
        }],
        "citations": [{
            "sourceStart": 0,
            "sourceEnd": 1,
            "outcome": { "status": "resolved", "segments": [{ "text": "(Smith 2024)" }] }
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
    let mut without_source_id = base_request(&corpus);
    without_source_id
        .as_object_mut()
        .expect("request object")
        .remove("sourceId");
    assert_eq!(
        serde_json::from_value::<WasmRequest>(without_source_id)
            .expect("omitted source ID")
            .source_id,
        None
    );

    assert_eq!(request.products, Default::default());
    assert_eq!(
        serde_json::to_value(request.products).expect("product defaults"),
        schema["browserProductDefault"]
    );
    assert_eq!(request.analysis_options, Default::default());
    assert_eq!(request.render_policy, Default::default());
    assert_eq!(request.output_limits, Default::default());
    assert_schema_defaults(
        &serde_json::to_value(&request.analysis_options).expect("analysis defaults"),
        "AnalysisOptions",
        &schema,
    );
    assert_wire_value(
        &serde_json::to_value(&request).expect("serializable default request"),
        "WasmRequest",
        &schema,
    );
    let expanded: WasmRequest =
        serde_json::from_value(expanded_request(&corpus)).expect("expanded request");
    assert_wire_value(
        &serde_json::to_value(expanded).expect("serializable expanded request"),
        "WasmRequest",
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

#[test]
fn public_request_wire_types_match_the_request_corpus_fixture() {
    fn assert_public<T>() {}

    assert_public::<WasmRequest>();
    assert_public::<WasmAnalysisOptions>();
    assert_public::<WasmSyntaxOptions>();
    assert_public::<WasmLimits>();
    assert_public::<WasmDiagnosticProfile>();
    assert_public::<WasmRuleSettings>();
    assert_public::<WasmAuthoredUrlPolicy>();
    assert_public::<WasmRenderPolicy>();
    assert_public::<WasmActiveUrlPolicy>();
    assert_public::<WasmExternalLinkPolicy>();
    assert_public::<WasmSourceLanguagePolicy>();
    assert_public::<WasmResourceCapabilities>();
    assert_public::<WasmOutputLimits>();
    assert_public::<WasmStylesheet>();

    let (schema, corpus) = documents();
    let request: WasmRequest =
        serde_json::from_value(expanded_request(&corpus)).expect("expanded fixture request");
    let serialized = serde_json::to_value(request).expect("serialized fixture request");
    assert_wire_value(&serialized, "WasmRequest", &schema);
}

#[test]
fn request_modules_keep_wire_normalization_conversion_and_execution_one_way() {
    const FACADE: &str = include_str!("../src/lib.rs");
    const WIRE: &str = include_str!("../src/request_wire.rs");
    const WIRE_GENERATED: &str = include_str!("../src/request_wire_generated.rs");
    const NORMALIZATION: &str = include_str!("../src/request_normalization.rs");
    const CONVERSION: &str = include_str!("../src/request_conversion.rs");
    const RENDER_GENERATED: &str = include_str!("../src/render_input_wire_generated.rs");
    const RENDER_WIRE: &str = include_str!("../src/render_input_wire.rs");
    const RENDER_NORMALIZATION: &str = include_str!("../src/render_input_normalization.rs");
    const RENDER_CONVERSION: &str = include_str!("../src/render_input_conversion.rs");

    assert!(WIRE.contains("protocol/public-api.json"));
    assert!(WIRE.contains("request_wire_generated"));
    assert!(!WIRE.contains("adocweave::"));
    assert!(!WIRE.contains("fn normalize"));
    assert!(!WIRE.contains("ExecutionRequest"));
    assert!(WIRE_GENERATED.starts_with("// @generated"));
    assert!(WIRE_GENERATED.contains("pub struct WasmRequest"));
    assert!(!WIRE_GENERATED.contains("adocweave::"));

    assert!(NORMALIZATION.contains("pub(crate) struct NormalizedRequest"));
    assert!(!NORMALIZATION.contains("adocweave::"));
    assert!(!NORMALIZATION.contains("Engine"));
    assert!(!NORMALIZATION.contains("RenderPolicy"));
    assert!(NORMALIZATION.contains("NormalizedRenderInputs"));

    assert!(CONVERSION.contains("NormalizedRequest"));
    assert!(CONVERSION.contains("NormalizedRenderInputs"));
    assert!(CONVERSION.contains("pub(crate) struct ExecutionRequest"));
    assert!(CONVERSION.contains("pub(crate) enum ProcessingExecution"));
    assert!(CONVERSION.contains("Engine"));
    assert!(CONVERSION.contains("RenderPolicy"));
    assert!(!CONVERSION.contains("pub(crate) engine:"));
    assert!(!CONVERSION.contains("pub(crate) preprocess:"));
    assert!(!CONVERSION.contains("analyze_cancellable"));

    assert!(!FACADE.contains("pub struct WasmRequest"));
    assert!(FACADE.contains("request_normalization::normalize(request)?"));
    assert!(FACADE.contains("request_conversion::convert(request)?"));
    assert!(FACADE.contains("fn execute_request("));

    assert!(RENDER_GENERATED.starts_with("// @generated"));
    assert!(RENDER_GENERATED.contains("pub struct WasmRenderInputs"));
    assert!(!RENDER_GENERATED.contains("adocweave::"));
    assert!(RENDER_WIRE.contains("render_input_wire_generated"));
    assert!(!RENDER_WIRE.contains("adocweave::"));
    assert!(RENDER_NORMALIZATION.contains("pub(crate) struct NormalizedRenderInputs"));
    assert!(!RENDER_NORMALIZATION.contains("adocweave::"));
    assert!(!RENDER_NORMALIZATION.contains("use adocweave::Analysis"));
    assert!(RENDER_CONVERSION.contains("inputs: NormalizedRenderInputs"));
    assert!(RENDER_CONVERSION.contains("analysis: &Analysis"));
}

#[test]
fn handwritten_wasm_wire_dtos_require_a_type_specific_exception() {
    const HANDWRITTEN_SOURCES: &[(&str, &str)] = &[
        ("lib.rs", include_str!("../src/lib.rs")),
        (
            "preprocess_wire.rs",
            include_str!("../src/preprocess_wire.rs"),
        ),
        (
            "render_input_conversion.rs",
            include_str!("../src/render_input_conversion.rs"),
        ),
        (
            "preprocess_projection.rs",
            include_str!("../src/preprocess_projection.rs"),
        ),
        (
            "render_input_normalization.rs",
            include_str!("../src/render_input_normalization.rs"),
        ),
        (
            "render_input_wire.rs",
            include_str!("../src/render_input_wire.rs"),
        ),
        (
            "request_conversion.rs",
            include_str!("../src/request_conversion.rs"),
        ),
        (
            "request_normalization.rs",
            include_str!("../src/request_normalization.rs"),
        ),
        ("request_wire.rs", include_str!("../src/request_wire.rs")),
        (
            "response_projection.rs",
            include_str!("../src/response_projection.rs"),
        ),
        ("response_wire.rs", include_str!("../src/response_wire.rs")),
    ];
    const HANDWRITTEN_WIRE_DTO_EXCEPTIONS: &[(&str, &str, &str)] = &[];

    let declared = HANDWRITTEN_SOURCES
        .iter()
        .flat_map(|(path, source)| {
            source.lines().filter_map(move |line| {
                let declaration = line
                    .trim()
                    .strip_prefix("pub struct ")
                    .or_else(|| line.trim().strip_prefix("pub enum "))?;
                let name = declaration
                    .split(|character: char| !character.is_ascii_alphanumeric())
                    .next()
                    .expect("public type name");
                Some(((*path).to_owned(), name.to_owned()))
            })
        })
        .collect::<BTreeSet<_>>();
    let excepted = HANDWRITTEN_WIRE_DTO_EXCEPTIONS
        .iter()
        .map(|(path, name, reason)| {
            assert!(
                !reason.trim().is_empty(),
                "{path}:{name} has no exception reason"
            );
            ((*path).to_owned(), (*name).to_owned())
        })
        .collect::<BTreeSet<_>>();

    assert_eq!(
        declared, excepted,
        "every handwritten WASM wire DTO must have a type-specific exception"
    );
}

#[test]
fn generated_render_inputs_match_the_schema_safe_integer_boundary() {
    fn assert_public<T>() {}
    assert_public::<WasmRenderInputs>();
    assert_public::<WasmResolvedReference>();
    assert_public::<WasmResolvedResource>();

    let outcome = |byte_length: Value| {
        serde_json::from_value::<WasmResourceOutcome>(json!({
            "status": "resolved",
            "href": "asset.png",
            "mediaType": "image/png",
            "byteLength": byte_length
        }))
    };
    assert!(outcome(json!(9_007_199_254_740_991_u64)).is_ok());
    for invalid in [json!(9_007_199_254_740_992_u64), json!(-1), json!(1.5)] {
        assert!(
            outcome(invalid.clone()).is_err(),
            "byteLength accepted {invalid}"
        );
    }
}

#[test]
fn generated_preprocess_wire_keeps_the_public_api_and_schema_defaults() {
    let (schema, _) = documents();
    let options = WasmPreprocessOptions::default();
    assert_schema_defaults(
        &serde_json::to_value(&options).expect("preprocess defaults"),
        "PreprocessOptions",
        &schema,
    );
    assert_eq!(options.safe_mode, WasmSafeMode::Secure);

    let resource = WasmResource {
        source_id: "file:///chapter.adoc".to_owned(),
        source: "= Chapter".to_owned(),
    };
    let analysis = WasmAnalysisPreprocessInput {
        resources: std::collections::BTreeMap::from([(
            "chapter.adoc".to_owned(),
            resource.clone(),
        )]),
        options: options.clone(),
    };
    let request = WasmPreprocessRequest {
        package_version: adocweave::VERSION.to_owned(),
        source_id: None,
        source: "include::chapter.adoc[]".to_owned(),
        resources: analysis.resources,
        options: analysis.options,
    };
    assert_wire_value(
        &serde_json::to_value(request).expect("public preprocess request"),
        "PreprocessRequest",
        &schema,
    );
    let response = WasmPreprocessResponse {
        package_version: adocweave::VERSION.to_owned(),
        source: "expanded".to_owned(),
        source_map: vec![WasmSourceMapSegment {
            output_start: 0,
            output_end: 8,
            source_id: Some("source".to_owned()),
            source_start: 1,
            source_end: 9,
            mapping: WasmSourceMapping::Identity,
        }],
    };
    assert_wire_value(
        &serde_json::to_value(response).expect("public preprocess response"),
        "PreprocessResponse",
        &schema,
    );
    assert_wire_value(
        &serde_json::to_value(WasmError {
            code: "invalid-request".to_owned(),
            message: "request is invalid".to_owned(),
        })
        .expect("public WASM error"),
        "WasmError",
        &schema,
    );

    const GENERATED: &str = include_str!("../src/preprocess_wire_generated.rs");
    assert!(GENERATED.starts_with("// @generated"));
    for name in [
        "WasmPreprocessResponse",
        "WasmSourceMapSegment",
        "WasmSourceMapping",
        "WasmError",
    ] {
        assert!(GENERATED.contains(name), "{name}");
    }
    assert!(!GENERATED.contains("adocweave::"));
}

#[test]
fn preprocess_corpus_without_expansion_limits_uses_schema_defaults() {
    let (_, corpus) = documents();
    let request: WasmPreprocessRequest =
        serde_json::from_value(corpus["preprocessRequest"].clone())
            .expect("schema 6 compatible preprocess request");
    let defaults = WasmPreprocessOptions::default();

    assert_eq!(
        request.options.max_attribute_expansion_depth,
        defaults.max_attribute_expansion_depth
    );
    assert_eq!(
        request.options.max_attribute_expansion_bytes,
        defaults.max_attribute_expansion_bytes
    );
}

#[test]
fn generated_request_enums_keep_the_public_api_and_schema_defaults() {
    let (schema, _) = documents();
    let cases = [
        (
            "SyntaxMode",
            serde_json::to_value(WasmSyntaxMode::Permissive).expect("syntax mode"),
            serde_json::to_value(WasmSyntaxMode::default()).expect("default syntax mode"),
        ),
        (
            "DocumentMode",
            serde_json::to_value(WasmDocumentMode::Fragment).expect("document mode"),
            serde_json::to_value(WasmDocumentMode::default()).expect("default document mode"),
        ),
        (
            "UnknownSourceLanguage",
            serde_json::to_value(WasmUnknownSourceLanguage::PreserveSanitized)
                .expect("unknown source language"),
            serde_json::to_value(WasmUnknownSourceLanguage::default())
                .expect("default unknown source language"),
        ),
        (
            "UnresolvedReferencePresentation",
            serde_json::to_value(WasmUnresolvedReferencePresentation::Target)
                .expect("unresolved reference presentation"),
            serde_json::to_value(WasmUnresolvedReferencePresentation::default())
                .expect("default unresolved reference presentation"),
        ),
    ];
    for (name, public_value, default_value) in cases {
        let expected = schema["enums"][name][0].clone();
        assert_eq!(public_value, expected, "{name} public JSON value");
        assert_eq!(default_value, expected, "{name} default JSON value");
    }

    const GENERATED: &str = include_str!("../src/request_enum_generated.rs");
    assert!(GENERATED.starts_with("// @generated"));
    for name in [
        "WasmSyntaxMode",
        "WasmDocumentMode",
        "WasmUnknownSourceLanguage",
        "WasmUnresolvedReferencePresentation",
    ] {
        assert!(GENERATED.contains(&format!("pub enum {name}")));
    }
}

fn assert_schema_defaults(value: &Value, name: &str, schema: &Value) {
    let contract = schema["settings"]
        .get(name)
        .or_else(|| schema["definitions"].get(name))
        .or_else(|| schema["preprocessDefinitions"].get(name))
        .unwrap_or_else(|| panic!("default contract {name}"));
    for field in contract["fields"].as_array().expect("default fields") {
        let field_name = field["json"].as_str().expect("default field name");
        assert_field_default(
            &value[field_name],
            field,
            schema,
            &format!("{name}.{field_name}"),
        );
    }
}

fn assert_field_default(value: &Value, field: &Value, schema: &Value, name: &str) {
    let default = &field["default"];
    if default == "browser-default" {
        assert_eq!(value, &schema["browserProductDefault"], "{name} default");
        return;
    }
    let nested_type = field["type"].as_str().expect("nested default type");
    let has_nested_contract = schema["settings"].get(nested_type).is_some()
        || schema["definitions"].get(nested_type).is_some()
        || schema["preprocessDefinitions"].get(nested_type).is_some();
    if default.as_object().is_some_and(serde_json::Map::is_empty) && has_nested_contract {
        assert_schema_defaults(value, nested_type, schema);
    } else {
        assert_eq!(value, default, "{name} default");
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
fn schema_mutations_enforce_every_request_object_field_recursively() {
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
            let field_name = field["json"].as_str().expect("field name");
            if field["type"] == "u32" {
                for invalid in [json!(-1), json!(1.5), json!(4_294_967_296_u64)] {
                    let mut request = expanded_request(&corpus);
                    request
                        .pointer_mut(path)
                        .expect("numeric object")
                        .as_object_mut()
                        .expect("numeric map")
                        .insert(field_name.to_owned(), invalid.clone());
                    assert!(
                        serde_json::from_value::<WasmRequest>(request).is_err(),
                        "{name}.{field_name} accepted {invalid}"
                    );
                }
            }
            let mut missing = expanded_request(&corpus);
            missing
                .pointer_mut(path)
                .expect("object path")
                .as_object_mut()
                .expect("object")
                .remove(field_name);
            if field["required"].as_bool() == Some(true) {
                assert!(
                    serde_json::from_value::<WasmRequest>(missing).is_err(),
                    "{name} accepted missing required field {field_name}"
                );
            } else {
                let request =
                    serde_json::from_value::<WasmRequest>(missing).unwrap_or_else(|error| {
                        panic!("{name} rejected defaulted field {field_name}: {error}")
                    });
                let serialized = serde_json::to_value(request).expect("serialized default probe");
                let object = serialized.pointer(path).expect("serialized object");
                assert_field_default(
                    &object[field_name],
                    field,
                    &schema,
                    &format!("{name}.{field_name}"),
                );
            }
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
            let valid = serde_json::from_value::<WasmRequest>(valid)
                .unwrap_or_else(|error| panic!("{name}.{variant} was rejected: {error}"));
            let serialized = serde_json::to_value(valid).expect("serialized union probe");
            assert_wire_value(
                serialized.pointer(path).expect("serialized union"),
                name,
                &schema,
            );

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
        "attributeQueries": true,
        "resourceQueries": true,
        "diagnostics": true,
        "symbols": true,
        "projection": true
    });
    let request: WasmRequest =
        serde_json::from_value(value.clone()).expect("response probe request");
    let response =
        serde_json::to_value(process_request(request, &NeverCancel).expect("response probe"))
            .expect("serializable response");

    assert_wire_value(&response, "AdocWeaveWasmResponse", &schema);
    for path in [
        "/attributeOccurrences/0",
        "/attributeQueries/bindings/0",
        "/attributeQueries/references/0",
        "/resourceQueries/0",
        "/diagnostics/0",
        "/projection/title",
        "/projection/targets/0",
        "/projection/structure/headings/0",
        "/projection/structure/toc/0",
        "/projection/sourceBlocks/0",
        "/projection/formulas/0",
        "/projection/orderedLists/0",
        "/projection/blockPresentations/0",
        "/projection/externalLinks/0",
        "/projection/referenceEdges/0",
        "/projection/searchableText/segments/0",
        "/projection/catalogs/footnotes/0",
        "/projection/catalogs/bibliography/0",
        "/projection/catalogs/index/0",
        "/symbols/0",
    ] {
        assert!(
            response.pointer(path).is_some(),
            "response probe must cover {path}"
        );
    }

    let edges = response["projection"]["referenceEdges"]
        .as_array()
        .expect("reference edges");
    let target_kinds = edges
        .iter()
        .map(|edge| edge["target"]["kind"].as_str().expect("target kind"))
        .collect::<BTreeSet<_>>();
    assert_eq!(
        target_kinds,
        BTreeSet::from(["document", "local", "scheme"]),
        "every ReferenceKey variant needs a non-empty wire witness"
    );
    value["renderInputs"] = json!({
        "references": [
            {
                "sourceStart": edges[0]["sourceRange"]["start"],
                "sourceEnd": edges[0]["sourceRange"]["end"],
                "outcome": {
                    "status": "resolved",
                    "href": "#first",
                    "notices": ["fallback"]
                }
            },
            {
                "sourceStart": edges[1]["sourceRange"]["start"],
                "sourceEnd": edges[1]["sourceRange"]["end"],
                "outcome": { "status": "failed", "kind": "missing-target" }
            }
        ]
    });
    let resolved = process_request(
        serde_json::from_value(value).expect("resolved response probe"),
        &NeverCancel,
    )
    .expect("resolved response");
    let resolved = serde_json::to_value(resolved).expect("serialized resolved response");
    assert_wire_value(&resolved, "AdocWeaveWasmResponse", &schema);
    assert_eq!(
        resolved["projection"]["referenceEdges"][0]["resolution"]["status"],
        "resolved"
    );
    assert_eq!(
        resolved["projection"]["referenceEdges"][0]["resolution"]["notices"],
        json!(["reference-resolution-fallback"])
    );
    assert_eq!(
        resolved["projection"]["referenceEdges"][1]["resolution"]["status"],
        "failed"
    );
}

#[test]
fn manpage_projection_is_a_non_empty_typed_schema_witness() {
    let (schema, corpus) = documents();
    let mut value = base_request(&corpus);
    value["source"] = json!(
        "= adocweave(1)\n:doctype: manpage\n\n== NAME\n\nadocweave - convert AsciiDoc safely\n"
    );
    value["products"] = json!({
        "syntax": false,
        "canonicalAst": false,
        "html": false,
        "attributeOccurrences": false,
        "attributeQueries": false,
        "resourceQueries": false,
        "diagnostics": false,
        "symbols": false,
        "projection": true
    });
    let response = process_request(
        serde_json::from_value(value).expect("manpage request"),
        &NeverCancel,
    )
    .expect("manpage response");
    let response = serde_json::to_value(response).expect("serialized manpage response");

    assert_wire_value(&response, "AdocWeaveWasmResponse", &schema);
    for field in [
        "name",
        "section",
        "purpose",
        "titleRange",
        "nameRange",
        "purposeRange",
    ] {
        assert!(
            response["projection"]["structure"]["manpage"]
                .get(field)
                .is_some(),
            "manpage witness must contain {field}"
        );
    }
}

#[test]
fn stable_typed_products_use_explicit_disabled_sentinels() {
    let (_, corpus) = documents();
    let mut disabled = base_request(&corpus);
    disabled["products"] = json!({
        "syntax": false,
        "canonicalAst": false,
        "html": false,
        "attributeOccurrences": false,
        "attributeQueries": false,
        "resourceQueries": false,
        "diagnostics": false,
        "symbols": false,
        "projection": false
    });
    let request: WasmRequest = serde_json::from_value(disabled).expect("disabled request");
    let response = process_request(request, &NeverCancel).expect("default response");

    assert!(response.diagnostics.is_empty());
    assert!(response.render_diagnostics.is_empty());
    assert!(response.symbols.is_empty());
    assert!(response.projection.is_none());

    let mut enabled = base_request(&corpus);
    enabled["source"] = corpus["responseProbe"]["source"].clone();
    enabled["products"] = json!({
        "syntax": false,
        "canonicalAst": false,
        "html": true,
        "attributeOccurrences": false,
        "attributeQueries": false,
        "resourceQueries": false,
        "diagnostics": true,
        "symbols": true,
        "projection": true
    });
    let response = process_request(
        serde_json::from_value(enabled).expect("enabled request"),
        &NeverCancel,
    )
    .expect("enabled response");
    assert!(!response.diagnostics.is_empty());
    assert!(!response.symbols.is_empty());
    assert!(response.projection.is_some());
}

#[test]
fn preprocess_wire_contract_round_trips_and_rejects_drift() {
    let (schema, corpus) = documents();
    let mut value = corpus["preprocessRequest"].clone();
    value["packageVersion"] = Value::String(adocweave::VERSION.to_owned());
    let request: WasmPreprocessRequest =
        serde_json::from_value(value.clone()).expect("preprocess request");
    assert_wire_value(
        &serde_json::to_value(&request).expect("serialized preprocess request"),
        "PreprocessRequest",
        &schema,
    );
    let defaults: WasmPreprocessRequest = serde_json::from_value(json!({
        "packageVersion": adocweave::VERSION,
        "source": "text"
    }))
    .expect("default preprocess request");
    assert_eq!(defaults.source_id, None);
    assert_schema_defaults(
        &serde_json::to_value(&defaults.options).expect("preprocess defaults"),
        "PreprocessOptions",
        &schema,
    );
    let response = preprocess_request(request).expect("preprocess response");
    assert_wire_value(
        &serde_json::to_value(response).expect("serialized preprocess response"),
        "PreprocessResponse",
        &schema,
    );

    for case in corpus["preprocessObjectCases"]
        .as_array()
        .expect("preprocess object cases")
    {
        let name = case["object"].as_str().expect("preprocess object name");
        let path = case["path"].as_str().expect("preprocess object path");
        let contract = if name == "PreprocessRequest" {
            &schema["preprocessRequest"]
        } else {
            &schema["preprocessDefinitions"][name]
        };
        let mut unknown = value.clone();
        unknown
            .pointer_mut(path)
            .expect("preprocess object")
            .as_object_mut()
            .expect("preprocess map")
            .insert("unexpected".to_owned(), Value::Bool(true));
        assert!(
            serde_json::from_value::<WasmPreprocessRequest>(unknown).is_err(),
            "preprocess unknown field at {path}"
        );
        for field in contract["fields"].as_array().expect("preprocess fields") {
            let field_name = field["json"].as_str().expect("preprocess field");
            if field["type"] == "u32" {
                for invalid in [json!(-1), json!(1.5), json!(4_294_967_296_u64)] {
                    let mut request = value.clone();
                    request
                        .pointer_mut(path)
                        .expect("preprocess numeric object")
                        .as_object_mut()
                        .expect("preprocess numeric map")
                        .insert(field_name.to_owned(), invalid.clone());
                    assert!(
                        serde_json::from_value::<WasmPreprocessRequest>(request).is_err(),
                        "{name}.{field_name} accepted {invalid}"
                    );
                }
            }
            let mut missing = value.clone();
            missing
                .pointer_mut(path)
                .expect("preprocess object path")
                .as_object_mut()
                .expect("preprocess object")
                .remove(field_name);
            if field["required"].as_bool() == Some(true) {
                assert!(
                    serde_json::from_value::<WasmPreprocessRequest>(missing).is_err(),
                    "{name} accepted missing {field_name}"
                );
            } else {
                let request = serde_json::from_value::<WasmPreprocessRequest>(missing)
                    .unwrap_or_else(|error| panic!("{name}.{field_name}: {error}"));
                let serialized =
                    serde_json::to_value(request).expect("serialized preprocess default");
                assert_field_default(
                    &serialized
                        .pointer(path)
                        .expect("serialized preprocess object")[field_name],
                    field,
                    &schema,
                    &format!("{name}.{field_name}"),
                );
            }
        }
    }
    for mode in schema["enums"]["SafeMode"].as_array().expect("safe modes") {
        let mut request = value.clone();
        request["options"]["safeMode"] = mode.clone();
        serde_json::from_value::<WasmPreprocessRequest>(request)
            .unwrap_or_else(|error| panic!("safe mode {mode}: {error}"));
    }
    let mut invalid_mode = value;
    invalid_mode["options"]["safeMode"] = Value::String("invalid".to_owned());
    assert!(serde_json::from_value::<WasmPreprocessRequest>(invalid_mode).is_err());

    let mut old = corpus["preprocessRequest"].clone();
    old["packageVersion"] = Value::String(corpus["oldVersion"].as_str().expect("old").to_owned());
    let error = preprocess_request(serde_json::from_value(old).expect("old preprocess request"))
        .expect_err("old preprocess package");
    assert_wire_value(
        &serde_json::to_value(error).expect("serialized preprocess error"),
        "WasmError",
        &schema,
    );
}

fn assert_wire_value(value: &Value, type_name: &str, schema: &Value) {
    if type_name == "string" {
        assert!(value.is_string(), "expected string, got {value}");
        return;
    }
    if type_name == "number" {
        assert!(value.is_number(), "expected number, got {value}");
        return;
    }
    if type_name == "u32" {
        assert!(
            value
                .as_u64()
                .is_some_and(|value| value <= u64::from(u32::MAX)),
            "expected u32, got {value}"
        );
        return;
    }
    if type_name == "safeInteger" {
        assert!(
            value
                .as_u64()
                .is_some_and(|value| value <= 9_007_199_254_740_991),
            "expected safe integer, got {value}"
        );
        return;
    }
    if type_name == "boolean" {
        assert!(value.is_boolean(), "expected boolean, got {value}");
        return;
    }
    if type_name == "unknown" {
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
    if let Some(element) = type_name
        .strip_prefix("Record<string, ")
        .and_then(|value| value.strip_suffix('>'))
    {
        for value in value
            .as_object()
            .unwrap_or_else(|| panic!("{type_name} must be an object"))
            .values()
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
    } else if type_name == "WasmRequest" {
        &schema["request"]
    } else if type_name == "ProductSet" {
        &schema["productSet"]
    } else if type_name == "PreprocessRequest" {
        &schema["preprocessRequest"]
    } else {
        schema["settings"]
            .get(type_name)
            .or_else(|| schema["definitions"].get(type_name))
            .or_else(|| schema["preprocessDefinitions"].get(type_name))
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
