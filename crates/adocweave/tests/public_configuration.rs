use adocweave::output::diagnostics::{LINT_RULES, LintRuleDescriptor, LintRuleId};
use adocweave::output::html::RenderPolicy;
use adocweave::resolution::{ActiveUrlPolicy, AuthoredUrlPolicy, UrlProvenance};
use adocweave::{
    AnalysisLimits, AnalysisOptions, DiagnosticProfile, Engine, OutputLimits, SyntaxOptions,
};

#[test]
fn responsibility_specific_configuration_is_publicly_importable() {
    let analysis_options = AnalysisOptions {
        syntax: SyntaxOptions {
            limits: AnalysisLimits::default(),
            ..SyntaxOptions::default()
        },
        diagnostics: DiagnosticProfile::default(),
        attributes: Default::default(),
    };
    let analysis = Engine::new(analysis_options)
        .analyze("link:https://example.com[example]")
        .expect("analysis");
    let policy = RenderPolicy {
        active_urls: ActiveUrlPolicy::default(),
        ..RenderPolicy::default()
    };

    assert!(policy.allows_url("https://example.com", UrlProvenance::Authored));
    assert!(!analysis.document().blocks().is_empty());
    assert!(AuthoredUrlPolicy::default().allows("guide.adoc"));
    assert!(OutputLimits::default().max_output_bytes > 0);

    let descriptor: &LintRuleDescriptor = &LINT_RULES[0];
    let id: LintRuleId = descriptor.id;
    assert!(!id.as_str().is_empty());
}

#[test]
fn release_note_configuration_example_uses_public_paths() {
    let mut lint = adocweave::output::diagnostics::LintConfig::default();
    lint.authored_url_policy.allow_relative = true;
    let _engine = adocweave::Engine::new(adocweave::AnalysisOptions {
        diagnostics: adocweave::DiagnosticProfile { lint },
        ..adocweave::AnalysisOptions::default()
    });
    let policy = adocweave::output::html::RenderPolicy {
        active_urls: adocweave::resolution::ActiveUrlPolicy {
            allow_authored_relative: true,
            ..adocweave::resolution::ActiveUrlPolicy::default()
        },
        ..adocweave::output::html::RenderPolicy::default()
    };

    assert!(policy.active_urls.allow_authored_relative);
}

#[test]
fn projection_product_field_types_are_publicly_nameable() {
    let _: Option<adocweave::preprocess::ProjectedAttributeBinding> = None;
    let _: Option<adocweave::preprocess::ProjectedAttributeReference> = None;
    let _: Option<adocweave::preprocess::ProjectedDiagnostic> = None;
    let _: Option<adocweave::preprocess::ProjectedDocumentAttribute> = None;
    let _: Option<adocweave::preprocess::ProjectedDocumentAttributeValueLine> = None;
    let _: Option<adocweave::preprocess::ProjectedDocumentSymbol> = None;
    let _: Option<adocweave::preprocess::ProjectedFix> = None;
    let _: Option<adocweave::preprocess::ProjectedLocalTarget> = None;
    let _: Option<adocweave::preprocess::ProjectedReference> = None;
    let _: Option<adocweave::preprocess::ProjectedResource> = None;
}

#[test]
fn cancellable_lint_api_is_public() {
    let analysis = Engine::new(AnalysisOptions::default())
        .analyze("paragraph\n")
        .expect("analysis");
    let cancellation = adocweave::CancellationToken::new();
    cancellation.cancel();

    assert_eq!(
        adocweave::output::diagnostics::lint_analysis(
            &analysis,
            &adocweave::output::diagnostics::LintConfig::default(),
            &cancellation,
        )
        .expect_err("cancelled lint"),
        adocweave::output::diagnostics::LintError::Cancelled
    );
}
