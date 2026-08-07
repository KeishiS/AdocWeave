use std::cell::Cell;
use std::sync::atomic::{AtomicUsize, Ordering};

use super::*;
use crate::cancellation::CHECKPOINT_INTERVAL;

struct CancelAfter {
    checks: AtomicUsize,
    completed_checks: usize,
}

impl CancellationCheck for CancelAfter {
    fn is_cancelled(&self) -> bool {
        self.checks.fetch_add(1, Ordering::Relaxed) >= self.completed_checks
    }
}

struct DeferredLookup<'a> {
    snapshot: &'a ResourceSnapshot,
    lookups: Cell<usize>,
}

impl ResourceLookup for DeferredLookup<'_> {
    fn lookup(&self, _target: &str) -> ResourceLookupResult {
        self.lookups.set(self.lookups.get().saturating_add(1));
        ResourceLookupResult::Deferred
    }
}

fn deferred_preprocess(
    source: &str,
    snapshot: &ResourceSnapshot,
    options: &PreprocessOptions,
) -> (
    Result<PreprocessedDocument, PreprocessError>,
    Vec<String>,
    usize,
) {
    let lookup = DeferredLookup {
        snapshot,
        lookups: Cell::new(0),
    };
    let mut requests = Vec::new();
    let mut step = preprocess_resumable(source, options, &lookup, &NeverCancel);
    loop {
        match step {
            PreprocessStep::Complete(document) => {
                return (Ok(document), requests, lookup.lookups.get());
            }
            PreprocessStep::NeedResource(suspended) => {
                let target = suspended.request().target().to_owned();
                requests.push(target.clone());
                let response = lookup.snapshot.get(&target).cloned().map_or_else(
                    || suspended.request().not_found(),
                    |document| suspended.request().found(document),
                );
                step = suspended.resume(response, &lookup, &NeverCancel);
            }
            PreprocessStep::Failed(error) => {
                return (Err(error), requests, lookup.lookups.get());
            }
            PreprocessStep::HostError(error) => panic!("unexpected host error: {error}"),
            PreprocessStep::Cancelled => panic!("NeverCancel cannot cancel preprocessing"),
        }
    }
}

fn resource(source_id: &str, source: impl Into<Arc<str>>) -> ResourceDocument {
    ResourceDocument {
        source_id: SourceId::new(source_id),
        source: source.into(),
    }
}

#[test]
fn flat_deferred_resources_resume_once_without_reprocessing_directives() {
    const INCLUDE_COUNT: usize = 32;
    let source = (0..INCLUDE_COUNT)
        .map(|index| format!("include::part-{index}.adoc[]\n"))
        .collect::<String>();
    let snapshot = (0..INCLUDE_COUNT)
        .map(|index| {
            (
                format!("part-{index}.adoc"),
                resource(&format!("part-{index}"), format!("part {index}\n")),
            )
        })
        .collect::<ResourceSnapshot>();
    let options = PreprocessOptions::default();
    let expected = preprocess(&source, &snapshot, &options).expect("one-shot preprocessing");
    RESUMABLE_INCLUDE_VISITS.with(|visits| visits.set(0));
    RESUMABLE_LINE_VISITS.with(|visits| visits.set(0));

    let (actual, requests, lookups) = deferred_preprocess(&source, &snapshot, &options);
    let actual = actual.expect("resumable preprocessing");

    assert_eq!(actual, expected);
    assert_eq!(actual.source_map(), expected.source_map());
    assert_eq!(actual.directives, expected.directives);
    assert_eq!(actual.notices, expected.notices);
    assert_eq!(requests.len(), INCLUDE_COUNT);
    assert_eq!(lookups, INCLUDE_COUNT);
    RESUMABLE_INCLUDE_VISITS.with(|visits| assert_eq!(visits.get(), INCLUDE_COUNT));
    RESUMABLE_LINE_VISITS.with(|visits| assert_eq!(visits.get(), INCLUDE_COUNT * 2));
}

#[test]
fn nested_and_attribute_dependent_includes_preserve_read_order() {
    let mut snapshot = ResourceSnapshot::default();
    snapshot.insert(
        "attributes.adoc",
        resource("attributes", ":selected: nested\ninclude::child.adoc[]\n"),
    );
    snapshot.insert(
        "child.adoc",
        resource("child", "child\ninclude::grandchild.adoc[]\n"),
    );
    snapshot.insert("grandchild.adoc", resource("grandchild", "grandchild\n"));
    snapshot.insert("nested.adoc", resource("nested", "selected\n"));
    let source = "include::attributes.adoc[]\ninclude::{selected}.adoc[]\n";
    let options = PreprocessOptions::default();
    let expected = preprocess(source, &snapshot, &options).expect("one-shot preprocessing");
    RESUMABLE_INCLUDE_VISITS.with(|visits| visits.set(0));
    RESUMABLE_LINE_VISITS.with(|visits| visits.set(0));

    let (actual, requests, _) = deferred_preprocess(source, &snapshot, &options);

    assert_eq!(actual.expect("resumable preprocessing"), expected);
    assert_eq!(
        requests,
        [
            "attributes.adoc",
            "child.adoc",
            "grandchild.adoc",
            "nested.adoc"
        ]
    );
    RESUMABLE_INCLUDE_VISITS.with(|visits| assert_eq!(visits.get(), 4));
    RESUMABLE_LINE_VISITS.with(|visits| assert_eq!(visits.get(), 8));
}

#[test]
fn deferred_selection_and_transformations_preserve_unicode_crlf_source_maps() {
    let source = "前\r\ninclude::part.adoc[tags=keep,lines=2..4,indent=2,leveloffset=+1]\r\n後\r\n";
    let mut snapshot = ResourceSnapshot::default();
    snapshot.insert(
        "part.adoc",
        resource(
            "part",
            "// tag::keep[]\r\n= 日本語🙂\r\n本文\r\n// end::keep[]\r\n除外\r\n",
        ),
    );
    let expected = preprocess(source, &snapshot, &PreprocessOptions::default())
        .expect("one-shot preprocessing");

    let (actual, requests, _) =
        deferred_preprocess(source, &snapshot, &PreprocessOptions::default());
    let actual = actual.expect("resumable preprocessing");

    assert_eq!(actual, expected);
    assert_eq!(actual.source, "前\r\n  == 日本語🙂\r\n  本文\r\n後\r\n");
    assert_eq!(requests, ["part.adoc"]);
    assert!(
        actual
            .source_map()
            .iter()
            .any(|segment| segment.mapping == SourceMapping::WholeOrigin)
    );
}

#[test]
fn attributes_and_depth_state_survive_multiple_resumes() {
    let source = ":part: child- \\\n  one\ninclude::{part}.adoc[]\n";
    let mut snapshot = ResourceSnapshot::default();
    snapshot.insert(
        "child- one.adoc",
        resource("child", ":next: grandchild\ninclude::{next}.adoc[]\n"),
    );
    snapshot.insert("grandchild.adoc", resource("grandchild", "完了\n"));
    let options = PreprocessOptions {
        max_include_depth: 2,
        ..PreprocessOptions::default()
    };
    let expected = preprocess(source, &snapshot, &options).expect("one-shot preprocessing");

    let (actual, requests, _) = deferred_preprocess(source, &snapshot, &options);

    assert_eq!(actual.expect("resumable preprocessing"), expected);
    assert_eq!(requests, ["child- one.adoc", "grandchild.adoc"]);
}

#[test]
fn terminal_include_validation_precedes_lookup() {
    let snapshot = ResourceSnapshot::default();
    let lookup = DeferredLookup {
        snapshot: &snapshot,
        lookups: Cell::new(0),
    };
    let step = preprocess_resumable(
        "include::../outside.adoc[]\n",
        &PreprocessOptions::default(),
        &lookup,
        &NeverCancel,
    );

    assert!(matches!(
        step,
        PreprocessStep::Failed(PreprocessError {
            kind: PreprocessErrorKind::UnsafeTarget,
            ..
        })
    ));
    assert_eq!(lookup.lookups.get(), 0);
}

#[test]
fn stale_or_wrong_response_is_a_terminal_host_error() {
    let snapshot = ResourceSnapshot::default();
    let lookup = DeferredLookup {
        snapshot: &snapshot,
        lookups: Cell::new(0),
    };
    let PreprocessStep::NeedResource(first) = preprocess_resumable(
        "include::one.adoc[optional]\ninclude::two.adoc[optional]\n",
        &PreprocessOptions::default(),
        &lookup,
        &NeverCancel,
    ) else {
        panic!("first request");
    };
    let stale = first.request().not_found();
    let PreprocessStep::NeedResource(second) = first.resume(stale.clone(), &lookup, &NeverCancel)
    else {
        panic!("second request");
    };

    let PreprocessStep::HostError(error) = second.resume(stale, &lookup, &NeverCancel) else {
        panic!("mismatched response must fail");
    };
    assert_eq!(error.kind(), HostResourceErrorKind::ResponseMismatch);
    assert_eq!(error.target(), "two.adoc");
}

#[test]
fn response_from_an_identical_request_in_another_run_is_rejected() {
    let snapshot = ResourceSnapshot::default();
    let lookup = DeferredLookup {
        snapshot: &snapshot,
        lookups: Cell::new(0),
    };
    let source = "include::part.adoc[optional]\n";
    let PreprocessStep::NeedResource(first_run) =
        preprocess_resumable(source, &PreprocessOptions::default(), &lookup, &NeverCancel)
    else {
        panic!("first run must request the resource");
    };
    let PreprocessStep::NeedResource(second_run) =
        preprocess_resumable(source, &PreprocessOptions::default(), &lookup, &NeverCancel)
    else {
        panic!("second run must request the resource");
    };
    assert_eq!(first_run.request().target(), second_run.request().target());
    assert_eq!(
        first_run.request().is_optional(),
        second_run.request().is_optional()
    );
    assert_eq!(
        first_run.request().source_id(),
        second_run.request().source_id()
    );
    assert_eq!(first_run.request().range(), second_run.request().range());
    let wrong_response = first_run.request().not_found();

    let PreprocessStep::HostError(error) = second_run.resume(wrong_response, &lookup, &NeverCancel)
    else {
        panic!("a response from another run must fail");
    };
    assert_eq!(error.kind(), HostResourceErrorKind::ResponseMismatch);
    assert_eq!(error.target(), "part.adoc");
}

#[test]
fn host_load_failure_discards_the_continuation() {
    let snapshot = ResourceSnapshot::default();
    let lookup = DeferredLookup {
        snapshot: &snapshot,
        lookups: Cell::new(0),
    };
    let PreprocessStep::NeedResource(suspended) = preprocess_resumable(
        "include::part.adoc[]\n",
        &PreprocessOptions::default(),
        &lookup,
        &NeverCancel,
    ) else {
        panic!("request");
    };
    let response = suspended.request().load_failed("host read failed");

    let PreprocessStep::HostError(error) = suspended.resume(response, &lookup, &NeverCancel) else {
        panic!("load failure must be terminal");
    };
    assert_eq!(error.kind(), HostResourceErrorKind::LoadFailed);
    assert_eq!(error.target(), "part.adoc");
    assert_eq!(error.message(), "host read failed");
}

#[test]
fn synchronous_lookup_failure_is_a_terminal_host_error() {
    struct FailedLookup;

    impl ResourceLookup for FailedLookup {
        fn lookup(&self, _target: &str) -> ResourceLookupResult {
            ResourceLookupResult::Failed("host lookup failed".to_owned())
        }
    }

    let PreprocessStep::HostError(error) = preprocess_resumable(
        "include::part.adoc[]\n",
        &PreprocessOptions::default(),
        &FailedLookup,
        &NeverCancel,
    ) else {
        panic!("lookup failure must be terminal");
    };
    assert_eq!(error.kind(), HostResourceErrorKind::LoadFailed);
    assert_eq!(error.target(), "part.adoc");
    assert_eq!(error.message(), "host lookup failed");
}

#[test]
fn selection_transform_and_resume_stages_observe_cancellation() {
    let finish_cancellation = CancelAfter {
        checks: AtomicUsize::new(0),
        completed_checks: 1,
    };
    assert!(matches!(
        preprocess_resumable(
            "",
            &PreprocessOptions::default(),
            &ResourceSnapshot::default(),
            &finish_cancellation,
        ),
        PreprocessStep::Cancelled
    ));
    assert_eq!(finish_cancellation.checks.load(Ordering::Relaxed), 2);

    let cancellation = crate::core::CancellationToken::new();
    cancellation.cancel();
    assert!(select_lines("one\ntwo\n", &BTreeMap::new(), &cancellation).is_err());
    assert!(matches!(
        transform_lines(
            vec![SelectedLine {
                text: "one\n".to_owned(),
                range: range(0, 4),
                mapping: SourceMapping::Identity,
            }],
            &BTreeMap::new(),
            usize::MAX,
            &cancellation,
        ),
        Err(TransformFailure::Cancelled)
    ));

    let snapshot = ResourceSnapshot::default();
    let lookup = DeferredLookup {
        snapshot: &snapshot,
        lookups: Cell::new(0),
    };
    let PreprocessStep::NeedResource(suspended) = preprocess_resumable(
        "include::part.adoc[]\n",
        &PreprocessOptions::default(),
        &lookup,
        &NeverCancel,
    ) else {
        panic!("request");
    };
    let response = suspended.request().not_found();
    assert!(matches!(
        suspended.resume(response, &lookup, &cancellation),
        PreprocessStep::Cancelled
    ));
}

#[test]
fn suspended_condition_stack_never_requests_an_unreachable_resource() {
    let mut snapshot = ResourceSnapshot::default();
    snapshot.insert("reachable.adoc", resource("reachable", "included\n"));
    let source = concat!(
        "ifdef::undefined[]\n",
        "include::unreachable.adoc[]\n",
        "endif::[]\n",
        "include::reachable.adoc[]\n",
    );
    let options = PreprocessOptions::default();
    let expected = preprocess(source, &snapshot, &options).expect("one-shot preprocessing");

    let (actual, requests, _) = deferred_preprocess(source, &snapshot, &options);

    assert_eq!(actual.expect("resumable preprocessing"), expected);
    assert_eq!(requests, ["reachable.adoc"]);
}

#[test]
fn optional_absence_is_authoritative_only_after_resume() {
    let snapshot = ResourceSnapshot::default();
    let source = "before\ninclude::missing.adoc[optional]\nafter\n";
    let options = PreprocessOptions::default();
    let expected = preprocess(source, &snapshot, &options).expect("one-shot preprocessing");

    let (actual, requests, _) = deferred_preprocess(source, &snapshot, &options);
    let actual = actual.expect("resumable preprocessing");

    assert_eq!(actual, expected);
    assert_eq!(requests, ["missing.adoc"]);
    assert_eq!(actual.notices.len(), 1);
    assert_eq!(
        actual.notices[0].kind,
        PreprocessNoticeKind::OptionalResourceMissing
    );
}

#[test]
fn repeated_resource_is_acquired_once_but_expanded_each_time() {
    let mut snapshot = ResourceSnapshot::default();
    snapshot.insert("part.adoc", resource("part", "included\n"));
    let source = "include::part.adoc[]\ninclude::part.adoc[]\n";
    let options = PreprocessOptions::default();
    let expected = preprocess(source, &snapshot, &options).expect("one-shot preprocessing");

    let (actual, requests, lookups) = deferred_preprocess(source, &snapshot, &options);

    assert_eq!(actual.expect("resumable preprocessing"), expected);
    assert_eq!(requests, ["part.adoc"]);
    assert_eq!(lookups, 1);
}

#[test]
fn cycle_and_include_limits_do_not_charge_again_after_resume() {
    let mut cycle_snapshot = ResourceSnapshot::default();
    cycle_snapshot.insert("part.adoc", resource("part", "include::part.adoc[]\n"));
    let cycle_source = "include::part.adoc[]\n";
    let cycle_options = PreprocessOptions::default();
    let expected_cycle =
        preprocess(cycle_source, &cycle_snapshot, &cycle_options).expect_err("cycle");

    let (actual_cycle, requests, _) =
        deferred_preprocess(cycle_source, &cycle_snapshot, &cycle_options);

    assert_eq!(actual_cycle.expect_err("resumable cycle"), expected_cycle);
    assert_eq!(requests, ["part.adoc"]);

    let source = "include::one.adoc[]\ninclude::two.adoc[]\ninclude::three.adoc[]\n";
    let snapshot = ["one", "two", "three"]
        .into_iter()
        .map(|name| (format!("{name}.adoc"), resource(name, "")))
        .collect::<ResourceSnapshot>();
    let accepted = PreprocessOptions {
        max_includes: 3,
        ..PreprocessOptions::default()
    };
    let expected = preprocess(source, &snapshot, &accepted).expect("exact include limit");
    let (actual, requests, _) = deferred_preprocess(source, &snapshot, &accepted);
    assert_eq!(actual.expect("resumable exact include limit"), expected);
    assert_eq!(requests.len(), 3);

    let rejected = PreprocessOptions {
        max_includes: 2,
        ..PreprocessOptions::default()
    };
    let expected = preprocess(source, &snapshot, &rejected).expect_err("include limit");
    let (actual, requests, _) = deferred_preprocess(source, &snapshot, &rejected);
    assert_eq!(actual.expect_err("resumable include limit"), expected);
    assert_eq!(requests.len(), 2);
}

#[test]
fn cumulative_node_byte_and_source_map_limits_survive_every_suspension() {
    let source = "include::one.adoc[]\ninclude::two.adoc[]\n";
    let snapshot = [
        ("one.adoc".to_owned(), resource("one", "a\n")),
        ("two.adoc".to_owned(), resource("two", "b\n")),
    ]
    .into_iter()
    .collect::<ResourceSnapshot>();

    for options in [
        PreprocessOptions {
            max_expanded_nodes: 4,
            ..PreprocessOptions::default()
        },
        PreprocessOptions {
            max_total_bytes: 4,
            ..PreprocessOptions::default()
        },
        PreprocessOptions {
            max_source_map_segments: 2,
            ..PreprocessOptions::default()
        },
    ] {
        let expected = preprocess(source, &snapshot, &options).expect("exact limit");
        let (actual, requests, _) = deferred_preprocess(source, &snapshot, &options);
        assert_eq!(actual.expect("resumable exact limit"), expected);
        assert_eq!(requests.len(), 2);
    }

    for options in [
        PreprocessOptions {
            max_expanded_nodes: 3,
            ..PreprocessOptions::default()
        },
        PreprocessOptions {
            max_total_bytes: 3,
            ..PreprocessOptions::default()
        },
        PreprocessOptions {
            max_source_map_segments: 1,
            ..PreprocessOptions::default()
        },
    ] {
        let expected = preprocess(source, &snapshot, &options).expect_err("limit exceeded");
        let (actual, requests, _) = deferred_preprocess(source, &snapshot, &options);
        assert_eq!(actual.expect_err("resumable limit exceeded"), expected);
        assert_eq!(requests.len(), 2);
    }
}

#[test]
fn cancellation_discards_a_suspended_run_without_exposing_partial_output() {
    let mut snapshot = ResourceSnapshot::default();
    snapshot.insert("part.adoc", resource("part", "included\n"));
    let lookup = DeferredLookup {
        snapshot: &snapshot,
        lookups: Cell::new(0),
    };
    let step = preprocess_resumable(
        "prefix\ninclude::part.adoc[]\n",
        &PreprocessOptions::default(),
        &lookup,
        &NeverCancel,
    );
    let PreprocessStep::NeedResource(suspended) = step else {
        panic!("preprocessing must suspend");
    };
    let cancellation = crate::core::CancellationToken::new();
    cancellation.cancel();
    let response = suspended.request().found(resource("part", "included\n"));

    assert!(matches!(
        suspended.resume(response, &lookup, &cancellation),
        PreprocessStep::Cancelled
    ));
}

#[test]
fn resumable_public_state_can_cross_a_worker_boundary() {
    fn assert_send<T: Send>() {}
    fn assert_send_sync<T: Send + Sync>() {}

    assert_send::<PreprocessStep>();
    assert_send::<SuspendedPreprocess>();
    assert_send::<EffectivePreprocessStep>();
    assert_send::<EffectiveSuspendedPreprocess>();
    assert_send::<PreparedPreprocessedDocument>();
    assert_send_sync::<ResourceRequest>();
    assert_send_sync::<ResourceResponse>();
    assert_send_sync::<ResourceLookupResult>();
}

#[test]
fn effective_resumption_prepares_for_the_same_analysis_contract() {
    let mut snapshot = ResourceSnapshot::default();
    snapshot.insert("part.adoc", resource("part.adoc", "included\n"));
    let lookup = DeferredLookup {
        snapshot: &snapshot,
        lookups: Cell::new(0),
    };
    let options = EffectiveProcessingOptions::new(
        crate::AnalysisOptions::default(),
        PreprocessOptions::default(),
    )
    .expect("effective options");
    let analyzer = options.clone();
    assert!(options.same_contract(&analyzer));
    let EffectivePreprocessStep::NeedResource(suspended) =
        options.preprocess_resumable("include::part.adoc[]\n", &lookup, &NeverCancel)
    else {
        panic!("preprocessing must suspend");
    };
    let response = suspended
        .request()
        .found(resource("part.adoc", "included\n"));
    let EffectivePreprocessStep::Complete(prepared) =
        suspended.resume(response, &lookup, &NeverCancel)
    else {
        panic!("preprocessing must complete");
    };

    let result = analyzer
        .analyze_preprocessed(prepared, PreprocessInputs::default())
        .expect("matching contract");

    assert_eq!(result.document.source, "included\n");
}

#[test]
fn prepared_document_rejects_a_different_effective_contract() {
    let first = EffectiveProcessingOptions::new(
        crate::AnalysisOptions::default(),
        PreprocessOptions::default(),
    )
    .expect("first contract");
    let second = EffectiveProcessingOptions::new(
        crate::AnalysisOptions::default(),
        PreprocessOptions::default(),
    )
    .expect("second contract");
    assert_eq!(first, second);
    assert!(!first.same_contract(&second));
    assert!(!first.same_contract(&first.clone().with_source_id(None)));
    let EffectivePreprocessStep::Complete(prepared) =
        first.preprocess_resumable("paragraph\n", &ResourceSnapshot::default(), &NeverCancel)
    else {
        panic!("preprocessing must complete");
    };

    assert!(matches!(
        second.analyze_preprocessed(prepared, PreprocessInputs::default()),
        Err(PreparedAnalysisError::ContractMismatch)
    ));
}

#[test]
fn preprocessing_cancels_at_a_bounded_line_checkpoint() {
    let cancellation = CancelAfter {
        checks: AtomicUsize::new(0),
        completed_checks: 2,
    };
    let source = "paragraph\n".repeat(CHECKPOINT_INTERVAL * 3);

    let failure = preprocess_with(
        &source,
        &ResourceSnapshot::default(),
        &PreprocessOptions::default(),
        PreprocessInputs {
            cancellation: Some(&cancellation),
        },
    )
    .expect_err("preprocessing should be cancelled");

    assert_eq!(failure, PreprocessFailure::Cancelled);
    assert_eq!(cancellation.checks.load(Ordering::Relaxed), 3);
}

#[test]
fn noncancellable_preprocessing_facade_preserves_output() {
    let source = "first\nsecond\n";
    let expected = preprocess(
        source,
        &ResourceSnapshot::default(),
        &PreprocessOptions::default(),
    )
    .expect("preprocess");
    let actual = preprocess_with(
        source,
        &ResourceSnapshot::default(),
        &PreprocessOptions::default(),
        PreprocessInputs::default(),
    )
    .expect("cancellable preprocess");

    assert_eq!(actual, expected);
}

#[test]
fn enormous_line_range_is_not_materialized() {
    let mut snapshot = ResourceSnapshot::default();
    snapshot.insert(
        "part.adoc",
        ResourceDocument {
            source_id: SourceId::new("part"),
            source: "first\nsecond\n".into(),
        },
    );

    let document = preprocess(
        "include::part.adoc[lines=1..18446744073709551615]\n",
        &snapshot,
        &PreprocessOptions::default(),
    )
    .expect("bounded line selection");

    assert_eq!(document.source, "first\nsecond\n");
}

#[test]
fn line_selection_parsing_remains_cancellable() {
    struct CancelAfterFirstCheckpoint(AtomicUsize);

    impl CancellationCheck for CancelAfterFirstCheckpoint {
        fn is_cancelled(&self) -> bool {
            self.0.fetch_add(1, Ordering::Relaxed) >= 1
        }
    }

    let value = (0..CHECKPOINT_INTERVAL * 3)
        .map(|line| line.to_string())
        .collect::<Vec<_>>()
        .join(",");
    let cancellation = CancelAfterFirstCheckpoint(AtomicUsize::new(0));

    assert_eq!(
        parse_line_selection(&value, &cancellation),
        Err(zero_range())
    );
    assert_eq!(cancellation.0.load(Ordering::Relaxed), 2);
}

#[test]
fn line_selection_normalizes_unordered_overlapping_and_boundary_ranges() {
    assert_eq!(
        parse_line_selection("5..8,1,2..4,7..10,12,12,11,20..19,not-a-line", &NeverCancel,)
            .expect("line selection"),
        LineSelection {
            ranges: vec![(1, 12)]
        }
    );

    let maximum = usize::MAX as u128;
    assert_eq!(
        parse_line_selection(
            &format!("{}..{},{}", maximum - 1, u128::MAX, maximum),
            &NeverCancel,
        )
        .expect("boundary line selection"),
        LineSelection {
            ranges: vec![(usize::MAX - 1, usize::MAX)]
        }
    );
    assert_eq!(
        parse_line_selection(&(maximum + 1).to_string(), &NeverCancel)
            .expect("out-of-range line selection"),
        LineSelection { ranges: Vec::new() }
    );
}

#[test]
fn combined_processing_classifies_preprocess_cancellation_separately() {
    let cancellation = crate::core::CancellationToken::new();
    cancellation.cancel();

    assert!(matches!(
        preprocess_and_analyze_with(
            &Engine::new(crate::core::AnalysisOptions::default()),
            "paragraph\n",
            &ResourceSnapshot::default(),
            &PreprocessOptions::default(),
            PreprocessInputs {
                cancellation: Some(&cancellation)
            }
        ),
        Err(PreprocessedAnalysisError::Cancelled)
    ));
}

#[test]
fn never_cancel_combined_processing_preserves_success_and_preprocess_errors() {
    let mut snapshot = ResourceSnapshot::default();
    snapshot.insert(
        "part.adoc",
        ResourceDocument {
            source_id: SourceId::new("part"),
            source: "included\n".into(),
        },
    );
    let engine = Engine::new(crate::core::AnalysisOptions::default());
    let options = PreprocessOptions::default();
    let expected = preprocess_and_analyze(&engine, "include::part.adoc[]\n", &snapshot, &options)
        .expect("compatibility analysis");
    let actual = preprocess_and_analyze_with(
        &engine,
        "include::part.adoc[]\n",
        &snapshot,
        &options,
        PreprocessInputs::default(),
    )
    .expect("cancellable analysis");

    assert_eq!(actual.document, expected.document);
    assert_eq!(
        actual.analysis.document().snapshot(),
        expected.analysis.document().snapshot()
    );
    assert_eq!(
        actual.analysis.diagnostics(),
        expected.analysis.diagnostics()
    );

    let expected_error =
        preprocess_and_analyze(&engine, "include::missing.adoc[]\n", &snapshot, &options)
            .expect_err("compatibility preprocessing error");
    let actual_error = preprocess_and_analyze_with(
        &engine,
        "include::missing.adoc[]\n",
        &snapshot,
        &options,
        PreprocessInputs::default(),
    )
    .expect_err("cancellable preprocessing error");
    assert_eq!(actual_error, expected_error);
}

#[test]
fn include_conditionals_filters_and_source_map_are_deterministic() {
    let mut snapshot = ResourceSnapshot::default();
    snapshot.insert(
        "part.adoc",
        ResourceDocument {
            source_id: SourceId::new("part"),
            source: "// tag::keep[]\n= Included\nline one\nline two\n// end::keep[]\n".into(),
        },
    );
    let mut options = PreprocessOptions {
        source_id: Some(SourceId::new("root")),
        ..PreprocessOptions::default()
    };
    options
        .attributes
        .insert("enabled".to_owned(), Some("".to_owned()));
    let source = "ifdef::enabled[]\ninclude::part.adoc[tag=keep,lines=2..3,leveloffset=+1,indent=2]\nendif::[]\n";
    let result = preprocess(source, &snapshot, &options).expect("preprocess");
    assert_eq!(result.source, "  == Included\n  line one\n");
    assert_eq!(result.directives.len(), 3);
    assert_eq!(result.source_map.len(), 2);
    assert_eq!(
        result.source_map[0]
            .origin
            .source_id
            .as_ref()
            .map(SourceId::as_str),
        Some("part")
    );
}

#[test]
fn include_indent_is_charged_before_the_padding_is_allocated() {
    let mut snapshot = ResourceSnapshot::default();
    snapshot.insert(
        "part.adoc",
        ResourceDocument {
            source_id: SourceId::new("part"),
            source: "one\ntwo\nthree\n".into(),
        },
    );
    let options = PreprocessOptions {
        source_id: Some(SourceId::new("root")),
        max_total_bytes: 1024,
        ..PreprocessOptions::default()
    };

    // Charging before the allocation reports the limit against the include
    // directive that requested the padding. Charging afterwards would report
    // it against a line of the included resource, after that line was built.
    let error = preprocess(
        "line\ninclude::part.adoc[indent=4096]\n",
        &snapshot,
        &options,
    )
    .expect_err("indent byte limit");
    assert_eq!(error.kind, PreprocessErrorKind::ByteLimit);
    assert_eq!(error.source_id.as_ref().map(SourceId::as_str), Some("root"));
    assert_eq!(error.range, range(5, 37));

    let document = preprocess(
        "include::part.adoc[indent=2]\n",
        &snapshot,
        &PreprocessOptions {
            source_id: Some(SourceId::new("root")),
            ..PreprocessOptions::default()
        },
    )
    .expect("indent within the budget");
    assert_eq!(document.source, "  one\n  two\n  three\n");
}

#[test]
fn include_indent_and_leveloffset_extremes_do_not_overflow() {
    let mut snapshot = ResourceSnapshot::default();
    snapshot.insert(
        "part.adoc",
        ResourceDocument {
            source_id: SourceId::new("part"),
            source: "    = Included\n    body\n".into(),
        },
    );

    let dedented = preprocess(
        "include::part.adoc[indent=-2147483648]\n",
        &snapshot,
        &PreprocessOptions::default(),
    )
    .expect("minimum indent");
    assert_eq!(dedented.source, "= Included\nbody\n");

    let raised = preprocess(
        "include::part.adoc[leveloffset=2147483647]\n",
        &snapshot,
        &PreprocessOptions::default(),
    )
    .expect("maximum leveloffset");
    assert_eq!(raised.source, "    = Included\n    body\n");

    let lowered = preprocess(
        "include::part.adoc[indent=-4,leveloffset=-2147483648]\n",
        &snapshot,
        &PreprocessOptions::default(),
    )
    .expect("minimum leveloffset");
    assert_eq!(lowered.source, "= Included\nbody\n");
}

#[test]
fn include_attributes_are_quote_aware_and_optional_missing_resources_are_notices() {
    let mut snapshot = ResourceSnapshot::default();
    snapshot.insert(
        "part.adoc",
        ResourceDocument {
            source_id: SourceId::new("part"),
            source: "// tag::one[]\none\n// end::one[]\n// tag::two[]\ntwo\n// end::two[]\n".into(),
        },
    );

    let document = preprocess(
        "include::part.adoc[tags=\"one,two\"]\ninclude::missing.adoc[optional]\n",
        &snapshot,
        &PreprocessOptions::default(),
    )
    .expect("preprocess");

    assert_eq!(document.source, "one\ntwo\n");
    assert_eq!(document.directives.len(), 2);
    assert_eq!(document.directives[1].resource_source_id, None);
    assert_eq!(document.notices.len(), 1);
    assert_eq!(
        document.notices[0].kind,
        PreprocessNoticeKind::OptionalResourceMissing
    );
    assert_eq!(document.notices[0].target, "missing.adoc");

    assert_eq!(
        preprocess(
            "include::missing.adoc[optional,encoding=shift_jis]\n",
            &ResourceSnapshot::default(),
            &PreprocessOptions::default(),
        )
        .expect_err("optional must not suppress encoding failures")
        .kind,
        PreprocessErrorKind::UnsupportedEncoding
    );
    assert_eq!(
        preprocess(
            "include::../missing.adoc[optional]\n",
            &ResourceSnapshot::default(),
            &PreprocessOptions::default(),
        )
        .expect_err("optional must not suppress unsafe target failures")
        .kind,
        PreprocessErrorKind::UnsafeTarget
    );
}

#[test]
fn cycles_limits_unsafe_targets_and_encoding_fail_before_parsing() {
    let mut snapshot = ResourceSnapshot::default();
    snapshot.insert(
        "cycle.adoc",
        ResourceDocument {
            source_id: SourceId::new("cycle"),
            source: "include::cycle.adoc[]\n".into(),
        },
    );
    assert_eq!(
        preprocess(
            "include::cycle.adoc[]\n",
            &snapshot,
            &PreprocessOptions::default()
        )
        .expect_err("cycle")
        .kind,
        PreprocessErrorKind::IncludeCycle
    );
    assert_eq!(
        preprocess(
            "include::../outside.adoc[]\n",
            &snapshot,
            &PreprocessOptions::default()
        )
        .expect_err("unsafe")
        .kind,
        PreprocessErrorKind::UnsafeTarget
    );
    assert_eq!(
        preprocess(
            "include::cycle.adoc[encoding=shift_jis]\n",
            &snapshot,
            &PreprocessOptions::default()
        )
        .expect_err("encoding")
        .kind,
        PreprocessErrorKind::UnsupportedEncoding
    );
}

#[test]
fn inline_and_expression_conditionals_follow_attribute_semantics() {
    let mut options = PreprocessOptions::default();
    options
        .attributes
        .insert("edition".to_owned(), Some("2".to_owned()));
    options
        .attributes
        .insert("web".to_owned(), Some(String::new()));
    let source = concat!(
        "ifdef::web[inline]\n",
        "ifndef::print[also inline]\n",
        "ifeval::[{edition} >= 2]\n",
        "selected\n",
        "endif::[]\n",
        "\\include::literal.adoc[]\n",
    );
    let result = preprocess(source, &ResourceSnapshot::default(), &options).expect("result");
    assert_eq!(
        result.source,
        "inline\nalso inline\nselected\ninclude::literal.adoc[]\n"
    );
}

#[test]
fn document_attributes_drive_includes_and_conditionals_in_read_order() {
    let mut snapshot = ResourceSnapshot::default();
    for (target, source_id, source) in [
        (
            "first.adoc",
            "first",
            include_str!("../../../../fixtures/attributes/preprocessor-first.adoc"),
        ),
        (
            "second.adoc",
            "second",
            include_str!("../../../../fixtures/attributes/preprocessor-second.adoc"),
        ),
        (
            "safe.adoc",
            "safe",
            include_str!("../../../../fixtures/attributes/preprocessor-safe.adoc"),
        ),
        ("bad.adoc", "bad", "bad resource\n"),
    ] {
        snapshot.insert(
            target,
            ResourceDocument {
                source_id: SourceId::new(source_id),
                source: source.into(),
            },
        );
    }
    let source = include_str!("../../../../fixtures/attributes/preprocessor-read-order.adoc");

    let result = preprocess(source, &snapshot, &PreprocessOptions::default()).expect("preprocess");

    assert!(result.source.contains("second resource"));
    assert!(result.source.contains("included attribute is visible"));
    assert!(result.source.contains("safe resource"));
    assert!(result.source.contains("unset is visible"));
    assert!(!result.source.contains("bad resource"));
    assert_eq!(
        result
            .directives
            .iter()
            .filter(|directive| directive.kind == DirectiveKind::Include)
            .map(|directive| directive.target.as_str())
            .collect::<Vec<_>>(),
        ["first.adoc", "second.adoc", "safe.adoc"]
    );
}

#[test]
fn multiline_locked_failed_and_delimited_definitions_follow_shared_rules() {
    let mut snapshot = ResourceSnapshot::default();
    for target in ["host.adoc", "folded- value.adoc"] {
        snapshot.insert(
            target,
            ResourceDocument {
                source_id: SourceId::new(target),
                source: format!("{target}\n").into(),
            },
        );
    }
    let source = "\
:locked: document
:part: folded- \\
 value
:literal: retained \\
include::missing.adoc[]
include::{locked}.adoc[]
include::{part}.adoc[]

:cycle: {cycle}
ifdef::cycle[]
cycle must stay hidden
endif::[]
----
:inside: visible
----
ifdef::inside[]
delimited attribute must stay hidden
endif::[]

:cycle: recovered
ifdef::cycle[]
recovered definition is visible
endif::[]
";
    let options = PreprocessOptions {
        attributes: BTreeMap::from([("locked".to_owned(), Some("host".to_owned()))]),
        ..PreprocessOptions::default()
    };

    let result = preprocess(source, &snapshot, &options).expect("preprocess");

    assert!(result.source.contains("host.adoc"));
    assert!(result.source.contains("folded- value.adoc"));
    assert!(!result.source.contains("cycle must stay hidden"));
    assert!(
        !result
            .source
            .contains("delimited attribute must stay hidden")
    );
    assert!(result.source.contains("recovered definition is visible"));
    assert_eq!(
        result
            .directives
            .iter()
            .filter(|directive| directive.kind == DirectiveKind::Include)
            .map(|directive| directive.target.as_str())
            .collect::<Vec<_>>(),
        ["host.adoc", "folded- value.adoc"]
    );

    let mut analysis_options = crate::AnalysisOptions::default();
    analysis_options.attributes.clone_from(&options.attributes);
    let analyzed =
        preprocess_and_analyze(&Engine::new(analysis_options), source, &snapshot, &options)
            .expect("preprocessed analysis");
    let locked = analyzed
        .analysis
        .attribute_environment()
        .resolve_at(
            "locked",
            TextSize::new(analyzed.document.source.len()).expect("offset"),
        )
        .expect("locked attribute");
    assert_eq!(locked.value, Ok(Some("host")));
    assert_eq!(locked.binding, None);
}

#[test]
fn base_uri_resolves_snapshot_keys_without_io() {
    let mut snapshot = ResourceSnapshot::default();
    snapshot.insert(
        "chapters/one.adoc",
        ResourceDocument {
            source_id: SourceId::new("one"),
            source: "chapter\n".into(),
        },
    );
    let options = PreprocessOptions {
        base_uri: Some("chapters".to_owned()),
        ..PreprocessOptions::default()
    };
    let result = preprocess("include::one.adoc[]\n", &snapshot, &options).expect("result");
    assert_eq!(result.source, "chapter\n");
}

#[test]
fn uri_base_preserves_snapshot_key_spelling() {
    assert_eq!(
        resolve_include_target("part.adoc", Some("file:///book")),
        "file:///book/part.adoc"
    );
}

#[test]
fn nested_includes_resolve_from_the_including_resource() {
    let mut snapshot = ResourceSnapshot::default();
    snapshot.insert(
        "book/chapters/one.adoc",
        ResourceDocument {
            source_id: SourceId::new("one"),
            source: "include::section.adoc[]\n".into(),
        },
    );
    snapshot.insert(
        "book/chapters/section.adoc",
        ResourceDocument {
            source_id: SourceId::new("section"),
            source: "nested\n".into(),
        },
    );
    let options = PreprocessOptions {
        base_uri: Some("book/chapters".to_owned()),
        ..PreprocessOptions::default()
    };

    let result = preprocess("include::one.adoc[]\n", &snapshot, &options).expect("result");
    assert_eq!(result.source, "nested\n");
    assert_eq!(result.directives[1].target, "book/chapters/section.adoc");
}

#[test]
fn include_discovery_is_io_free_and_ignores_escaped_or_incomplete_directives() {
    let requests = discover_includes(concat!(
        "include::one.adoc[tag=a]\n",
        "\\include::literal.adoc[]\n",
        "include::incomplete.adoc[\n",
        "ifdef::web[]\ninclude::conditional.adoc[]\nendif::[]\n",
    ))
    .expect("bounded source");

    assert_eq!(requests.len(), 2);
    assert_eq!(requests[0].target, "one.adoc");
    assert_eq!(requests[0].attributes, "tag=a");
    assert_eq!(requests[1].target, "conditional.adoc");
}

#[test]
fn range_projection_preserves_identity_and_marks_transforms_conservatively() {
    let document = PreprocessedDocument::from_parts(
        "abcXYZ".to_owned(),
        vec![
            SourceMapSegment {
                output_range: ExpandedRange::new(range(0, 3)),
                origin: SourceOrigin {
                    source_id: Some(SourceId::new("root")),
                    range: OriginRange::new(range(10, 13)),
                },
                mapping: SourceMapping::Identity,
            },
            SourceMapSegment {
                output_range: ExpandedRange::new(range(3, 6)),
                origin: SourceOrigin {
                    source_id: Some(SourceId::new("included")),
                    range: OriginRange::new(range(20, 28)),
                },
                mapping: SourceMapping::WholeOrigin,
            },
        ],
        Vec::new(),
        Vec::new(),
    )
    .expect("valid source map");

    assert_eq!(
        document.origins_for_range(ExpandedRange::new(range(1, 5))),
        vec![
            SourceOrigin {
                source_id: Some(SourceId::new("root")),
                range: OriginRange::new(range(11, 13)),
            },
            SourceOrigin {
                source_id: Some(SourceId::new("included")),
                range: OriginRange::new(range(20, 28)),
            },
        ]
    );
    assert_eq!(
        document.origins_for_range(ExpandedRange::new(range(2, 2))),
        vec![SourceOrigin {
            source_id: Some(SourceId::new("root")),
            range: OriginRange::new(range(12, 12)),
        }]
    );
    assert_eq!(
        document.origins_for_range(ExpandedRange::new(range(3, 3))),
        vec![SourceOrigin {
            source_id: Some(SourceId::new("included")),
            range: OriginRange::new(range(20, 28)),
        }]
    );
}

#[test]
fn analysis_projection_maps_reference_resource_and_symbol_targets() {
    let mut snapshot = ResourceSnapshot::default();
    snapshot.insert(
        "part.adoc",
        ResourceDocument {
            source_id: SourceId::new("part"),
            source: "== Included\nSee xref:other.adoc#target[] and image::cover.png[].\n".into(),
        },
    );
    let engine = Engine::new(crate::core::AnalysisOptions::default());
    let analysis = preprocess_and_analyze(
        &engine,
        "include::part.adoc[]\n",
        &snapshot,
        &PreprocessOptions {
            source_id: Some(SourceId::new("root")),
            ..PreprocessOptions::default()
        },
    )
    .expect("analysis");
    let projection = analysis
        .project_origins(ProjectionLimits::default())
        .expect("projection");

    assert_eq!(projection.symbols.len(), 1);
    assert_eq!(projection.references.len(), 1);
    assert_eq!(projection.resources.len(), 1);
    assert_eq!(
        projection.references[0].target_origins[0]
            .source_id
            .as_ref()
            .map(SourceId::as_str),
        Some("part")
    );
    assert_eq!(
        projection.resources[0].target_origins[0]
            .source_id
            .as_ref()
            .map(SourceId::as_str),
        Some("part")
    );
}

#[test]
fn analysis_projection_maps_included_body_attribute_occurrences() {
    let included = include_str!("../../../../fixtures/attributes/body-set-unset.adoc");
    let mut snapshot = ResourceSnapshot::default();
    snapshot.insert(
        "attributes.adoc",
        ResourceDocument {
            source_id: SourceId::new("included-attributes"),
            source: included.into(),
        },
    );
    let engine = Engine::new(crate::core::AnalysisOptions::default());
    let analysis = preprocess_and_analyze(
        &engine,
        "include::attributes.adoc[]\n",
        &snapshot,
        &PreprocessOptions {
            source_id: Some(SourceId::new("root")),
            ..PreprocessOptions::default()
        },
    )
    .expect("analysis");
    let projection = analysis
        .project_origins(ProjectionLimits::default())
        .expect("projection");

    assert_eq!(projection.attribute_occurrences.len(), 2);
    for attribute in &projection.attribute_occurrences {
        assert_eq!(
            attribute.origins[0]
                .source_id
                .as_ref()
                .map(SourceId::as_str),
            Some("included-attributes")
        );
        assert_eq!(
            attribute.name_origins[0]
                .source_id
                .as_ref()
                .map(SourceId::as_str),
            Some("included-attributes")
        );
        assert_eq!(
            attribute.value_origins[0]
                .source_id
                .as_ref()
                .map(SourceId::as_str),
            Some("included-attributes")
        );
    }
    let theme = &projection.attribute_occurrences[0];
    assert_eq!(
        theme.origins[0].range.text_range(),
        text_range_in(included, ":theme: dark\n")
    );
    assert_eq!(
        theme.name_origins[0].range.text_range(),
        text_range_in(included, "theme")
    );
    assert_eq!(
        theme.value_origins[0].range.text_range(),
        text_range_in(included, "dark")
    );
}

#[test]
fn analysis_projection_connects_attribute_references_to_included_bindings() {
    let included = ":shared: included\n";
    let root = "include::attributes.adoc[]\n\n{shared}\n";
    let mut snapshot = ResourceSnapshot::default();
    snapshot.insert(
        "attributes.adoc",
        ResourceDocument {
            source_id: SourceId::new("included-attributes"),
            source: included.into(),
        },
    );
    let analysis = preprocess_and_analyze(
        &Engine::new(crate::core::AnalysisOptions::default()),
        root,
        &snapshot,
        &PreprocessOptions {
            source_id: Some(SourceId::new("root")),
            ..PreprocessOptions::default()
        },
    )
    .expect("analysis");
    let projection = analysis
        .project_origins(ProjectionLimits::default())
        .expect("projection");

    assert_eq!(projection.attribute_bindings.len(), 1);
    assert_eq!(projection.attribute_references.len(), 1);
    let binding = &projection.attribute_bindings[0];
    let reference = &projection.attribute_references[0];
    assert_eq!(reference.value.binding_id, Some(binding.value.id()));
    assert_eq!(reference.value.value, Ok(Some("included".to_owned())));
    assert_eq!(
        binding.name_origins[0]
            .source_id
            .as_ref()
            .map(SourceId::as_str),
        Some("included-attributes")
    );
    assert_eq!(
        reference.name_origins[0]
            .source_id
            .as_ref()
            .map(SourceId::as_str),
        Some("root")
    );
    assert_eq!(
        binding.name_origins[0].range.text_range(),
        text_range_in(included, "shared")
    );
    assert_eq!(
        reference.name_origins[0].range.text_range(),
        text_range_in(root, "shared")
    );
}

#[test]
fn analysis_projection_preserves_each_included_attribute_value_line() {
    let included = include_str!("../../../../fixtures/attributes/multiline-soft-hard.adoc");
    let mut snapshot = ResourceSnapshot::default();
    snapshot.insert(
        "multiline.adoc",
        ResourceDocument {
            source_id: SourceId::new("included-multiline"),
            source: included.into(),
        },
    );
    let engine = Engine::new(crate::core::AnalysisOptions::default());
    let analysis = preprocess_and_analyze(
        &engine,
        "include::multiline.adoc[]\n",
        &snapshot,
        &PreprocessOptions {
            source_id: Some(SourceId::new("root")),
            ..PreprocessOptions::default()
        },
    )
    .expect("analysis");
    let projection = analysis
        .project_origins(ProjectionLimits::default())
        .expect("projection");

    let soft = &projection.attribute_occurrences[0];
    assert_eq!(
        soft.value.value.folded_text,
        "first line 日本語🙂 third line"
    );
    assert_eq!(soft.value_lines.len(), 3);
    for line in &soft.value_lines {
        for origins in [&line.origins, &line.content_origins, &line.ending_origins] {
            assert_eq!(origins.len(), 1);
            assert_eq!(
                origins[0].source_id.as_ref().map(SourceId::as_str),
                Some("included-multiline")
            );
        }
    }
    assert_eq!(
        soft.value_lines
            .iter()
            .map(|line| {
                let range = line.content_origins[0].range.text_range();
                &included[range.start().to_usize()..range.end().to_usize()]
            })
            .collect::<Vec<_>>(),
        ["first line", "日本語🙂", "third line"]
    );
}

#[test]
fn empty_attribute_value_at_an_include_boundary_projects_to_the_include() {
    let included = include_str!("../../../../fixtures/attributes/include-empty-no-newline.adoc");
    assert!(!included.ends_with('\n'));
    let mut snapshot = ResourceSnapshot::default();
    snapshot.insert(
        "empty.adoc",
        ResourceDocument {
            source_id: SourceId::new("empty-include"),
            source: included.into(),
        },
    );
    let analysis = preprocess_and_analyze(
        &Engine::new(crate::core::AnalysisOptions::default()),
        "include::empty.adoc[]\n\nBody\n",
        &snapshot,
        &PreprocessOptions {
            source_id: Some(SourceId::new("root")),
            ..PreprocessOptions::default()
        },
    )
    .expect("analysis");
    let projection = analysis
        .project_origins(ProjectionLimits::default())
        .expect("projection");

    assert_eq!(projection.attribute_occurrences.len(), 1);
    let attribute = &projection.attribute_occurrences[0];
    assert!(attribute.value.value.source_range.is_empty());
    assert_eq!(
        attribute.value_origins,
        vec![SourceOrigin {
            source_id: Some(SourceId::new("empty-include")),
            range: OriginRange::new(range(included.len(), included.len())),
        }]
    );
    assert_eq!(
        attribute.origins.len(),
        2,
        "the line ending originates in the root segment"
    );
}

fn text_range_in(source: &str, needle: &str) -> TextRange {
    let start = source.find(needle).expect("fixture contains needle");
    range(start, start + needle.len())
}

#[test]
fn source_map_and_projection_limits_fail_explicitly() {
    let source_map_error = preprocess(
        "one\ntwo\n",
        &ResourceSnapshot::default(),
        &PreprocessOptions {
            max_source_map_segments: 1,
            ..PreprocessOptions::default()
        },
    )
    .expect_err("source map limit");
    assert_eq!(source_map_error.kind, PreprocessErrorKind::SourceMapLimit);

    let engine = Engine::new(crate::core::AnalysisOptions::default());
    let analysis = preprocess_and_analyze(
        &engine,
        "= Title\n\n== Section\n",
        &ResourceSnapshot::default(),
        &PreprocessOptions::default(),
    )
    .expect("analysis");
    let error = analysis
        .project_origins(ProjectionLimits {
            max_origin_segments: 1,
        })
        .expect_err("projection limit");
    assert_eq!(error.limit, 1);
    assert!(error.actual > 1);
}

#[test]
fn source_map_constructor_rejects_unsorted_overlap_and_out_of_bounds_segments() {
    let segment = |start, end| SourceMapSegment {
        output_range: ExpandedRange::new(range(start, end)),
        origin: SourceOrigin {
            source_id: None,
            range: OriginRange::new(range(start, end)),
        },
        mapping: SourceMapping::Identity,
    };
    assert!(
        PreprocessedDocument::from_parts(
            "abcd".to_owned(),
            vec![segment(2, 4), segment(1, 2)],
            Vec::new(),
            Vec::new(),
        )
        .is_err()
    );
    assert!(
        PreprocessedDocument::from_parts(
            "abcd".to_owned(),
            vec![segment(0, 3), segment(2, 4)],
            Vec::new(),
            Vec::new(),
        )
        .is_err()
    );
    assert!(
        PreprocessedDocument::from_parts(
            "abcd".to_owned(),
            vec![segment(0, 5)],
            Vec::new(),
            Vec::new(),
        )
        .is_err()
    );
}

#[test]
fn disabled_include_capability_preserves_syntax_without_resolving() {
    let source = "include::missing.adoc[]\n";
    let document = preprocess(
        source,
        &ResourceSnapshot::default(),
        &PreprocessOptions {
            enable_includes: false,
            ..PreprocessOptions::default()
        },
    )
    .expect("disabled include does not require a resource");

    assert_eq!(document.source, source);
    assert_eq!(document.directives.len(), 1);
    assert_eq!(document.directives[0].kind, DirectiveKind::Include);
    assert!(document.directives[0].resource_source_id.is_none());
}
