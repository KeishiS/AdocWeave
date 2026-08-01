//! Lint rule identifiers reached through the public API.
//!
//! A rule is written in two hand-maintained places: the catalog macro that
//! defines its constant, and the re-export list in the crate root.
//! `unprocessed-directive` was released with only the first, so the rule
//! produced diagnostics but consumers could not name it. Naming it by string
//! still worked, and that is the failure mode these tests exist to prevent: a
//! renamed rule then goes unnoticed until a document is accepted that should
//! not be.

use adocweave::output::diagnostics::{
    LINT_RULES, LintConfig, RuleSettings, Severity, UNPROCESSED_DIRECTIVE,
};

#[test]
fn the_unprocessed_directive_rule_is_named_by_constant() {
    assert_eq!(UNPROCESSED_DIRECTIVE.as_str(), "unprocessed-directive");
}

#[test]
fn the_rule_constant_configures_the_rule_it_names() {
    let mut config = LintConfig::default();
    assert!(
        config.rule(UNPROCESSED_DIRECTIVE).enabled,
        "the rule reports without configuration"
    );

    config.set_rule(
        UNPROCESSED_DIRECTIVE,
        RuleSettings {
            enabled: false,
            severity: Severity::Warning,
        },
    );
    assert!(!config.rule(UNPROCESSED_DIRECTIVE).enabled);
}

#[test]
fn the_catalog_describes_the_rule_the_constant_names() {
    let descriptor = LINT_RULES
        .iter()
        .find(|rule| rule.id == UNPROCESSED_DIRECTIVE)
        .expect("catalog entry");

    assert!(descriptor.default_enabled);
    assert!(descriptor.user_configurable);
    assert!(!descriptor.fixable);
}
