use std::sync::Arc;

use adocweave::SourceId;
use adocweave::preprocess::{
    DirectiveKind, PreprocessErrorKind, PreprocessFailure, PreprocessInputs, PreprocessOptions,
    PreprocessStep, ProjectionFailure, ProjectionLimits, ResourceDocument, ResourceLookup,
    ResourceLookupResult, ResourceResponse, ResourceSnapshot, SourceMapping, preprocess,
    preprocess_and_analyze_with, preprocess_resumable, preprocess_with,
};
use adocweave::{AnalysisOptions, CancellationToken, Engine, NeverCancel};
use serde_json::Value;

fn public_fixture() -> Value {
    serde_json::from_str(include_str!(
        "../../../fixtures/preprocessor/public-v1.json"
    ))
    .expect("public preprocess fixture")
}

fn fixture_snapshot(fixture: &Value) -> ResourceSnapshot {
    fixture["resources"]
        .as_object()
        .expect("resources")
        .iter()
        .map(|(target, resource)| {
            (
                target.clone(),
                ResourceDocument {
                    source_id: SourceId::new(resource["sourceId"].as_str().expect("sourceId")),
                    source: Arc::from(resource["source"].as_str().expect("resource source")),
                },
            )
        })
        .collect()
}

#[test]
fn public_preprocess_fixture_fixes_source_map_directives_and_notices() {
    let fixture = public_fixture();
    assert_eq!(fixture["schemaVersion"], 1);
    let document = preprocess(
        fixture["source"].as_str().expect("source"),
        &fixture_snapshot(&fixture),
        &PreprocessOptions {
            source_id: Some(SourceId::new(
                fixture["sourceId"].as_str().expect("sourceId"),
            )),
            base_uri: Some(
                fixture["options"]["baseUri"]
                    .as_str()
                    .expect("baseUri")
                    .to_owned(),
            ),
            ..PreprocessOptions::default()
        },
    )
    .expect("public preprocess result");

    assert_eq!(
        document.source,
        fixture["expected"]["source"]
            .as_str()
            .expect("expected source")
    );
    assert_eq!(
        document
            .source_map()
            .iter()
            .map(|segment| {
                segment
                    .origin
                    .source_id
                    .as_ref()
                    .map_or("", SourceId::as_str)
            })
            .collect::<Vec<_>>(),
        fixture["expected"]["sourceIds"]
            .as_array()
            .expect("sourceIds")
            .iter()
            .map(|value| value.as_str().expect("sourceId"))
            .collect::<Vec<_>>()
    );
    assert_eq!(
        document
            .source_map()
            .iter()
            .map(|segment| match segment.mapping {
                SourceMapping::Identity => "identity",
                SourceMapping::WholeOrigin => "whole-origin",
            })
            .collect::<Vec<_>>(),
        fixture["expected"]["mappings"]
            .as_array()
            .expect("mappings")
            .iter()
            .map(|value| value.as_str().expect("mapping"))
            .collect::<Vec<_>>()
    );
    assert_eq!(
        document
            .directives
            .iter()
            .map(|directive| match directive.kind {
                DirectiveKind::Include => "include",
                DirectiveKind::Ifdef => "ifdef",
                DirectiveKind::Ifndef => "ifndef",
                DirectiveKind::Ifeval => "ifeval",
                DirectiveKind::Endif => "endif",
            })
            .collect::<Vec<_>>(),
        fixture["expected"]["directiveKinds"]
            .as_array()
            .expect("directiveKinds")
            .iter()
            .map(|value| value.as_str().expect("directive kind"))
            .collect::<Vec<_>>()
    );
    assert_eq!(
        document
            .notices
            .iter()
            .map(|notice| notice.target.as_str())
            .collect::<Vec<_>>(),
        fixture["expected"]["noticeTargets"]
            .as_array()
            .expect("noticeTargets")
            .iter()
            .map(|value| value.as_str().expect("notice target"))
            .collect::<Vec<_>>()
    );
}

#[test]
fn every_public_processing_limit_accepts_its_boundary_and_rejects_the_next_item() {
    type LimitCase = (
        &'static str,
        fn(&mut PreprocessOptions),
        PreprocessErrorKind,
    );

    let mut snapshot = ResourceSnapshot::default();
    snapshot.insert(
        "part.adoc",
        ResourceDocument {
            source_id: SourceId::new("part"),
            source: Arc::from("text"),
        },
    );
    let cases: [LimitCase; 5] = [
        (
            "include::part.adoc[]\n",
            |options: &mut PreprocessOptions| options.max_include_depth = 0,
            PreprocessErrorKind::DepthLimit,
        ),
        (
            "include::part.adoc[]\n",
            |options: &mut PreprocessOptions| options.max_includes = 0,
            PreprocessErrorKind::IncludeLimit,
        ),
        (
            "text",
            |options: &mut PreprocessOptions| options.max_total_bytes = 3,
            PreprocessErrorKind::ByteLimit,
        ),
        (
            "text",
            |options: &mut PreprocessOptions| options.max_expanded_nodes = 0,
            PreprocessErrorKind::NodeLimit,
        ),
        (
            "text",
            |options: &mut PreprocessOptions| options.max_source_map_segments = 0,
            PreprocessErrorKind::SourceMapLimit,
        ),
    ];
    for (source, configure, expected) in cases {
        let mut options = PreprocessOptions::default();
        configure(&mut options);
        assert_eq!(
            preprocess(source, &snapshot, &options)
                .expect_err("limit must reject the first excess item")
                .kind,
            expected
        );
    }

    let options = PreprocessOptions {
        max_include_depth: 1,
        max_includes: 1,
        max_total_bytes: 4,
        max_expanded_nodes: 2,
        max_source_map_segments: 1,
        ..PreprocessOptions::default()
    };
    let document = preprocess("include::part.adoc[]\n", &snapshot, &options)
        .expect("exact processing boundaries");
    assert_eq!(document.source, "text");
    assert_eq!(document.source_map().len(), 1);
}

#[test]
fn cancellable_preprocess_and_projection_apis_are_public() {
    let cancellation = CancellationToken::new();
    cancellation.cancel();
    assert_eq!(
        preprocess_with(
            "text\n",
            &ResourceSnapshot::default(),
            &PreprocessOptions::default(),
            PreprocessInputs {
                cancellation: Some(&cancellation)
            }
        )
        .expect_err("cancelled preprocess"),
        PreprocessFailure::Cancelled
    );

    let analysis = adocweave::preprocess::preprocess_and_analyze(
        &Engine::new(AnalysisOptions::default()),
        "text\n",
        &ResourceSnapshot::default(),
        &PreprocessOptions::default(),
    )
    .expect("analysis");
    assert_eq!(
        analysis
            .project_origins_cancellable(ProjectionLimits::default(), &cancellation)
            .expect_err("cancelled projection"),
        ProjectionFailure::Cancelled
    );
    assert!(matches!(
        preprocess_and_analyze_with(
            &Engine::new(AnalysisOptions::default()),
            "text\n",
            &ResourceSnapshot::default(),
            &PreprocessOptions::default(),
            PreprocessInputs {
                cancellation: Some(&cancellation)
            }
        ),
        Err(adocweave::preprocess::PreprocessedAnalysisError::Cancelled)
    ));
}

#[test]
fn resumable_preprocess_contract_is_public_without_exposing_continuation_state() {
    struct Deferred;

    impl ResourceLookup for Deferred {
        fn lookup(&self, _target: &str) -> ResourceLookupResult {
            ResourceLookupResult::Deferred
        }
    }

    let PreprocessStep::NeedResource(suspended) = preprocess_resumable(
        "include::part.adoc[]\n",
        &PreprocessOptions::default(),
        &Deferred,
        &NeverCancel,
    ) else {
        panic!("deferred lookup must suspend preprocessing");
    };
    assert_eq!(suspended.request().target(), "part.adoc");
    assert!(!suspended.request().is_optional());
    let step = suspended.resume(
        ResourceResponse::Found(ResourceDocument {
            source_id: SourceId::new("part"),
            source: Arc::from("included\n"),
        }),
        &Deferred,
        &NeverCancel,
    );
    let PreprocessStep::Complete(document) = step else {
        panic!("one supplied resource must complete preprocessing");
    };
    assert_eq!(document.source, "included\n");
}
