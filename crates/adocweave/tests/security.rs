use std::sync::atomic::{AtomicUsize, Ordering};

use adocweave::ProcessingLimits;
use adocweave::output::html::{RenderPolicy, render, render_with_inputs};
use adocweave::resolution::RenderInputs;
use adocweave::resolution::ResolvedReference;
use adocweave::resolution::ResolvedResource;
use adocweave::resolution::{ReferenceKey, ResolutionFailureKind};
use adocweave::{Analysis, CancellationCheck, Engine, ParseError, ParseOptions};

type LimitCase = (&'static str, fn(&mut ProcessingLimits));
type BoundaryCase = (
    &'static str,
    &'static str,
    u32,
    fn(&mut ProcessingLimits, u32),
);

fn analyze_with_limits(source: &str, limits: ProcessingLimits) -> Result<Analysis, ParseError> {
    Engine::new(ParseOptions {
        limits,
        ..ParseOptions::default()
    })
    .analyze(source)
}

#[test]
fn adversarial_fixture_never_emits_active_input_or_unsafe_urls() {
    let source = include_str!("../../../fixtures/security/adversarial.adoc");
    let analysis = Engine::new(ParseOptions::default())
        .analyze(source)
        .expect("adversarial fixture remains bounded");
    let output = render(analysis.document(), &RenderPolicy::default());
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
    let output = render_with_inputs(
        analysis.document(),
        &RenderPolicy::default(),
        &RenderInputs::new(
            vec![ResolvedReference::resolved(range, "javascript:alert(1)")],
            vec![],
        ),
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
fn hostile_resource_href_is_revalidated_by_the_renderer() {
    let analysis = Engine::new(ParseOptions::default())
        .analyze("image:asset.png[safe]")
        .expect("analysis");
    let range = analysis.resources()[0].range;
    let output = render_with_inputs(
        analysis.document(),
        &RenderPolicy::default(),
        &RenderInputs::new(
            vec![],
            vec![ResolvedResource::resolved(
                range,
                "javascript:alert(1)",
                Some("image/png".to_owned()),
                Some(42),
            )],
        ),
    );

    assert_eq!(output.html, "<p>safe</p>\n");
    assert!(!output.html.contains("<img"));
    assert!(
        output
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code.as_str() == "invalid-url-scheme")
    );
}

#[test]
fn hostile_stylesheet_configuration_never_reaches_the_output() {
    use adocweave::output::html::{HtmlDocumentMode, StylesheetPolicy, StylesheetSource};

    let analysis = Engine::new(ParseOptions::default())
        .analyze("paragraph")
        .expect("analysis");
    let output = render(
        analysis.document(),
        &RenderPolicy {
            document_mode: HtmlDocumentMode::Complete,
            stylesheets: StylesheetPolicy {
                sources: vec![
                    StylesheetSource::Inline("p {}</StYlE><script>alert(1)</script>".to_owned()),
                    StylesheetSource::External("javascript:alert(1)".to_owned()),
                    StylesheetSource::External("https://ok.example/x.css\"onload=\"x".to_owned()),
                ],
                ..StylesheetPolicy::default()
            },
            ..RenderPolicy::default()
        },
    );

    let lower = output.html.to_ascii_lowercase();
    assert!(!lower.contains("<script"));
    assert!(!lower.contains("<style"));
    assert!(!lower.contains("javascript:"));
    assert!(!lower.contains("onload"));
    let codes = output
        .diagnostics
        .iter()
        .map(|diagnostic| diagnostic.code.as_str())
        .collect::<Vec<_>>();
    assert!(codes.contains(&"invalid-stylesheet-content"));
    assert!(codes.contains(&"invalid-stylesheet-url"));
}

#[test]
fn heading_anchor_cannot_break_out_of_the_id_attribute() {
    let source = "[[x\"onclick=\"alert(1)]]\n== Target\n";
    let analysis = Engine::new(ParseOptions::default())
        .analyze(source)
        .expect("analysis");
    let output = render(analysis.document(), &RenderPolicy::default());

    // The dangerous anchor never reaches the output as raw attribute syntax:
    // neither an attribute breakout nor an unescaped quote survives.
    assert!(!output.html.contains("onclick="));
    assert!(!output.html.contains("\"onclick"));
    // A heading is still emitted, using a safe generated id.
    assert!(output.html.contains("<h1 id=\"_target\">Target</h1>"));
    // The unsafe anchor is rejected with a diagnostic rather than trusted.
    assert!(
        analysis
            .diagnostics()
            .iter()
            .any(|diagnostic| diagnostic.code.as_str() == "invalid-anchor")
    );
}

#[test]
fn tight_limits_fail_without_partial_analysis() {
    let limits = ProcessingLimits {
        max_input_bytes: 32,
        max_output_bytes: 8,
        max_line_bytes: 8,
        max_list_depth: 2,
        max_list_continuations: 1,
        max_block_depth: 2,
        max_inline_depth: 2,
        max_formula_bytes: 4,
        max_table_bytes: 8,
        max_table_cells: 2,
        max_table_columns: 2,
        max_table_depth: 1,
        max_catalog_entries: 2,
        max_catalog_bytes: 8,
        max_blocks: 2,
        max_nodes: 4,
        max_references: 1,
        max_attributes: 1,
        max_attribute_expansion_depth: 1,
        max_attribute_expansion_bytes: 8,
        max_diagnostics: 1,
    };
    for source in [
        "a very long line",
        "\
one

two

three",
        "https://example.com[x] https://example.com[y]",
    ] {
        assert!(matches!(
            analyze_with_limits(source, limits),
            Err(ParseError::LimitExceeded { .. })
        ));
    }
}

#[test]
fn each_structural_resource_limit_rejects_the_corresponding_input() {
    let cases: [LimitCase; 3] = [
        (
            "\
one

two
",
            |limits: &mut ProcessingLimits| limits.max_blocks = 1,
        ),
        (
            "xref:note:a[] xref:note:b[]",
            |limits: &mut ProcessingLimits| {
                limits.max_references = 1;
            },
        ),
        (
            "\
= Title
:one: 1
:two: 2
",
            |limits: &mut ProcessingLimits| limits.max_attributes = 1,
        ),
    ];

    for (source, restrict) in cases {
        let mut limits = ProcessingLimits::default();
        restrict(&mut limits);
        let result = analyze_with_limits(source, limits);
        assert!(
            matches!(result, Err(ParseError::LimitExceeded { .. })),
            "{source:?}"
        );
    }
}

#[test]
fn construction_budgets_accept_exact_boundaries_and_reject_the_next_item() {
    let cases: [BoundaryCase; 5] = [
        (
            "blocks",
            "\
one

two",
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
            "\
= T
:a: 1
:b: 2
",
            2_u32,
            |limits: &mut ProcessingLimits, value| {
                limits.max_attributes = value;
            },
        ),
        (
            "list continuations",
            "\
* item
+
first
+
second
",
            2_u32,
            |limits: &mut ProcessingLimits, value| {
                limits.max_list_continuations = value;
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
    let source = "stem:[12345<script>]";
    let analysis = analyze_with_limits(source, limits).expect("formula overflow is recoverable");
    let html = render(analysis.document(), &RenderPolicy::default()).html;
    let diagnostics = adocweave::output::diagnostics::render_json(analysis.diagnostics());

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
    let source = "\
* one
** two
*** three
";
    let analysis = analyze_with_limits(source, limits).expect("list depth overflow is recoverable");
    let html = render(analysis.document(), &RenderPolicy::default()).html;
    let diagnostics = adocweave::output::diagnostics::render_json(analysis.diagnostics());

    assert!(html.contains("three"));
    assert!(diagnostics.contains("configured limit"));
}

#[test]
fn compound_block_depth_limit_rejects_unbounded_nesting() {
    let limits = ProcessingLimits {
        max_block_depth: 1,
        ..ProcessingLimits::default()
    };
    let source = "\
=====
outer
======
inner
======
=====
";

    assert!(matches!(
        analyze_with_limits(source, limits),
        Err(ParseError::LimitExceeded {
            resource: "block nesting depth",
            ..
        })
    ));
}

#[test]
fn asciidoc_cell_uses_the_parent_table_depth_budget() {
    let limits = ProcessingLimits {
        max_table_depth: 1,
        ..ProcessingLimits::default()
    };
    let source = "\
[cols=a]
|===
|!===
!nested
!===
|===
";

    assert!(matches!(
        analyze_with_limits(source, limits),
        Err(ParseError::LimitExceeded {
            resource: "table nesting depth",
            ..
        })
    ));
}

#[test]
fn table_resources_are_rejected_at_the_construction_boundary() {
    let cases = [
        (
            "table bytes",
            ProcessingLimits {
                max_table_bytes: 3,
                ..ProcessingLimits::default()
            },
        ),
        (
            "table cells",
            ProcessingLimits {
                max_table_cells: 1,
                ..ProcessingLimits::default()
            },
        ),
        (
            "table columns",
            ProcessingLimits {
                max_table_columns: 1,
                ..ProcessingLimits::default()
            },
        ),
        (
            "table nesting depth",
            ProcessingLimits {
                max_table_depth: 0,
                ..ProcessingLimits::default()
            },
        ),
    ];
    for (resource, limits) in cases {
        assert!(matches!(
            analyze_with_limits(
                "\
|===
|a |b
|===
",
                limits,
            ),
            Err(ParseError::LimitExceeded { resource: actual, .. }) if actual == resource
        ));
    }
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
