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
