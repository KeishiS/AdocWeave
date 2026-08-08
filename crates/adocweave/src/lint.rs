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
use crate::core::CancellationCheck;
#[cfg(test)]
use crate::core::NeverCancel;
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
    document: &'a crate::block_model::AstDocument,
}

impl<'a> LintContext<'a> {
    pub(crate) const fn new(
        syntax: &'a SyntaxTree,
        document: &'a crate::block_model::AstDocument,
    ) -> Self {
        Self { syntax, document }
    }

    const fn syntax(&self) -> &'a SyntaxTree {
        self.syntax
    }

    const fn document(&self) -> &'a crate::block_model::AstDocument {
        self.document
    }

    fn source_document(&self) -> &'a crate::source::SourceDocument {
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
        MONOSPACE_BOUNDARY,
        "monospace-boundary",
        "制約付き等幅表記の境界違反",
        false
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
        UNPROCESSED_DIRECTIVE,
        "unprocessed-directive",
        "前処理されていないpreprocessor directive",
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

/// Applies diagnostics to one analysis with cooperative cancellation.
pub fn lint_analysis(
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
mod tests;
