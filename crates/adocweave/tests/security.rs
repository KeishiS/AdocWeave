use std::sync::atomic::{AtomicUsize, Ordering};

use adocweave::html::{RenderPolicy, ResolvedReference, render, render_with_resolutions};
use adocweave::limits::{ProcessConfig, ProcessingLimits, SyntaxMode};
use adocweave::reference::{ReferenceKey, ResolutionFailureKind};
use adocweave::{
    CancellationCheck, CheckOutput, Engine, Operation, ParseError, ParseOptions, ProcessError,
    process_check_with_config, process_with_config,
};

type LimitCase = (&'static str, fn(&mut ProcessingLimits));
type BoundaryCase = (
    &'static str,
    &'static str,
    u32,
    fn(&mut ProcessingLimits, u32),
);

#[test]
fn adversarial_fixture_never_emits_active_input_or_unsafe_urls() {
    let source = include_str!("../../../fixtures/security/adversarial.adoc");
    let analysis = Engine::new(ParseOptions::default())
        .analyze(source)
        .expect("adversarial fixture remains bounded");
    let output = render(analysis.ast(), &RenderPolicy::default());
    let lower = output.html.to_ascii_lowercase();

    assert!(!lower.contains("<script"));
    assert!(!lower.contains("<img"));
    assert!(!lower.contains("href=\"javascript:"));
    assert!(!lower.contains("href=\"data:"));
    assert!(output.html.contains("&lt;script&gt;"));
    assert!(output.html.contains("href=\"https://example.com/path\""));
}

#[test]
fn hostile_resolver_href_is_revalidated_by_the_renderer() {
    let source = "xref:note:item[unsafe]";
    let analysis = Engine::new(ParseOptions::default())
        .analyze(source)
        .expect("analysis");
    let range = analysis.references()[0].range;
    let output = render_with_resolutions(
        analysis.ast(),
        &RenderPolicy::default(),
        &[ResolvedReference::resolved(range, "javascript:alert(1)")],
    );

    assert_eq!(output.html, "<p>unsafe</p>\n");
    assert!(
        output
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code.as_str() == "invalid-url-scheme")
    );
}

#[test]
fn malformed_bytes_and_tight_limits_fail_without_partial_output() {
    let invalid_utf8 = [0xf0, 0x28, 0x8c, 0x28];
    for operation in [
        Operation::Convert,
        Operation::Check,
        Operation::Format,
        Operation::Symbols,
    ] {
        assert!(matches!(
            process_with_config(operation, &invalid_utf8, &ProcessConfig::default()),
            Err(ProcessError::InvalidUtf8 { .. })
        ));
    }

    let config = ProcessConfig {
        limits: ProcessingLimits {
            max_input_bytes: 32,
            max_output_bytes: 8,
            max_line_bytes: 8,
            max_list_depth: 2,
            max_inline_depth: 2,
            max_formula_bytes: 4,
            max_blocks: 2,
            max_nodes: 4,
            max_references: 1,
            max_attributes: 1,
            max_diagnostics: 1,
        },
        syntax_mode: SyntaxMode::Permissive,
    };
    for input in [
        b"a very long line".as_slice(),
        b"one\n\ntwo\n\nthree".as_slice(),
        b"https://example.com[x] https://example.com[y]".as_slice(),
    ] {
        assert!(matches!(
            process_with_config(Operation::Convert, input, &config),
            Err(ProcessError::LimitExceeded { .. })
        ));
        assert!(matches!(
            process_check_with_config(input, CheckOutput::Json, &config),
            Err(ProcessError::LimitExceeded { .. })
        ));
    }
}

#[test]
fn each_structural_resource_limit_rejects_the_corresponding_input() {
    let cases: [LimitCase; 3] = [
        ("one\n\ntwo\n", |limits: &mut ProcessingLimits| {
            limits.max_blocks = 1
        }),
        (
            "xref:note:a[] xref:note:b[]",
            |limits: &mut ProcessingLimits| {
                limits.max_references = 1;
            },
        ),
        (
            "= Title\n:one: 1\n:two: 2\n",
            |limits: &mut ProcessingLimits| limits.max_attributes = 1,
        ),
    ];

    for (source, restrict) in cases {
        let mut limits = ProcessingLimits::default();
        restrict(&mut limits);
        let result = process_with_config(
            Operation::Convert,
            source.as_bytes(),
            &ProcessConfig {
                limits,
                syntax_mode: SyntaxMode::Permissive,
            },
        );
        assert!(
            matches!(result, Err(ProcessError::LimitExceeded { .. })),
            "{source:?}"
        );
    }
}

#[test]
fn construction_budgets_accept_exact_boundaries_and_reject_the_next_item() {
    let cases: [BoundaryCase; 4] = [
        (
            "blocks",
            "one\n\ntwo",
            2_u32,
            |limits: &mut ProcessingLimits, value| {
                limits.max_blocks = value;
            },
        ),
        (
            "nodes",
            "plain",
            3_u32,
            |limits: &mut ProcessingLimits, value| {
                limits.max_nodes = value;
            },
        ),
        (
            "references",
            "xref:#a[] xref:#b[]",
            2_u32,
            |limits: &mut ProcessingLimits, value| {
                limits.max_references = value;
            },
        ),
        (
            "document attributes",
            "= T\n:a: 1\n:b: 2\n",
            2_u32,
            |limits: &mut ProcessingLimits, value| {
                limits.max_attributes = value;
            },
        ),
    ];

    for (resource, source, exact, set_limit) in cases {
        let mut accepted = ParseOptions::default();
        set_limit(&mut accepted.limits, exact);
        Engine::new(accepted)
            .analyze(source)
            .unwrap_or_else(|error| panic!("{resource} exact boundary failed: {error}"));

        let mut rejected = ParseOptions::default();
        set_limit(&mut rejected.limits, exact - 1);
        match Engine::new(rejected).analyze(source) {
            Err(ParseError::LimitExceeded {
                resource: actual_resource,
                limit,
                actual,
            }) => {
                assert_eq!(actual_resource, resource);
                assert_eq!(limit, exact - 1);
                assert_eq!(actual, u64::from(exact));
            }
            other => panic!("{resource} over-boundary result was {other:?}"),
        }
    }
}

#[test]
fn formula_limit_recovers_as_text_and_reports_a_diagnostic() {
    let limits = ProcessingLimits {
        max_formula_bytes: 4,
        ..ProcessingLimits::default()
    };
    let config = ProcessConfig {
        limits,
        syntax_mode: SyntaxMode::Permissive,
    };
    let source = b"stem:[12345<script>]";

    let html = process_with_config(Operation::Convert, source, &config)
        .expect("formula overflow is recoverable");
    let diagnostics = process_check_with_config(source, CheckOutput::Json, &config)
        .expect("formula overflow diagnostics");

    assert!(!html.contains("<script>"));
    assert!(html.contains("&lt;script&gt;"));
    assert!(diagnostics.contains("invalid-stem"));
    assert!(diagnostics.contains("size limit"));
}

#[test]
fn list_depth_limit_recovers_with_a_diagnostic() {
    let limits = ProcessingLimits {
        max_list_depth: 2,
        ..ProcessingLimits::default()
    };
    let config = ProcessConfig {
        limits,
        syntax_mode: SyntaxMode::Permissive,
    };
    let source = b"* one\n** two\n*** three\n";

    let html = process_with_config(Operation::Convert, source, &config)
        .expect("list depth overflow is recoverable");
    let diagnostics = process_check_with_config(source, CheckOutput::Json, &config)
        .expect("list depth diagnostics");

    assert!(html.contains("three"));
    assert!(diagnostics.contains("configured limit"));
}

#[test]
fn cooperative_cancellation_returns_no_analysis_to_render() {
    struct CancelAfter {
        checks: AtomicUsize,
        threshold: usize,
    }
    impl CancellationCheck for CancelAfter {
        fn is_cancelled(&self) -> bool {
            self.checks.fetch_add(1, Ordering::Relaxed) >= self.threshold
        }
    }

    let source = "paragraph\n\n".repeat(10_000);
    let result = Engine::new(ParseOptions::default()).analyze_cancellable(
        &source,
        &CancelAfter {
            checks: AtomicUsize::new(0),
            threshold: 1,
        },
    );
    assert!(matches!(result, Err(ParseError::Cancelled)));
}

#[test]
fn reference_failure_codes_remain_total_for_host_failures() {
    let cases = [
        (
            ResolutionFailureKind::MissingTarget,
            "missing-reference-target",
        ),
        (
            ResolutionFailureKind::MissingAnchor,
            "missing-reference-anchor",
        ),
        (
            ResolutionFailureKind::AmbiguousTarget,
            "ambiguous-reference-target",
        ),
        (ResolutionFailureKind::OutsideRoot, "reference-outside-root"),
        (
            ResolutionFailureKind::ResolverFailure,
            "reference-resolver-failure",
        ),
    ];
    for (kind, code) in cases {
        assert_eq!(kind.diagnostic_code(), code);
    }

    let outside = ReferenceKey::Document {
        document: "../outside.adoc".to_owned(),
        anchor: None,
    };
    assert!(matches!(outside, ReferenceKey::Document { .. }));
}
