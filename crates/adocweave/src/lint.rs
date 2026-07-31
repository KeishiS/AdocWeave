//! Output-independent lint catalog, configuration, and orchestration.

mod attributes;
mod catalogs;
mod presentation;
mod references;
mod source;
mod structure;
mod syntax;
mod tables;

use std::collections::BTreeMap;
use std::error::Error;
use std::fmt;

use crate::cancellation::CancellationCheckpoint;
use crate::core::{CancellationCheck, NeverCancel};
use crate::diagnostic::{
    Applicability, Diagnostic, DiagnosticCode, DiagnosticId, Fix, RelatedInformation, Severity,
    TextEdit, sort_diagnostics,
};
#[cfg(test)]
use crate::parser::{ParseConfig, parse_with_config};
#[cfg(test)]
use crate::source::TextSize;
use crate::source::{PositionError, TextRange};
use crate::syntax::SyntaxTree;

/// Immutable syntax and semantic views produced by one parser execution.
///
/// Rule groups receive this value instead of independent inputs so every rule
/// observes the same analysis snapshot.
pub(crate) struct LintContext<'a> {
    syntax: &'a SyntaxTree,
    document: &'a crate::parser::AstDocument,
}

impl<'a> LintContext<'a> {
    pub(crate) const fn new(
        syntax: &'a SyntaxTree,
        document: &'a crate::parser::AstDocument,
    ) -> Self {
        Self { syntax, document }
    }

    const fn syntax(&self) -> &'a SyntaxTree {
        self.syntax
    }

    const fn document(&self) -> &'a crate::parser::AstDocument {
        self.document
    }

    fn source_document(&self) -> &'a crate::source_document::SourceDocument {
        self.syntax.source_document()
    }
}

/// Stable identifier for a lint rule.
///
/// Rule identifiers are values rather than enum variants, so adding a rule
/// does not break exhaustive matches in callers.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct LintRuleId(&'static str);

impl LintRuleId {
    pub const fn as_str(self) -> &'static str {
        self.0
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LintRuleDescriptor {
    pub id: LintRuleId,
    pub default_enabled: bool,
    pub default_severity: Severity,
    pub description: &'static str,
    pub fixable: bool,
    pub user_configurable: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LintError {
    Position(PositionError),
    Cancelled,
}

impl fmt::Display for LintError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Position(error) => error.fmt(formatter),
            Self::Cancelled => formatter.write_str("linting was cancelled"),
        }
    }
}

impl Error for LintError {}

macro_rules! lint_rule_catalog {
    (@enabled) => {
        true
    };
    (@enabled $enabled:literal) => {
        $enabled
    };
    ($(($constant:ident, $code:literal, $description:literal, $fixable:literal $(, $default_enabled:literal)?)),+ $(,)?) => {
        $(pub const $constant: LintRuleId = LintRuleId($code);)+

        pub const LINT_RULES: &[LintRuleDescriptor] = &[
            $(LintRuleDescriptor {
                id: $constant,
                default_enabled: lint_rule_catalog!(@enabled $($default_enabled)?),
                default_severity: Severity::Warning,
                description: $description,
                fixable: $fixable,
                user_configurable: true,
            }),+
        ];
    };
}

lint_rule_catalog!(
    (
        TRAILING_WHITESPACE,
        "trailing-whitespace",
        "行末の不要な空白",
        true
    ),
    (
        EXCESSIVE_BLANK_LINES,
        "excessive-blank-lines",
        "連続する空行の上限超過",
        true
    ),
    (LINE_TOO_LONG, "line-too-long", "行長の上限超過", false),
    (
        INVALID_HEADING_LEVEL,
        "invalid-heading-level",
        "不正な見出しレベル",
        false
    ),
    (
        DUPLICATE_HEADING_ID,
        "duplicate-heading-id",
        "重複する見出しID",
        false
    ),
    (
        HEADING_MARKER_SPACE,
        "heading-marker-space",
        "見出し記号の後の空白不足",
        true
    ),
    (
        UNCLOSED_INLINE,
        "unclosed-inline",
        "閉じられていないインライン構文",
        false
    ),
    (
        NESTING_LIMIT_EXCEEDED,
        "nesting-limit-exceeded",
        "構文の入れ子上限超過",
        false
    ),
    (
        UNCLOSED_BLOCK,
        "unclosed-block",
        "閉じられていないブロック",
        false
    ),
    (
        MISSING_SOURCE_LANGUAGE,
        "missing-source-language",
        "ソースブロックの言語指定不足",
        false
    ),
    (
        INVALID_ATTRIBUTE,
        "invalid-attribute",
        "不正な文書属性",
        false
    ),
    (
        UNDEFINED_ATTRIBUTE,
        "undefined-attribute",
        "未定義の文書属性参照",
        false
    ),
    (
        ATTRIBUTE_EXPANSION,
        "attribute-expansion",
        "不正な文書属性展開",
        false
    ),
    (
        UNUSED_ATTRIBUTE,
        "unused-attribute",
        "使用されていない文書属性",
        false,
        false
    ),
    (
        PROTECTED_ATTRIBUTE,
        "protected-attribute",
        "保護された文書属性の変更",
        false
    ),
    (INVALID_ANCHOR, "invalid-anchor", "不正なアンカー", false),
    (
        DUPLICATE_ANCHOR,
        "duplicate-anchor",
        "重複するアンカー",
        false
    ),
    (
        INVALID_URL_SCHEME,
        "invalid-url-scheme",
        "許可されていないURL",
        false
    ),
    (
        INVALID_CROSS_REFERENCE,
        "invalid-cross-reference",
        "不正な相互参照",
        false
    ),
    (
        UNRESOLVED_CROSS_REFERENCE,
        "unresolved-cross-reference",
        "未解決の相互参照",
        false
    ),
    (
        ASCIIDOC_FILE_LINK,
        "asciidoc-file-link",
        "AsciiDoc文書への通常リンク",
        true
    ),
    (
        NON_ASCIIDOC_XREF,
        "non-asciidoc-xref",
        "AsciiDoc以外のファイルへの相互参照",
        true
    ),
    (
        MACRO_BOUNDARY,
        "macro-boundary",
        "inline macroの開始境界違反",
        true,
        false
    ),
    (
        INCONSISTENT_LIST,
        "inconsistent-list",
        "一貫しないリスト構造",
        true
    ),
    (
        INVALID_LIST_PRESENTATION,
        "invalid-list-presentation",
        "不正なリスト表示指定",
        false
    ),
    (INVALID_STEM, "invalid-stem", "不正な数式構文", false),
    (INVALID_TABLE, "invalid-table", "不正な表", false),
    (
        INVALID_CATALOG,
        "invalid-catalog",
        "不正な文書カタログ",
        false
    ),
    (
        INVALID_DOCUMENT_STRUCTURE,
        "invalid-document-structure",
        "不正な文書構造",
        false
    ),
);

pub fn lint_rule(code: &str) -> Option<&'static LintRuleDescriptor> {
    LINT_RULES
        .iter()
        .find(|descriptor| descriptor.id.as_str() == code)
}

pub fn render_lint_rule_catalog_json() -> String {
    let mut rules = LINT_RULES.iter().collect::<Vec<_>>();
    rules.sort_by_key(|descriptor| descriptor.id.as_str());
    serde_json::to_string(&serde_json::json!({
        "schemaVersion": 1,
        "packageVersion": crate::VERSION,
        "rules": rules
            .into_iter()
            .map(|descriptor| serde_json::json!({
                "code": descriptor.id.as_str(),
                "defaultSeverity": descriptor.default_severity.as_str(),
                "enabledByDefault": descriptor.default_enabled,
                "description": descriptor.description,
                "fixable": descriptor.fixable,
                "userConfigurable": descriptor.user_configurable,
            }))
            .collect::<Vec<_>>(),
    }))
    .expect("lint rule catalog contains only serializable values")
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RuleSettings {
    pub enabled: bool,
    pub severity: Severity,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LintConfig {
    rules: BTreeMap<LintRuleId, RuleSettings>,
    pub max_line_length: usize,
    pub max_consecutive_blank_lines: usize,
    pub max_diagnostics: usize,
    pub protected_attributes: BTreeMap<String, Option<String>>,
    pub authored_url_policy: crate::url::AuthoredUrlPolicy,
}

impl Default for LintConfig {
    fn default() -> Self {
        let mut rules = LINT_RULES
            .iter()
            .map(|descriptor| {
                (
                    descriptor.id,
                    RuleSettings {
                        enabled: descriptor.default_enabled,
                        severity: descriptor.default_severity,
                    },
                )
            })
            .collect::<BTreeMap<_, _>>();
        rules.insert(
            PROTECTED_ATTRIBUTE,
            RuleSettings {
                enabled: true,
                severity: Severity::Error,
            },
        );
        Self {
            rules,
            max_line_length: 100,
            max_consecutive_blank_lines: 2,
            max_diagnostics: 1_000,
            protected_attributes: BTreeMap::new(),
            authored_url_policy: crate::url::AuthoredUrlPolicy::default(),
        }
    }
}

impl LintConfig {
    pub fn set_rule(&mut self, rule: LintRuleId, settings: RuleSettings) {
        self.rules.insert(rule, settings);
    }

    pub fn rule(&self, rule: LintRuleId) -> RuleSettings {
        self.rules.get(&rule).copied().unwrap_or(RuleSettings {
            enabled: false,
            severity: lint_rule(rule.as_str())
                .map_or(Severity::Warning, |descriptor| descriptor.default_severity),
        })
    }

    pub(crate) fn configured_rules(
        &self,
    ) -> impl ExactSizeIterator<Item = (LintRuleId, RuleSettings)> + '_ {
        self.rules.iter().map(|(rule, settings)| (*rule, *settings))
    }
}

struct LintDiagnosticSink<'a> {
    config: &'a LintConfig,
    diagnostics: Vec<Diagnostic>,
    cancellation: CancellationCheckpoint<'a>,
    cancelled: bool,
}

struct LintDiagnosticBody {
    message: String,
    related: Vec<RelatedInformation>,
    fixes: Vec<LintFixSpec>,
}

impl LintDiagnosticBody {
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            related: Vec::new(),
            fixes: Vec::new(),
        }
    }

    fn with_related(mut self, related: Vec<RelatedInformation>) -> Self {
        self.related = related;
        self
    }

    fn with_fix(
        mut self,
        title: impl Into<String>,
        applicability: Applicability,
        edits: Vec<TextEdit>,
    ) -> Self {
        self.fixes.push(LintFixSpec {
            title: title.into(),
            applicability,
            edits,
        });
        self
    }

    fn with_edit_fix(
        self,
        title: impl Into<String>,
        range: TextRange,
        replacement: impl Into<String>,
        applicability: Applicability,
    ) -> Self {
        self.with_fix(
            title,
            applicability,
            vec![TextEdit {
                range,
                replacement: replacement.into(),
            }],
        )
    }

    fn with_optional_fix(
        self,
        fix: Option<(&str, TextRange, &str)>,
        applicability: Applicability,
    ) -> Self {
        match fix {
            Some((title, range, replacement)) => {
                self.with_edit_fix(title, range, replacement, applicability)
            }
            None => self,
        }
    }
}

struct LintFixSpec {
    title: String,
    applicability: Applicability,
    edits: Vec<TextEdit>,
}

impl<'a> LintDiagnosticSink<'a> {
    #[cfg(test)]
    fn new(config: &'a LintConfig) -> Self {
        Self::new_cancellable(config, &NeverCancel)
    }

    fn new_cancellable(config: &'a LintConfig, cancellation: &'a dyn CancellationCheck) -> Self {
        Self {
            config,
            diagnostics: Vec::new(),
            cancellation: CancellationCheckpoint::new(cancellation),
            cancelled: false,
        }
    }

    fn is_full(&self) -> bool {
        self.diagnostics.len() >= self.config.max_diagnostics
    }

    fn config(&self) -> &LintConfig {
        self.config
    }

    fn should_stop(&mut self) -> bool {
        if self.cancelled || self.is_full() {
            return true;
        }
        self.cancelled = self.cancellation.is_cancelled();
        self.cancelled
    }

    /// Records one diagnostic, dropping a fix this rule may not offer.
    ///
    /// A rule that produces a fix it did not declare, or edits that overlap, is
    /// a defect in this crate. It is still not a reason to end the calling
    /// process: a Language Server that dies mid-keystroke loses the editor
    /// session, and a command-line run loses every diagnostic it had already
    /// found. The diagnostic itself is reported without the fix, so the author
    /// keeps the finding and the defect stays visible in the tests below.
    fn emit(
        &mut self,
        rule: LintRuleId,
        range: TextRange,
        body: impl FnOnce() -> LintDiagnosticBody,
    ) {
        let Some(descriptor) = lint_rule(rule.as_str()) else {
            debug_assert!(false, "lint diagnostic rule is not registered: {rule:?}");
            return;
        };
        if self.is_full() {
            return;
        }
        let settings = self.config.rule(rule);
        if !settings.enabled {
            return;
        }
        let body = body();
        debug_assert!(
            body.fixes.is_empty() || descriptor.fixable,
            "non-fixable lint rule emitted a fix: {}",
            rule.as_str()
        );
        let fixes = if descriptor.fixable {
            body.fixes
                .into_iter()
                .filter_map(|fix| {
                    let title = fix.title.clone();
                    Fix::new(fix.title, fix.applicability, fix.edits)
                        .inspect_err(|_| {
                            debug_assert!(false, "lint fix edits conflict: {rule:?} {title}");
                        })
                        .ok()
                })
                .collect()
        } else {
            Vec::new()
        };
        self.diagnostics.push(Diagnostic {
            id: DiagnosticId::new(format!(
                "{}@{}:{}",
                rule.as_str(),
                range.start().to_u32(),
                range.end().to_u32()
            )),
            code: DiagnosticCode::new(rule.as_str()),
            severity: settings.severity,
            message: body.message,
            range,
            related: body.related,
            fixes,
        });
    }

    fn finish(mut self) -> Vec<Diagnostic> {
        sort_diagnostics(&mut self.diagnostics);
        self.diagnostics
    }
}

#[cfg(test)]
fn lint(source: &str, config: &LintConfig) -> Result<Vec<Diagnostic>, PositionError> {
    lint_with_analysis_limits(source, config, crate::limits::AnalysisLimits::default())
}

#[cfg(test)]
fn lint_with_analysis_limits(
    source: &str,
    config: &LintConfig,
    limits: crate::limits::AnalysisLimits,
) -> Result<Vec<Diagnostic>, PositionError> {
    let parsed = parse_with_config(
        source,
        &ParseConfig {
            max_inline_depth: usize::try_from(limits.max_inline_depth)
                .expect("u32 fits usize on supported targets"),
            max_list_depth: usize::try_from(limits.max_list_depth)
                .expect("u32 fits usize on supported targets"),
            max_formula_bytes: usize::try_from(limits.max_formula_bytes)
                .expect("u32 fits usize on supported targets"),
            limits: crate::limits::AnalysisLimits {
                max_attribute_expansion_depth: limits.max_attribute_expansion_depth,
                max_attribute_expansion_bytes: limits.max_attribute_expansion_bytes,
                ..ParseConfig::default().limits
            },
            ..ParseConfig::default()
        },
    )?;
    lint_parsed_document(LintContext::new(&parsed.syntax, &parsed.ast), config)
}

pub fn lint_analysis(
    analysis: &crate::core::Analysis,
    config: &LintConfig,
) -> Result<Vec<Diagnostic>, PositionError> {
    match lint_analysis_cancellable(analysis, config, &NeverCancel) {
        Ok(diagnostics) => Ok(diagnostics),
        Err(LintError::Position(error)) => Err(error),
        Err(LintError::Cancelled) => {
            unreachable!("NeverCancel cannot cancel lint analysis")
        }
    }
}

/// Applies diagnostics to one analysis with cooperative cancellation.
pub fn lint_analysis_cancellable(
    analysis: &crate::core::Analysis,
    config: &LintConfig,
    cancellation: &dyn CancellationCheck,
) -> Result<Vec<Diagnostic>, LintError> {
    lint_parsed_document_cancellable(
        LintContext::new(analysis.syntax(), analysis.ast()),
        config,
        cancellation,
    )
}

#[cfg(test)]
pub(crate) fn lint_parsed_document(
    context: LintContext<'_>,
    config: &LintConfig,
) -> Result<Vec<Diagnostic>, PositionError> {
    match lint_parsed_document_cancellable(context, config, &NeverCancel) {
        Ok(diagnostics) => Ok(diagnostics),
        Err(LintError::Position(error)) => Err(error),
        Err(LintError::Cancelled) => {
            unreachable!("NeverCancel cannot cancel lint analysis")
        }
    }
}

pub(crate) fn lint_parsed_document_cancellable(
    context: LintContext<'_>,
    config: &LintConfig,
    cancellation: &dyn CancellationCheck,
) -> Result<Vec<Diagnostic>, LintError> {
    let mut sink = LintDiagnosticSink::new_cancellable(config, cancellation);
    source::lint_source_lines(&context, &mut sink).map_err(LintError::Position)?;

    if !sink.should_stop() {
        syntax::lint_syntax_issues(&context, &mut sink);
    }
    if !sink.should_stop() {
        structure::lint_headings(&context, &mut sink);
    }
    if !sink.should_stop() {
        attributes::lint_attributes(&context, &mut sink);
    }
    if !sink.should_stop() {
        references::lint_anchors(&context, &mut sink);
    }
    if !sink.should_stop() {
        references::lint_links_and_references(&context, &mut sink);
    }
    if !sink.should_stop() {
        presentation::lint_list_presentation(&context, &mut sink);
    }
    if !sink.should_stop() {
        presentation::lint_document_presentation(&context, &mut sink);
    }
    if !sink.should_stop() {
        tables::lint_tables(&context, &mut sink);
    }
    if !sink.should_stop() {
        catalogs::lint_catalogs(&context, &mut sink);
    }
    if !sink.should_stop() {
        structure::lint_document_structure(&context, &mut sink);
    }
    if sink.cancelled || sink.cancellation.is_cancelled_now() {
        Err(LintError::Cancelled)
    } else {
        Ok(sink.finish())
    }
}

#[cfg(test)]
fn text_range(start: usize, end: usize) -> Result<TextRange, PositionError> {
    TextRange::new(TextSize::new(start)?, TextSize::new(end)?)
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::{cell::Cell, ops::ControlFlow};

    use super::{
        DUPLICATE_ANCHOR, DUPLICATE_HEADING_ID, INVALID_ANCHOR, INVALID_CATALOG, INVALID_TABLE,
        LINE_TOO_LONG, LINT_RULES, LintConfig, LintContext, LintDiagnosticBody, LintDiagnosticSink,
        LintError, LintRuleId, MACRO_BOUNDARY, PROTECTED_ATTRIBUTE, RuleSettings,
        TRAILING_WHITESPACE, UNUSED_ATTRIBUTE, lint, lint_analysis, lint_analysis_cancellable,
        lint_rule, lint_with_analysis_limits, render_lint_rule_catalog_json, text_range,
    };
    use crate::core::{AnalysisOptions, CancellationCheck, Engine};
    use crate::diagnostic::{Applicability, RelatedInformation, Severity, TextEdit};

    /// A defect inside a lint rule must not end the calling process.
    ///
    /// The Language Server answers keystrokes from one process, so a panic here
    /// takes the editor session with it. The command-line interface loses every
    /// diagnostic it had already found. Both are worse outcomes than reporting
    /// the finding without its fix, so these paths are exercised in release
    /// builds where the debug assertions do not fire.
    #[test]
    #[cfg(not(debug_assertions))]
    fn a_defective_rule_loses_its_fix_rather_than_the_whole_run() {
        let range = text_range(0, 1).expect("range");
        let edit = TextEdit {
            range,
            replacement: "x".to_owned(),
        };
        let conflicting = LintDiagnosticBody {
            message: "message".to_owned(),
            related: Vec::new(),
            fixes: vec![super::LintFixSpec {
                title: "title".to_owned(),
                applicability: Applicability::Always,
                // The same range twice cannot be applied as one fix.
                edits: vec![edit.clone(), edit],
            }],
        };

        let config = LintConfig::default();

        // A rule that may offer a fix, but produced one that cannot be applied.
        let mut sink = LintDiagnosticSink::new(&config);
        sink.emit(TRAILING_WHITESPACE, range, || conflicting);
        let diagnostics = sink.finish();
        assert_eq!(diagnostics.len(), 1, "the finding survives");
        assert!(
            diagnostics[0].fixes.is_empty(),
            "the unusable fix is dropped"
        );

        // A rule that may not offer a fix at all, but produced one.
        let mut sink = LintDiagnosticSink::new(&config);
        sink.emit(LINE_TOO_LONG, range, || LintDiagnosticBody {
            message: "message".to_owned(),
            related: Vec::new(),
            fixes: vec![super::LintFixSpec {
                title: "title".to_owned(),
                applicability: Applicability::Always,
                edits: vec![TextEdit {
                    range,
                    replacement: String::new(),
                }],
            }],
        });
        let diagnostics = sink.finish();
        assert_eq!(diagnostics.len(), 1, "the finding survives");
        assert!(
            diagnostics[0].fixes.is_empty(),
            "the undeclared fix is dropped"
        );

        // A rule the catalog does not know reports nothing and returns.
        let mut sink = LintDiagnosticSink::new(&config);
        sink.emit(LintRuleId("never-registered"), range, || {
            LintDiagnosticBody {
                message: "message".to_owned(),
                related: Vec::new(),
                fixes: Vec::new(),
            }
        });
        assert!(sink.finish().is_empty());
    }

    #[test]
    fn linting_cancels_at_a_bounded_line_checkpoint() {
        struct CancelAfterFirstCheckpoint(AtomicUsize);

        impl CancellationCheck for CancelAfterFirstCheckpoint {
            fn is_cancelled(&self) -> bool {
                self.0.fetch_add(1, Ordering::Relaxed) >= 1
            }
        }

        let source = "paragraph\n".repeat(crate::cancellation::CHECKPOINT_INTERVAL * 3);
        let analysis = Engine::new(AnalysisOptions::default())
            .analyze(&source)
            .expect("analysis");
        let cancellation = CancelAfterFirstCheckpoint(AtomicUsize::new(0));

        let error = lint_analysis_cancellable(&analysis, &LintConfig::default(), &cancellation)
            .expect_err("linting should be cancelled");

        assert_eq!(error, LintError::Cancelled);
        assert_eq!(cancellation.0.load(Ordering::Relaxed), 2);
    }

    #[test]
    fn attribute_lint_checks_cancellation_inside_binding_reference_indexing() {
        struct CancelAfterFirstCheckpoint(AtomicUsize);

        impl CancellationCheck for CancelAfterFirstCheckpoint {
            fn is_cancelled(&self) -> bool {
                self.0.fetch_add(1, Ordering::Relaxed) >= 1
            }
        }

        let mut source = String::new();
        for index in 0..160 {
            source.push_str(&format!(":attribute-{index}: value\n"));
        }
        source.push('\n');
        for index in 0..160 {
            source.push_str(&format!("{{attribute-{index}}} "));
        }
        source.push('\n');
        let analysis = Engine::new(AnalysisOptions::default())
            .analyze(&source)
            .expect("analysis");
        let cancellation = CancelAfterFirstCheckpoint(AtomicUsize::new(0));
        let config = LintConfig::default();
        let mut sink = LintDiagnosticSink::new_cancellable(&config, &cancellation);

        super::attributes::lint_attributes(
            &LintContext::new(analysis.syntax(), analysis.ast()),
            &mut sink,
        );

        assert!(sink.cancelled);
        assert_eq!(cancellation.0.load(Ordering::Relaxed), 2);
    }

    #[test]
    fn reference_target_index_construction_is_cancellable() {
        struct CancelAfterFirstCheckpoint(AtomicUsize);

        impl CancellationCheck for CancelAfterFirstCheckpoint {
            fn is_cancelled(&self) -> bool {
                self.0.fetch_add(1, Ordering::Relaxed) >= 1
            }
        }

        let source = (0..crate::cancellation::CHECKPOINT_INTERVAL * 2)
            .map(|index| format!("[[target-{index}]]\nparagraph\n\n"))
            .collect::<String>();
        let analysis = Engine::new(AnalysisOptions::default())
            .analyze(&source)
            .expect("analysis");
        let cancellation = CancelAfterFirstCheckpoint(AtomicUsize::new(0));
        let config = LintConfig::default();
        let mut sink = LintDiagnosticSink::new_cancellable(&config, &cancellation);

        super::references::lint_links_and_references(
            &LintContext::new(analysis.syntax(), analysis.ast()),
            &mut sink,
        );

        assert!(sink.cancelled);
        assert_eq!(cancellation.0.load(Ordering::Relaxed), 2);
    }

    #[test]
    fn never_cancel_lint_api_preserves_diagnostics() {
        let analysis = Engine::new(AnalysisOptions::default())
            .analyze("paragraph  \n\nxref:missing[Missing]\n")
            .expect("analysis");
        let config = LintConfig::default();

        assert_eq!(
            lint_analysis_cancellable(&analysis, &config, &crate::core::NeverCancel)
                .expect("cancellable lint"),
            lint_analysis(&analysis, &config).expect("compatibility lint")
        );
    }

    #[test]
    fn lint_rule_catalog_is_unique_resolvable_and_sorted_in_json() {
        let mut codes = LINT_RULES
            .iter()
            .map(|descriptor| descriptor.id.as_str())
            .collect::<Vec<_>>();
        let original_len = codes.len();
        codes.sort_unstable();
        codes.dedup();
        assert_eq!(codes.len(), original_len);
        assert!(
            LINT_RULES
                .iter()
                .all(|descriptor| lint_rule(descriptor.id.as_str()) == Some(descriptor))
        );

        let value: serde_json::Value =
            serde_json::from_str(&render_lint_rule_catalog_json()).expect("catalog JSON");
        let mut descriptors = LINT_RULES.iter().collect::<Vec<_>>();
        descriptors.sort_by_key(|descriptor| descriptor.id.as_str());
        let expected = serde_json::json!({
            "schemaVersion": 1,
            "packageVersion": crate::VERSION,
            "rules": descriptors
                .into_iter()
                .map(|descriptor| serde_json::json!({
                    "code": descriptor.id.as_str(),
                    "defaultSeverity": descriptor.default_severity.as_str(),
                    "enabledByDefault": descriptor.default_enabled,
                    "description": descriptor.description,
                    "fixable": descriptor.fixable,
                    "userConfigurable": descriptor.user_configurable,
                }))
                .collect::<Vec<_>>(),
        });
        assert_eq!(value, expected);
    }

    #[test]
    fn every_rule_is_project_configurable() {
        for descriptor in LINT_RULES {
            assert!(
                descriptor.user_configurable,
                "{} must be configurable",
                descriptor.id.as_str()
            );
        }
    }

    #[test]
    fn protected_attribute_defaults_are_explicit_for_lint_and_analysis_profiles() {
        assert_eq!(
            lint_rule(PROTECTED_ATTRIBUTE.as_str())
                .expect("registered rule")
                .default_severity,
            Severity::Warning
        );
        assert_eq!(
            LintConfig::default().rule(PROTECTED_ATTRIBUTE).severity,
            Severity::Error
        );
        assert_eq!(
            crate::core::AnalysisOptions::default()
                .diagnostics
                .lint
                .rule(PROTECTED_ATTRIBUTE)
                .severity,
            Severity::Warning
        );

        let mut config = LintConfig::default();
        config
            .protected_attributes
            .insert("locked".to_owned(), Some("expected".to_owned()));
        let diagnostics = lint(":locked: changed\n", &config).expect("lint");
        assert!(diagnostics.iter().any(|diagnostic| {
            diagnostic.code.as_str() == PROTECTED_ATTRIBUTE.as_str()
                && diagnostic.severity == Severity::Error
        }));
    }

    #[test]
    fn diagnostic_sink_skips_payloads_for_disabled_and_full_rules() {
        let mut config = LintConfig {
            max_diagnostics: 2,
            ..LintConfig::default()
        };
        config.set_rule(
            TRAILING_WHITESPACE,
            RuleSettings {
                enabled: false,
                severity: Severity::Error,
            },
        );
        let calls = Cell::new(0);
        let mut sink = LintDiagnosticSink::new(&config);
        let range = text_range(0, 1).expect("range");

        sink.emit(TRAILING_WHITESPACE, range, || {
            calls.set(calls.get() + 1);
            LintDiagnosticBody::new("disabled")
        });
        assert_eq!(calls.get(), 0);
        sink.emit(LINE_TOO_LONG, range, || {
            calls.set(calls.get() + 1);
            LintDiagnosticBody::new("accepted")
        });
        assert_eq!(calls.get(), 1);
        sink.emit(INVALID_ANCHOR, range, || {
            calls.set(calls.get() + 1);
            LintDiagnosticBody::new("second")
        });
        assert_eq!(calls.get(), 2);
        sink.emit(INVALID_TABLE, range, || {
            calls.set(calls.get() + 1);
            LintDiagnosticBody::new("full")
        });
        assert_eq!(calls.get(), 2);

        let zero = LintConfig {
            max_diagnostics: 0,
            ..LintConfig::default()
        };
        let mut zero_sink = LintDiagnosticSink::new(&zero);
        zero_sink.emit(LINE_TOO_LONG, range, || {
            calls.set(calls.get() + 1);
            LintDiagnosticBody::new("zero")
        });
        assert_eq!(calls.get(), 2);
    }

    #[test]
    fn semantic_phase_limit_is_applied_before_canonical_sort() {
        let source = concat!(
            "[format=unknown]\n",
            "|===\n",
            "|cell\n",
            "|===\n",
            "\n",
            "[start=0]\n",
            ". item\n",
            "\n",
            "link:guide.adoc[guide]\n",
        );
        for (max_diagnostics, expected) in [
            (0, Vec::<&str>::new()),
            (1, vec!["asciidoc-file-link"]),
            (2, vec!["invalid-list-presentation", "asciidoc-file-link"]),
            (
                3,
                vec![
                    "invalid-table",
                    "invalid-list-presentation",
                    "asciidoc-file-link",
                ],
            ),
        ] {
            let config = LintConfig {
                max_diagnostics,
                ..LintConfig::default()
            };

            let diagnostics = lint(source, &config).expect("lint");
            let codes = diagnostics
                .iter()
                .map(|diagnostic| diagnostic.code.as_str())
                .collect::<Vec<_>>();

            assert_eq!(codes, expected, "max_diagnostics={max_diagnostics}");
        }
    }

    #[test]
    fn source_phase_limit_is_applied_before_later_phases_and_canonical_sort() {
        let source = "long \n*x\n";
        for (max_diagnostics, expected) in [
            (0, Vec::<&str>::new()),
            (1, vec!["trailing-whitespace"]),
            (2, vec!["line-too-long", "trailing-whitespace"]),
            (
                3,
                vec!["line-too-long", "trailing-whitespace", "unclosed-inline"],
            ),
        ] {
            let config = LintConfig {
                max_line_length: 4,
                max_diagnostics,
                ..LintConfig::default()
            };

            let diagnostics = lint(source, &config).expect("lint");
            let codes = diagnostics
                .iter()
                .map(|diagnostic| diagnostic.code.as_str())
                .collect::<Vec<_>>();

            assert_eq!(codes, expected, "max_diagnostics={max_diagnostics}");
        }
    }

    #[test]
    fn syntax_phase_limit_is_applied_before_semantic_phases_and_canonical_sort() {
        let source = "*x\n\n== Repeated\n\n== Repeated\n";
        for (max_diagnostics, expected) in [
            (0, Vec::<&str>::new()),
            (1, vec!["unclosed-inline"]),
            (2, vec!["unclosed-inline", "duplicate-heading-id"]),
        ] {
            let config = LintConfig {
                max_diagnostics,
                ..LintConfig::default()
            };

            let diagnostics = lint(source, &config).expect("lint");
            let codes = diagnostics
                .iter()
                .map(|diagnostic| diagnostic.code.as_str())
                .collect::<Vec<_>>();

            assert_eq!(codes, expected, "max_diagnostics={max_diagnostics}");
        }
    }

    #[test]
    fn link_phase_stops_immediately_when_a_node_fills_the_sink() {
        let source = "link:first.adoc[first]\nlink:second.adoc[second]\n";
        for (max_diagnostics, expected) in [
            (1, vec!["invalid-url-scheme"]),
            (2, vec!["asciidoc-file-link", "invalid-url-scheme"]),
            (
                3,
                vec![
                    "asciidoc-file-link",
                    "invalid-url-scheme",
                    "invalid-url-scheme",
                ],
            ),
        ] {
            let mut config = LintConfig {
                max_diagnostics,
                ..LintConfig::default()
            };
            config.authored_url_policy.allow_relative = false;

            let diagnostics = lint(source, &config).expect("lint");
            let codes = diagnostics
                .iter()
                .map(|diagnostic| diagnostic.code.as_str())
                .collect::<Vec<_>>();

            assert_eq!(codes, expected, "max_diagnostics={max_diagnostics}");
        }
    }

    #[test]
    fn semantic_lint_phases_stop_walking_when_the_sink_fills() {
        fn node_kind(node: crate::walker::SemanticNode<'_>) -> &'static str {
            match node {
                crate::walker::SemanticNode::Block(crate::parser::AstBlock::List(_)) => {
                    "block-list"
                }
                crate::walker::SemanticNode::Block(_) => "block",
                crate::walker::SemanticNode::Table(_) => "table",
                crate::walker::SemanticNode::Inline(crate::inline::Inline::Link(_)) => {
                    "inline-link"
                }
                crate::walker::SemanticNode::Inline(_) => "inline",
                crate::walker::SemanticNode::List(_) => "list",
                crate::walker::SemanticNode::ListItem(_) => "list-item",
                crate::walker::SemanticNode::TableRow(_) => "table-row",
                crate::walker::SemanticNode::TableCell(_) => "table-cell",
                crate::walker::SemanticNode::Attribute(_) => "attribute",
                crate::walker::SemanticNode::Anchor(_) => "anchor",
                crate::walker::SemanticNode::Metadata(_) => "metadata",
                crate::walker::SemanticNode::MetadataTitle(_) => "metadata-title",
                crate::walker::SemanticNode::MetadataId(_) => "metadata-id",
                crate::walker::SemanticNode::MetadataRole(_) => "metadata-role",
                crate::walker::SemanticNode::MetadataOption(_) => "metadata-option",
                crate::walker::SemanticNode::ElementAttribute(_) => "element-attribute",
            }
        }

        fn full_walk_len(document: &crate::parser::AstDocument) -> usize {
            let mut count = 0;
            let result = crate::walker::try_walk_ast(document, |_| {
                count += 1;
                ControlFlow::<()>::Continue(())
            });
            assert_eq!(result, ControlFlow::Continue(()));
            count
        }

        let list =
            crate::parser::parse("[start=0]\n. first\n\nparagraph after\n").expect("list source");
        let config = LintConfig {
            max_diagnostics: 1,
            ..LintConfig::default()
        };
        let mut sink = LintDiagnosticSink::new(&config);
        let mut visited = Vec::new();
        super::presentation::lint_list_presentation_with_observer(&list.ast, &mut sink, |node| {
            visited.push(node_kind(node));
        });
        assert_eq!(sink.finish().len(), 1);
        assert_eq!(visited.last(), Some(&"block-list"));
        assert!(visited.len() < full_walk_len(&list.ast));

        let table =
            crate::parser::parse("[format=unknown]\n|===\n|cell\n|===\n\nparagraph after\n")
                .expect("table source");
        let mut sink = LintDiagnosticSink::new(&config);
        let mut visited = Vec::new();
        super::tables::lint_tables_with_observer(&table.ast, &mut sink, |node| {
            visited.push(node_kind(node));
        });
        assert_eq!(sink.finish().len(), 1);
        assert_eq!(visited.last(), Some(&"table"));
        assert!(visited.len() < full_walk_len(&table.ast));

        let links = crate::parser::parse("link:first.adoc[first]\nlink:second.adoc[second]\n")
            .expect("link source");
        let mut link_config = config.clone();
        link_config.authored_url_policy.allow_relative = false;
        let mut sink = LintDiagnosticSink::new(&link_config);
        let mut visited = Vec::new();
        super::references::lint_links_and_references_with_observer(
            &links.ast,
            &link_config.authored_url_policy,
            &mut sink,
            |node| {
                visited.push(node_kind(node));
            },
        );
        assert_eq!(sink.finish().len(), 1);
        assert_eq!(visited.last(), Some(&"inline-link"));
        assert!(visited.len() < full_walk_len(&links.ast));
    }

    #[test]
    fn diagnostic_sink_applies_every_severity_and_canonical_order() {
        let rules = [
            (LINE_TOO_LONG, Severity::Error),
            (INVALID_ANCHOR, Severity::Warning),
            (INVALID_TABLE, Severity::Information),
            (INVALID_CATALOG, Severity::Hint),
        ];
        let mut config = LintConfig::default();
        for (rule, severity) in rules {
            config.set_rule(
                rule,
                RuleSettings {
                    enabled: true,
                    severity,
                },
            );
        }
        let mut sink = LintDiagnosticSink::new(&config);
        for (index, (rule, _)) in rules.into_iter().enumerate().rev() {
            let range = text_range(index * 2, index * 2 + 1).expect("range");
            sink.emit(rule, range, || LintDiagnosticBody::new("message"));
        }
        let diagnostics = sink.finish();
        let mut forward = LintDiagnosticSink::new(&config);
        for (index, (rule, _)) in rules.into_iter().enumerate() {
            let range = text_range(index * 2, index * 2 + 1).expect("range");
            forward.emit(rule, range, || LintDiagnosticBody::new("message"));
        }
        assert_eq!(diagnostics, forward.finish());

        assert_eq!(
            diagnostics
                .iter()
                .map(|diagnostic| diagnostic.severity)
                .collect::<Vec<_>>(),
            [
                Severity::Error,
                Severity::Warning,
                Severity::Information,
                Severity::Hint
            ]
        );
        for (index, diagnostic) in diagnostics.iter().enumerate() {
            assert_eq!(
                diagnostic.id.as_str(),
                format!(
                    "{}@{}:{}",
                    rules[index].0.as_str(),
                    index * 2,
                    index * 2 + 1
                )
            );
        }
    }

    #[test]
    fn diagnostic_sink_preserves_related_information_and_fix_applicability() {
        let mut config = LintConfig::default();
        config.set_rule(
            MACRO_BOUNDARY,
            RuleSettings {
                enabled: true,
                severity: Severity::Warning,
            },
        );
        let mut sink = LintDiagnosticSink::new(&config);
        let first = text_range(0, 1).expect("first");
        let fix_range = text_range(4, 4).expect("fix");
        let second_fix_range = text_range(6, 6).expect("second fix");
        let duplicate = text_range(8, 9).expect("duplicate");

        sink.emit(DUPLICATE_ANCHOR, duplicate, || {
            LintDiagnosticBody::new("duplicate").with_related(vec![RelatedInformation {
                message: "first".to_owned(),
                range: first,
            }])
        });
        sink.emit(MACRO_BOUNDARY, fix_range, || {
            LintDiagnosticBody::new("boundary")
                .with_fix(
                    "insert",
                    Applicability::Maybe,
                    vec![
                        TextEdit {
                            range: fix_range,
                            replacement: " ".to_owned(),
                        },
                        TextEdit {
                            range: second_fix_range,
                            replacement: " ".to_owned(),
                        },
                    ],
                )
                .with_edit_fix("alternative", fix_range, "_", Applicability::Always)
        });
        let diagnostics = sink.finish();

        assert_eq!(diagnostics[0].code.as_str(), "macro-boundary");
        assert_eq!(diagnostics[0].fixes.len(), 2);
        assert_eq!(diagnostics[0].fixes[0].title, "insert");
        assert_eq!(diagnostics[0].fixes[0].applicability, Applicability::Maybe);
        assert_eq!(diagnostics[0].fixes[0].edits().len(), 2);
        assert_eq!(diagnostics[0].fixes[0].edits()[0].range, fix_range);
        assert_eq!(diagnostics[0].fixes[0].edits()[0].replacement, " ");
        assert_eq!(diagnostics[0].fixes[0].edits()[1].range, second_fix_range);
        assert_eq!(diagnostics[0].fixes[1].title, "alternative");
        assert_eq!(diagnostics[0].fixes[1].applicability, Applicability::Always);
        assert_eq!(diagnostics[1].code.as_str(), "duplicate-anchor");
        assert_eq!(diagnostics[1].related.len(), 1);
        assert_eq!(diagnostics[1].related[0].message, "first");
        assert_eq!(diagnostics[1].related[0].range, first);
    }

    /// Debug builds still stop at the defect, so it is found while developing.
    #[test]
    #[cfg(debug_assertions)]
    #[should_panic(expected = "non-fixable lint rule emitted a fix: line-too-long")]
    fn diagnostic_sink_rejects_fixes_from_non_fixable_rules() {
        let config = LintConfig::default();
        let mut sink = LintDiagnosticSink::new(&config);
        let range = text_range(0, 1).expect("range");

        sink.emit(LINE_TOO_LONG, range, || {
            LintDiagnosticBody::new("too long").with_edit_fix(
                "invalid",
                range,
                "",
                Applicability::Always,
            )
        });
    }

    #[test]
    #[cfg(debug_assertions)]
    #[should_panic(expected = "lint diagnostic rule is not registered")]
    fn diagnostic_sink_rejects_rules_missing_from_the_catalog() {
        let rule = LintRuleId("missing-from-catalog");
        let mut config = LintConfig::default();
        config.set_rule(
            rule,
            RuleSettings {
                enabled: true,
                severity: Severity::Warning,
            },
        );
        let mut sink = LintDiagnosticSink::new(&config);
        let range = text_range(0, 1).expect("range");

        sink.emit(rule, range, || LintDiagnosticBody::new("invalid rule"));
    }

    #[test]
    fn formerly_direct_diagnostics_use_shared_rule_settings() {
        for (rule, source) in [
            (
                DUPLICATE_ANCHOR,
                "[[same]]\n== First\n\n[[same]]\n== Second\n",
            ),
            (DUPLICATE_HEADING_ID, "== Same\n\n== Same\n"),
        ] {
            let mut config = LintConfig::default();
            config.set_rule(
                rule,
                RuleSettings {
                    enabled: true,
                    severity: Severity::Information,
                },
            );
            let diagnostics = lint(source, &config).expect("lint");
            let diagnostic = diagnostics
                .iter()
                .find(|diagnostic| diagnostic.code.as_str() == rule.as_str())
                .expect("configured diagnostic");
            assert_eq!(diagnostic.severity, Severity::Information);
            assert_eq!(diagnostic.related.len(), 1);

            config.set_rule(
                rule,
                RuleSettings {
                    enabled: false,
                    severity: Severity::Error,
                },
            );
            assert!(
                lint(source, &config)
                    .expect("lint")
                    .iter()
                    .all(|diagnostic| diagnostic.code.as_str() != rule.as_str())
            );
        }

        let mut config = LintConfig::default();
        config
            .protected_attributes
            .insert("locked".to_owned(), Some("expected".to_owned()));
        config.set_rule(
            PROTECTED_ATTRIBUTE,
            RuleSettings {
                enabled: true,
                severity: Severity::Hint,
            },
        );
        let diagnostics = lint(":locked: changed\n", &config).expect("lint");
        let protected = diagnostics
            .iter()
            .find(|diagnostic| diagnostic.code.as_str() == PROTECTED_ATTRIBUTE.as_str())
            .expect("protected attribute diagnostic");
        assert_eq!(protected.severity, Severity::Hint);
    }

    #[test]
    fn lint_reports_trailing_whitespace_with_safe_fix() {
        let diagnostics = lint("text \t\r\n", &LintConfig::default()).expect("valid source");

        assert_eq!(diagnostics.len(), 1);
        assert_eq!(diagnostics[0].code.as_str(), "trailing-whitespace");
        assert_eq!(diagnostics[0].range.start().to_u32(), 4);
        assert_eq!(diagnostics[0].range.end().to_u32(), 6);
        assert_eq!(diagnostics[0].fixes[0].edits()[0].replacement, "");
    }

    #[test]
    fn lint_reports_only_blank_lines_beyond_configured_limit() {
        let config = LintConfig {
            max_consecutive_blank_lines: 1,
            ..LintConfig::default()
        };
        let diagnostics = lint("first\n\n\nlast\n", &config).expect("valid source");

        assert_eq!(
            diagnostics
                .iter()
                .map(|diagnostic| diagnostic.code.as_str())
                .collect::<Vec<_>>(),
            ["excessive-blank-lines"]
        );
        assert_eq!(diagnostics[0].fixes[0].edits()[0].replacement, "");
    }

    #[test]
    fn lint_counts_unicode_scalars_for_line_length() {
        let config = LintConfig {
            max_line_length: 3,
            ..LintConfig::default()
        };
        let diagnostics = lint("日本語😀\n", &config).expect("valid source");

        assert_eq!(diagnostics.len(), 1);
        assert_eq!(diagnostics[0].code.as_str(), "line-too-long");
        assert_eq!(diagnostics[0].range.start().to_u32(), 9);
    }

    #[test]
    fn lint_rules_can_be_disabled_and_change_severity() {
        let mut config = LintConfig::default();
        config.set_rule(
            TRAILING_WHITESPACE,
            RuleSettings {
                enabled: false,
                severity: Severity::Error,
            },
        );
        config.set_rule(
            LINE_TOO_LONG,
            RuleSettings {
                enabled: true,
                severity: Severity::Error,
            },
        );
        config.max_line_length = 1;
        let diagnostics = lint("long \n", &config).expect("valid source");

        assert_eq!(diagnostics.len(), 1);
        assert_eq!(diagnostics[0].code.as_str(), "line-too-long");
        assert_eq!(diagnostics[0].severity, Severity::Error);
    }

    #[test]
    fn lint_matches_basic_fixture() {
        let source = include_str!("../../../fixtures/lint/basic.adoc");
        let diagnostics = lint(source, &LintConfig::default()).expect("valid source");

        assert_eq!(diagnostics.len(), 2);
        assert_eq!(diagnostics[0].code.as_str(), "trailing-whitespace");
        assert_eq!(diagnostics[1].code.as_str(), "line-too-long");
    }

    #[test]
    fn list_presentation_diagnostics_use_lowered_attribute_problems() {
        let diagnostics = lint("[start=0,style=unknown]\n. item\n", &LintConfig::default())
            .expect("valid source");
        let messages = diagnostics
            .iter()
            .filter(|diagnostic| diagnostic.code.as_str() == "invalid-list-presentation")
            .map(|diagnostic| diagnostic.message.as_str())
            .collect::<Vec<_>>();

        assert_eq!(
            messages,
            [
                "ordered list start must be a positive integer",
                "unsupported ordered list style"
            ]
        );
    }

    #[test]
    fn invalid_toclevels_uses_the_resolved_attribute_range() {
        let diagnostics =
            lint("= Title\n:toclevels: 0\n", &LintConfig::default()).expect("valid source");

        assert!(diagnostics.iter().any(|diagnostic| {
            diagnostic.code.as_str() == "invalid-attribute"
                && diagnostic.message == "toclevels must be an integer from 1 to 5"
        }));
    }

    #[test]
    fn explicit_ordered_numbers_must_be_sequential() {
        let diagnostics = lint("4. four\n6. six\n", &LintConfig::default()).expect("valid source");

        assert!(diagnostics.iter().any(|diagnostic| {
            diagnostic.code.as_str() == "invalid-list-presentation"
                && diagnostic.message == "explicit ordered-list numbers must be sequential"
        }));
    }

    #[test]
    fn invalid_explicit_ordered_numbers_have_stable_diagnostics() {
        let diagnostics =
            lint("4294967296. overflow\n0. zero\n", &LintConfig::default()).expect("valid source");

        let invalid = diagnostics
            .iter()
            .filter(|diagnostic| {
                diagnostic.code.as_str() == "invalid-list-presentation"
                    && diagnostic.message
                        == "explicit ordered-list number must be a positive 32-bit integer"
            })
            .collect::<Vec<_>>();
        assert_eq!(invalid.len(), 2);
        assert_eq!(invalid[0].range.start().to_u32(), 0);
        assert_eq!(invalid[0].range.end().to_u32(), 11);
    }

    #[test]
    fn heading_lint_reports_hierarchy_duplicates_and_missing_space() {
        let source = "= Title\n\n=== Too deep\n\n==Same\n\n== Same\n";
        let diagnostics = lint(source, &LintConfig::default()).expect("valid source");
        let codes = diagnostics
            .iter()
            .map(|diagnostic| diagnostic.code.as_str())
            .collect::<Vec<_>>();

        assert!(codes.contains(&"invalid-heading-level"));
        assert!(codes.contains(&"heading-marker-space"));
        assert!(codes.contains(&"duplicate-heading-id"));
        let spacing = diagnostics
            .iter()
            .find(|diagnostic| diagnostic.code.as_str() == "heading-marker-space")
            .expect("spacing diagnostic");
        assert_eq!(spacing.fixes[0].edits()[0].replacement, " ");
    }

    #[test]
    fn document_structure_lint_reports_doctype_specific_failures() {
        let source = "[bibliography]\n= tool(1)\n:doctype: manpage\n\n= Not a book part\n\n[appendix]\n=== Bad appendix\n";
        let diagnostics = lint(source, &LintConfig::default()).expect("valid source");
        let messages = diagnostics
            .iter()
            .filter(|diagnostic| diagnostic.code.as_str() == "invalid-document-structure")
            .map(|diagnostic| diagnostic.message.as_str())
            .collect::<Vec<_>>();

        assert!(messages.contains(&"appendix must be a level-one section"));
        assert!(messages.contains(&"appendix is only valid for article or book documents"));
        assert!(
            messages.contains(
                &"bibliography must be a section, not a document title or discrete heading"
            )
        );
        assert!(messages.contains(&"bibliography is only valid for article or book documents"));
        assert!(messages.contains(&"manpage NAME section is missing"));
    }

    #[test]
    fn bibliography_style_requires_a_structural_section() {
        let diagnostics = lint(
            "= Title\n\n[discrete,bibliography]\n=== References\n",
            &LintConfig::default(),
        )
        .expect("valid source");

        assert!(diagnostics.iter().any(|diagnostic| {
            diagnostic.code.as_str() == "invalid-document-structure"
                && diagnostic.message
                    == "bibliography must be a section, not a document title or discrete heading"
        }));
    }

    #[test]
    fn bibliography_scope_accepts_article_book_part_and_nested_section() {
        for source in [
            "= Title\n\n[bibliography]\n== References\n",
            "= Book\n:doctype: book\n\n[bibliography]\n== References\n",
            "= Book\n:doctype: book\n\n= Part\n\n== Chapter\n\n[bibliography]\n== References\n",
            "= Book\n:doctype: book\n\n= Part\n\n== Chapter\n\n[bibliography]\n= References\n",
            "= Title\n:doctype: manpage\n\n== NAME\n\ntool - purpose\n\n=== Parent\n\n[bibliography]\n==== References\n",
        ] {
            let diagnostics = lint(source, &LintConfig::default()).expect("valid source");
            assert!(
                !diagnostics.iter().any(|diagnostic| {
                    diagnostic.code.as_str() == "invalid-document-structure"
                        && diagnostic.message.contains("bibliography")
                }),
                "{source}"
            );
        }
    }

    #[test]
    fn bibliography_scope_requires_a_multipart_book_for_level_zero() {
        let diagnostics = lint(
            "= Book\n:doctype: book\n\n[bibliography]\n= References\n",
            &LintConfig::default(),
        )
        .expect("valid source");

        assert!(diagnostics.iter().any(|diagnostic| {
            diagnostic.code.as_str() == "invalid-document-structure"
                && diagnostic.message
                    == "whole-book bibliography must be a level-zero section in a multipart book"
        }));
    }

    #[test]
    fn monospace_lint_reports_unclosed_span() {
        let diagnostics = lint("before `open\nnext", &LintConfig::default()).expect("valid source");

        assert!(
            diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code.as_str() == "unclosed-inline")
        );
    }

    #[test]
    fn table_lint_reports_an_unclosed_quoted_header_candidate() {
        let source = "[format=csv]\n|===\nname,\"open\n\ncontinued\n|===\n";
        let diagnostics = lint(source, &LintConfig::default()).expect("valid source");

        assert!(diagnostics.iter().any(|diagnostic| {
            diagnostic.code.as_str() == "invalid-table"
                && diagnostic.message == "unclosed quoted table cell"
        }));
    }

    #[test]
    fn strong_lint_reports_unclosed_span() {
        let diagnostics = lint("*open text", &LintConfig::default()).expect("valid source");

        assert!(diagnostics.iter().any(|diagnostic| {
            diagnostic.code.as_str() == "unclosed-inline" && diagnostic.message.contains("strong")
        }));
    }

    #[test]
    fn emphasis_lint_reports_unclosed_span() {
        let diagnostics = lint("_open", &LintConfig::default()).expect("valid source");

        assert!(diagnostics.iter().any(|diagnostic| {
            diagnostic.code.as_str() == "unclosed-inline" && diagnostic.message.contains("emphasis")
        }));
    }

    #[test]
    fn inline_recovery_uses_dedicated_nesting_limit_code() {
        let diagnostics = lint_with_analysis_limits(
            "*nested*",
            &LintConfig::default(),
            crate::limits::AnalysisLimits {
                max_inline_depth: 0,
                ..crate::limits::AnalysisLimits::default()
            },
        )
        .expect("valid source");

        assert_eq!(diagnostics[0].code.as_str(), "nesting-limit-exceeded");
    }

    #[test]
    fn literal_block_lint_reports_unclosed_block() {
        let diagnostics = lint("....\ncontent", &LintConfig::default()).expect("valid source");

        assert_eq!(diagnostics.len(), 1);
        assert_eq!(diagnostics[0].code.as_str(), "unclosed-block");
        assert_eq!(diagnostics[0].range.start().to_u32(), 0);
    }

    #[test]
    fn source_block_lint_reports_missing_language() {
        let diagnostics =
            lint("[source]\n----\ncode\n----\n", &LintConfig::default()).expect("valid source");

        assert_eq!(diagnostics.len(), 1);
        assert_eq!(diagnostics[0].code.as_str(), "missing-source-language");
        assert_eq!(diagnostics[0].range.start().to_u32(), 0);
        assert_eq!(diagnostics[0].range.end().to_u32(), 8);
    }

    #[test]
    fn document_attributes_allow_redefinition_and_report_dataflow_problems() {
        let mut config = LintConfig::default();
        config.set_rule(
            UNUSED_ATTRIBUTE,
            RuleSettings {
                enabled: true,
                severity: Severity::Warning,
            },
        );
        let diagnostics = lint(
            "= Note\n\
             :bad name: value\n\
             :unused: value\n\
             :name: first\n\
             :name: second\n\
             \n\
             {name} {missing}\n",
            &config,
        )
        .expect("lint");
        let codes = diagnostics
            .iter()
            .map(|diagnostic| diagnostic.code.as_str())
            .collect::<Vec<_>>();
        assert!(codes.contains(&"invalid-attribute"));
        assert!(!codes.contains(&"duplicate-attribute"));
        assert!(codes.contains(&"undefined-attribute"));
        assert!(codes.contains(&"unused-attribute"));
    }

    #[test]
    fn attribute_lint_selects_bindings_at_each_reference_position() {
        let mut config = LintConfig::default();
        config.set_rule(
            UNUSED_ATTRIBUTE,
            RuleSettings {
                enabled: true,
                severity: Severity::Warning,
            },
        );
        let source = "\
:a: first

{a}

:a: second

{a}

:a!:

{a}

:future: {later}
:later: value
";
        let diagnostics = lint(source, &config).expect("lint");
        assert!(
            diagnostics
                .iter()
                .all(|diagnostic| diagnostic.code.as_str() != "duplicate-attribute")
        );

        let unset_reference = source.rfind("{a}").expect("unset reference") + 1;
        let future_reference = source.find("{later}").expect("future reference") + 1;
        let undefined = diagnostics
            .iter()
            .filter(|diagnostic| diagnostic.code.as_str() == "undefined-attribute")
            .collect::<Vec<_>>();
        assert_eq!(undefined.len(), 2);
        assert_eq!(
            undefined
                .iter()
                .map(|diagnostic| diagnostic.range.start().to_usize())
                .collect::<Vec<_>>(),
            [unset_reference, future_reference]
        );
        for diagnostic in undefined {
            assert_eq!(
                diagnostic.id.as_str(),
                format!(
                    "undefined-attribute@{}:{}",
                    diagnostic.range.start().to_u32(),
                    diagnostic.range.end().to_u32()
                )
            );
            assert_eq!(diagnostic.severity, Severity::Warning);
            assert!(diagnostic.fixes.is_empty());
        }

        let unused = diagnostics
            .iter()
            .filter(|diagnostic| diagnostic.code.as_str() == "unused-attribute")
            .collect::<Vec<_>>();
        assert_eq!(unused.len(), 2);
        assert_eq!(
            unused
                .iter()
                .map(|diagnostic| {
                    &source[diagnostic.range.start().to_usize()..diagnostic.range.end().to_usize()]
                })
                .collect::<Vec<_>>(),
            ["future", "later"]
        );
    }

    #[test]
    fn attribute_lint_reports_cycle_depth_and_size_at_the_failing_binding_reference() {
        let source = "\
:cycle: {cycle}
:base: x
:deep: {base}
:large: xx

{cycle} {deep} {large}
";
        let limits = crate::limits::AnalysisLimits {
            max_attribute_expansion_depth: 0,
            max_attribute_expansion_bytes: 1,
            ..crate::limits::AnalysisLimits::default()
        };
        let diagnostics =
            lint_with_analysis_limits(source, &LintConfig::default(), limits).expect("lint");
        let expansion = diagnostics
            .iter()
            .filter(|diagnostic| diagnostic.code.as_str() == "attribute-expansion")
            .collect::<Vec<_>>();

        assert!(expansion.iter().any(|diagnostic| {
            diagnostic.message == "document attribute expansion contains a cycle"
                && &source[diagnostic.range.start().to_usize()..diagnostic.range.end().to_usize()]
                    == "cycle"
        }));
        assert!(expansion.iter().any(|diagnostic| {
            diagnostic.message == "document attribute expansion exceeds the depth limit"
                && &source[diagnostic.range.start().to_usize()..diagnostic.range.end().to_usize()]
                    == "base"
        }));
        assert!(expansion.iter().any(|diagnostic| {
            diagnostic.message == "document attribute expansion exceeds the size limit"
                && &source[diagnostic.range.start().to_usize()..diagnostic.range.end().to_usize()]
                    == "xx"
        }));
        assert!(
            expansion
                .iter()
                .all(|diagnostic| diagnostic.fixes.is_empty())
        );
    }

    #[test]
    fn anchors_report_invalid_unattached_and_duplicate_ids() {
        let diagnostics = lint(
            "[[same]]\n== One\n\n[[same]]\n== Two\n\n[[bad id]]\nParagraph\n\n[[orphan]]\n",
            &LintConfig::default(),
        )
        .expect("lint");
        let codes = diagnostics
            .iter()
            .map(|diagnostic| diagnostic.code.as_str())
            .collect::<Vec<_>>();

        assert!(codes.contains(&"duplicate-anchor"));
        assert!(
            codes
                .iter()
                .filter(|code| **code == "invalid-anchor")
                .count()
                >= 2
        );
    }

    #[test]
    fn lint_cst_reuses_analysis_without_changing_diagnostics() {
        let source = "= Note\n:name: value\n\n{name}  \n";
        let parsed = crate::parser::parse(source).expect("parse");
        let config = LintConfig::default();

        assert_eq!(
            lint(source, &config).expect("standalone lint"),
            super::lint_parsed_document(
                super::LintContext::new(&parsed.syntax, &parsed.ast),
                &config,
            )
            .expect("lint existing analysis")
        );
    }

    #[test]
    fn links_and_url_policy_reject_dangerous_schemes() {
        let source = include_str!("../../../fixtures/links/security.adoc");
        let diagnostics = lint(source, &LintConfig::default()).expect("lint");
        let codes = diagnostics
            .iter()
            .map(|diagnostic| diagnostic.code.as_str())
            .collect::<Vec<_>>();

        assert_eq!(
            codes
                .iter()
                .filter(|code| **code == "invalid-url-scheme")
                .count(),
            2
        );
        assert_eq!(
            codes
                .iter()
                .filter(|code| **code == "invalid-cross-reference")
                .count(),
            1
        );
        assert!(codes.contains(&"unresolved-cross-reference"));
    }

    #[test]
    fn link_and_xref_rules_use_extensions_without_filesystem_access() {
        let source = "= Title\n:doc: guide\n:asset: data\n\n\
            link:guide.adoc[guide]\n\
            link:GUIDE.ASCIIDOC?view=1#top[guide]\n\
            link:guide.adoc?next=https://example.test[query]\n\
            link:bad%ZZ.adoc[invalid escape]\n\
            link:https://example.com/guide.adoc[external]\n\
            link:{doc}.adoc[attribute]\n\
            link:{missing}.adoc[missing]\n\
            link:/root/manual.adoc[root]\n\
            link:.adoc[hidden]\n\
            link:empty.[empty]\n\
            xref:data.json?download=1#top[data]\n\
            xref:manual.PDF[pdf]\n\
            xref:{asset}.toml[attribute]\n\
            xref:{missing}.pdf[missing]\n\
            xref:/root/data.json[root]\n\
            xref:README[extensionless]\n\
            xref:guide.ADOC[guide]\n\
            xref:note:asset.pdf[scheme]\n\
            <<local>>\n";
        let diagnostics = lint(source, &LintConfig::default()).expect("lint");
        let relevant = diagnostics
            .iter()
            .filter(|diagnostic| {
                matches!(
                    diagnostic.code.as_str(),
                    "asciidoc-file-link" | "non-asciidoc-xref"
                )
            })
            .collect::<Vec<_>>();

        assert_eq!(
            relevant.len(),
            10,
            "{:?}",
            relevant
                .iter()
                .map(|diagnostic| (
                    diagnostic.code.as_str(),
                    &source[diagnostic.range.start().to_usize()..diagnostic.range.end().to_usize()]
                ))
                .collect::<Vec<_>>()
        );
        assert_eq!(
            relevant
                .iter()
                .map(|diagnostic| diagnostic.code.as_str())
                .collect::<Vec<_>>(),
            vec![
                "asciidoc-file-link",
                "asciidoc-file-link",
                "asciidoc-file-link",
                "asciidoc-file-link",
                "asciidoc-file-link",
                "asciidoc-file-link",
                "non-asciidoc-xref",
                "non-asciidoc-xref",
                "non-asciidoc-xref",
                "non-asciidoc-xref"
            ]
        );
        for diagnostic in relevant {
            let macro_name =
                &source[diagnostic.range.start().to_usize()..diagnostic.range.end().to_usize()];
            let (expected_name, expected_replacement) =
                if diagnostic.code.as_str() == "asciidoc-file-link" {
                    ("link", "xref")
                } else {
                    ("xref", "link")
                };
            assert_eq!(macro_name, expected_name);
            assert_eq!(diagnostic.severity, Severity::Warning);
            let target_line = source[diagnostic.range.end().to_usize()..]
                .lines()
                .next()
                .expect("diagnostic line");
            if target_line.starts_with(":{")
                || target_line.starts_with(":/")
                || target_line.contains("https:")
                || target_line.contains("%ZZ")
            {
                assert!(diagnostic.fixes.is_empty());
            } else {
                assert_eq!(diagnostic.fixes.len(), 1);
                assert_eq!(
                    diagnostic.fixes[0].applicability,
                    super::Applicability::Always
                );
                assert_eq!(diagnostic.fixes[0].edits()[0].range, diagnostic.range);
                assert_eq!(
                    diagnostic.fixes[0].edits()[0].replacement,
                    expected_replacement
                );
            }
        }
    }

    #[test]
    fn macro_boundary_rule_is_opt_in_and_uses_recognized_complete_macros() {
        let source = include_str!("../../../fixtures/lint/macro-boundary.adoc");
        let default = lint(source, &LintConfig::default()).expect("lint");
        assert!(
            default
                .iter()
                .all(|diagnostic| diagnostic.code.as_str() != "macro-boundary")
        );

        let mut config = LintConfig::default();
        config.set_rule(
            MACRO_BOUNDARY,
            RuleSettings {
                enabled: true,
                severity: Severity::Warning,
            },
        );
        let diagnostics = lint(source, &config).expect("lint");
        let boundary = diagnostics
            .iter()
            .filter(|diagnostic| diagnostic.code.as_str() == "macro-boundary")
            .collect::<Vec<_>>();
        assert_eq!(boundary.len(), 23, "{boundary:#?}");
        assert_eq!(
            boundary
                .iter()
                .map(|diagnostic| {
                    &source[diagnostic.range.start().to_usize()..diagnostic.range.end().to_usize()]
                })
                .collect::<Vec<_>>(),
            [
                "xref",
                "link",
                "image",
                "footnote",
                "anchor",
                "bibanchor",
                "indexterm",
                "kbd",
                "btn",
                "menu",
                "icon",
                "audio",
                "video",
                "stem",
                "latexmath",
                "pass",
                "https",
                "user@example.test",
                "user@example.test",
                "user@example.test",
                "user@example.test",
                "xref",
                "https"
            ]
        );
        assert!(
            boundary
                .iter()
                .enumerate()
                .all(|(index, diagnostic)| if index == 21 {
                    diagnostic.fixes.len() == 1
                        && diagnostic.fixes[0].applicability == super::Applicability::Maybe
                        && diagnostic.fixes[0].edits()[0].replacement == " "
                } else {
                    diagnostic.fixes.is_empty()
                })
        );
    }

    #[test]
    fn macro_boundary_rule_honors_severity_and_diagnostic_limit() {
        let mut config = LintConfig {
            max_diagnostics: 1,
            ..LintConfig::default()
        };
        config.set_rule(
            MACRO_BOUNDARY,
            RuleSettings {
                enabled: true,
                severity: Severity::Error,
            },
        );
        let diagnostics =
            lint("本文xref:one.adoc[]\n本文link:two.json[]\n", &config).expect("lint");

        assert_eq!(diagnostics.len(), 1);
        assert_eq!(diagnostics[0].code.as_str(), "macro-boundary");
        assert_eq!(diagnostics[0].severity, Severity::Error);
    }

    #[test]
    fn multiline_list_principal_text_keeps_diagnostic_ranges_on_continuation_lines() {
        let source = "* first\n本文xref:target[]\n";
        let mut config = LintConfig::default();
        config.set_rule(
            MACRO_BOUNDARY,
            RuleSettings {
                enabled: true,
                severity: Severity::Warning,
            },
        );

        let diagnostics = lint(source, &config).expect("lint");
        let diagnostic = diagnostics
            .iter()
            .find(|diagnostic| diagnostic.code.as_str() == "macro-boundary")
            .expect("macro boundary diagnostic");

        assert_eq!(
            &source[diagnostic.range.start().to_usize()..diagnostic.range.end().to_usize()],
            "xref"
        );
    }

    #[test]
    fn expanded_xref_target_does_not_keep_the_authored_safety_diagnostic() {
        let diagnostics = lint(
            "= Title\n:asset: data\n\nxref:{asset}.toml[Data]\n",
            &LintConfig::default(),
        )
        .expect("lint");
        let codes = diagnostics
            .iter()
            .map(|diagnostic| diagnostic.code.as_str())
            .collect::<Vec<_>>();

        assert!(codes.contains(&"non-asciidoc-xref"), "{codes:?}");
        assert!(!codes.contains(&"invalid-cross-reference"));
        assert!(!codes.contains(&"unused-attribute"));
    }

    #[test]
    fn relative_links_and_cross_document_targets_do_not_require_host_resolution() {
        let diagnostics = lint(
            "link:../release-manifest.json[release manifest]\n\
             link:../%2e%2e/secret[encoded relative]\n\
             xref:../guide.adoc[guide]\n",
            &LintConfig::default(),
        )
        .expect("lint");

        assert!(!diagnostics.iter().any(|diagnostic| {
            matches!(
                diagnostic.code.as_str(),
                "invalid-url-scheme" | "invalid-cross-reference"
            )
        }));
    }

    #[test]
    fn relative_target_validation_remains_lexically_bounded() {
        for (source, expected_code) in [
            (
                "link://example.com/path[network path]",
                "invalid-url-scheme",
            ),
            ("link:../line%0afeed[encoded control]", "invalid-url-scheme"),
            ("link:javascript:alert(1)[scheme]", "invalid-url-scheme"),
            ("xref:/absolute.adoc[absolute]", "invalid-cross-reference"),
            (
                "xref:..\\\\secret.adoc[backslash]",
                "invalid-cross-reference",
            ),
        ] {
            let diagnostics = lint(source, &LintConfig::default()).expect("lint");
            assert!(
                diagnostics
                    .iter()
                    .any(|diagnostic| diagnostic.code.as_str() == expected_code),
                "missing {expected_code} diagnostic for {source}"
            );
        }
    }

    #[test]
    fn url_policy_checks_the_semantically_expanded_link_target() {
        let source = ":scheme: https\n\n{scheme}://example.com[label]\n";
        let parsed = crate::parser::parse(source).expect("parse");
        let diagnostics = super::lint_parsed_document(
            super::LintContext::new(&parsed.syntax, &parsed.ast),
            &LintConfig::default(),
        )
        .expect("lint");

        assert!(
            !diagnostics
                .iter()
                .any(|diagnostic| { diagnostic.code.as_str() == "invalid-url-scheme" })
        );
    }

    #[test]
    fn forward_attribute_references_are_not_rebound_later() {
        let diagnostics =
            lint("= T\n:a: {b}\n:b: {a}\n\n{a}", &LintConfig::default()).expect("lint");
        assert!(
            !diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code.as_str() == "attribute-expansion")
        );
    }

    #[test]
    fn cross_references_resolve_local_targets_but_leave_documents_for_hosts() {
        let diagnostics = lint(
            "[[target]]\n== Target\n\n\
             <<target>> xref:#target[] xref:other.adoc#part[] xref:../guide.adoc[]",
            &LintConfig::default(),
        )
        .expect("lint");

        assert!(!diagnostics.iter().any(|diagnostic| {
            matches!(
                diagnostic.code.as_str(),
                "invalid-cross-reference" | "unresolved-cross-reference"
            )
        }));
    }

    #[test]
    fn lists_report_structure_and_offer_a_safe_separator_fix() {
        let diagnostics =
            lint("*\titem\n*** skipped\n. changed\n", &LintConfig::default()).expect("lint");
        let list_diagnostics = diagnostics
            .iter()
            .filter(|diagnostic| diagnostic.code.as_str() == "inconsistent-list")
            .collect::<Vec<_>>();

        assert!(list_diagnostics.len() >= 3);
        assert!(list_diagnostics.iter().any(|diagnostic| {
            diagnostic
                .fixes
                .iter()
                .any(|fix| fix.edits()[0].replacement == " ")
        }));
    }

    #[test]
    fn unknown_reference_schemes_have_no_note_specific_semantics_by_default() {
        let diagnostics =
            lint("xref:note:not-a-uuid[label]", &LintConfig::default()).expect("lint");

        assert!(
            diagnostics
                .iter()
                .all(|diagnostic| diagnostic.code.as_str() != "invalid-note-uuid")
        );
    }

    #[test]
    fn note_reference_incomplete_fixture_recovers_without_panicking() {
        let source = include_str!("../../../fixtures/references/incomplete-note.adoc");
        let parsed = crate::parser::parse(source).expect("parse");

        assert_eq!(parsed.ast.blocks().len(), 1);
    }

    #[test]
    fn stem_recovery_reports_empty_and_unclosed_formulas() {
        let diagnostics = lint(
            "stem:[] and stem:[open\n\n[stem]\n++++\n++++\n",
            &LintConfig::default(),
        )
        .expect("lint");

        assert_eq!(
            diagnostics
                .iter()
                .filter(|diagnostic| diagnostic.code.as_str() == "invalid-stem")
                .count(),
            3
        );
    }

    #[test]
    fn stem_size_limit_is_reported_without_evaluating_the_formula() {
        let source = format!(
            "stem:[{}]",
            "x".repeat(
                usize::try_from(crate::limits::AnalysisLimits::default().max_formula_bytes)
                    .expect("u32 fits usize")
                    + 1
            )
        );
        let diagnostics = lint(&source, &LintConfig::default()).expect("lint");

        assert!(diagnostics.iter().any(|diagnostic| {
            diagnostic.code.as_str() == "invalid-stem" && diagnostic.message.contains("size limit")
        }));
    }

    #[test]
    fn invalid_table_format_separator_and_quote_have_stable_diagnostics() {
        for source in [
            "[format=unknown]\n|===\n|cell\n|===\n",
            "[format=csv,separator=too-long]\n|===\na,b\n|===\n",
            "[format=csv]\n|===\na,\"open\n|===\n",
            "[separator=;]\n,===\na,b\n,===\n",
            "\0===\ncell\n\0===\n",
        ] {
            let diagnostics = lint(source, &LintConfig::default()).expect("lint");
            assert!(
                diagnostics
                    .iter()
                    .any(|diagnostic| { diagnostic.code.as_str() == "invalid-table" })
            );
        }
    }

    #[test]
    fn table_presentation_diagnostics_cover_invalid_duplicate_and_conflicting_values() {
        let diagnostics = lint(
            ".Caption\n[frame=ends,frame=sides,grid=diagonal,stripes=even,width=75%,options=autowidth]\n|===\n|cell\n|===\n",
            &LintConfig::default(),
        )
        .expect("lint");
        let invalid = diagnostics
            .iter()
            .filter(|diagnostic| diagnostic.code.as_str() == "invalid-table")
            .collect::<Vec<_>>();
        assert_eq!(invalid.len(), 3);
        assert!(invalid.iter().all(|diagnostic| {
            diagnostic.message == "invalid or conflicting table presentation attribute"
        }));
    }

    #[test]
    fn table_presentation_width_rejects_signs_zero_and_out_of_range_values() {
        for width in ["+75%", "0", "101", "75px", "%"] {
            let diagnostics = lint(
                &format!("[width={width}]\n|===\n|cell\n|===\n"),
                &LintConfig::default(),
            )
            .expect("lint");
            assert!(diagnostics.iter().any(|diagnostic| {
                diagnostic.code.as_str() == "invalid-table"
                    && diagnostic.message == "invalid or conflicting table presentation attribute"
            }));
        }

        for width in ["1", "75", "100", "75%"] {
            let diagnostics = lint(
                &format!("[width={width}]\n|===\n|cell\n|===\n"),
                &LintConfig::default(),
            )
            .expect("lint");
            assert!(
                !diagnostics
                    .iter()
                    .any(|diagnostic| diagnostic.code.as_str() == "invalid-table")
            );
        }
    }

    #[test]
    fn prose_colons_are_not_automatic_urls() {
        let diagnostics =
            lint("TODO: text\nResult: value\n", &LintConfig::default()).expect("lint");
        assert!(
            !diagnostics
                .iter()
                .any(|diagnostic| { diagnostic.code.as_str() == "invalid-url-scheme" })
        );
    }

    #[test]
    fn catalog_diagnostics_preserve_duplicate_and_missing_ranges() {
        let mut config = LintConfig::default();
        config.set_rule(
            INVALID_CATALOG,
            RuleSettings {
                enabled: true,
                severity: Severity::Hint,
            },
        );
        let diagnostics = lint(
            "footnote:missing[] footnote:n[one] footnote:n[two] bibanchor:b[] bibanchor:b[] indexterm:[]",
            &config,
        )
        .expect("lint");
        let catalogs = diagnostics
            .iter()
            .filter(|diagnostic| diagnostic.code.as_str() == "invalid-catalog")
            .collect::<Vec<_>>();
        assert_eq!(catalogs.len(), 4);
        assert!(
            catalogs
                .iter()
                .all(|diagnostic| diagnostic.severity == Severity::Hint)
        );
        let related = catalogs
            .iter()
            .filter(|diagnostic| !diagnostic.related.is_empty())
            .collect::<Vec<_>>();
        assert_eq!(related.len(), 2);
        assert!(related.iter().all(|diagnostic| {
            diagnostic.related.len() == 1
                && diagnostic.related[0].message == "first definition is here"
                && diagnostic.related[0].range.start() < diagnostic.range.start()
        }));
    }
}
