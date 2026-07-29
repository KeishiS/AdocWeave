//! Output-independent lint rules over the original source.

use std::collections::{BTreeMap, BTreeSet};
use std::ops::ControlFlow;

use crate::diagnostic::{
    Applicability, Diagnostic, DiagnosticCode, DiagnosticId, Fix, RelatedInformation, Severity,
    TextEdit, sort_diagnostics,
};
use crate::document::heading_id_base;
use crate::parser::{AstBlock, HeadingKind};
#[cfg(test)]
use crate::parser::{ParseConfig, parse_with_config};
use crate::source::{PositionError, TextRange, TextSize};
use crate::source_document::LineEnding;
use crate::syntax::{SyntaxIssueClass, SyntaxIssueDetail, SyntaxTree};

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
    fn new(config: &'a LintConfig) -> Self {
        Self {
            config,
            diagnostics: Vec::new(),
        }
    }

    fn is_full(&self) -> bool {
        self.diagnostics.len() >= self.config.max_diagnostics
    }

    fn emit(
        &mut self,
        rule: LintRuleId,
        range: TextRange,
        body: impl FnOnce() -> LintDiagnosticBody,
    ) {
        let descriptor = lint_rule(rule.as_str()).expect("lint diagnostic rule is registered");
        if self.is_full() {
            return;
        }
        let settings = self.config.rule(rule);
        if !settings.enabled {
            return;
        }
        let body = body();
        assert!(
            body.fixes.is_empty() || descriptor.fixable,
            "non-fixable lint rule emitted a fix: {}",
            rule.as_str()
        );
        let fixes = body
            .fixes
            .into_iter()
            .map(|fix| {
                Fix::new(fix.title, fix.applicability, fix.edits)
                    .expect("lint fix edits cannot conflict")
            })
            .collect();
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
    lint_syntax(&parsed.syntax, &parsed.ast, config)
}

pub fn lint_analysis(
    analysis: &crate::core::Analysis,
    config: &LintConfig,
) -> Result<Vec<Diagnostic>, PositionError> {
    lint_syntax(analysis.syntax(), analysis.ast(), config)
}

pub(crate) fn lint_syntax(
    syntax: &SyntaxTree,
    document: &crate::parser::AstDocument,
    config: &LintConfig,
) -> Result<Vec<Diagnostic>, PositionError> {
    let source_document = syntax.source_document();
    let mut sink = LintDiagnosticSink::new(config);
    let mut blank_count = 0;

    for line in source_document.lines() {
        if sink.is_full() {
            break;
        }
        let content = source_document
            .text(line.content_range())
            .expect("line ranges are valid");
        let is_virtual_final_line =
            line.full_range().is_empty() && line.ending() == LineEnding::None;
        let is_blank = content.trim_matches([' ', '\t']).is_empty();

        if is_blank && !is_virtual_final_line {
            blank_count += 1;
            if blank_count > config.max_consecutive_blank_lines {
                sink.emit(EXCESSIVE_BLANK_LINES, line.full_range(), || {
                    LintDiagnosticBody::new("excessive blank line").with_edit_fix(
                        "remove excessive blank line",
                        line.full_range(),
                        "",
                        Applicability::Always,
                    )
                });
            }
        } else {
            blank_count = 0;
        }

        let trimmed_end = content.trim_end_matches([' ', '\t']);
        if trimmed_end.len() != content.len() {
            let range = text_range(
                line.content_range().start().to_usize() + trimmed_end.len(),
                line.content_range().end().to_usize(),
            )?;
            sink.emit(TRAILING_WHITESPACE, range, || {
                LintDiagnosticBody::new("trailing whitespace").with_edit_fix(
                    "remove trailing whitespace",
                    range,
                    "",
                    Applicability::Always,
                )
            });
        }

        let character_count = content.chars().count();
        if character_count > config.max_line_length {
            let overflow_start = content
                .char_indices()
                .nth(config.max_line_length)
                .map(|(offset, _)| offset)
                .expect("line is longer than configured maximum");
            let range = text_range(
                line.content_range().start().to_usize() + overflow_start,
                line.content_range().end().to_usize(),
            )?;
            sink.emit(LINE_TOO_LONG, range, || {
                LintDiagnosticBody::new(format!(
                    "line has {character_count} characters; maximum is {}",
                    config.max_line_length
                ))
            });
        }
    }

    if !sink.is_full() {
        lint_syntax_issues(syntax, &mut sink);
    }
    if !sink.is_full() {
        lint_headings(document, &mut sink);
    }
    if !sink.is_full() {
        lint_attributes(document, config, &mut sink);
    }
    if !sink.is_full() {
        lint_anchors(document, &mut sink);
    }
    if !sink.is_full() {
        lint_links_and_references(document, config, &mut sink);
    }
    if !sink.is_full() {
        lint_list_presentation(document, &mut sink);
    }
    if !sink.is_full() {
        lint_document_presentation(document, &mut sink);
    }
    if !sink.is_full() {
        lint_tables(document, &mut sink);
    }
    if !sink.is_full() {
        lint_catalogs(document, &mut sink);
    }
    if !sink.is_full() {
        lint_document_structure(document, &mut sink);
    }
    Ok(sink.finish())
}

fn lint_list_presentation(
    document: &crate::parser::AstDocument,
    sink: &mut LintDiagnosticSink<'_>,
) {
    lint_list_presentation_with_observer(document, sink, |_| {});
}

fn lint_list_presentation_with_observer<'document>(
    document: &'document crate::parser::AstDocument,
    sink: &mut LintDiagnosticSink<'_>,
    mut observe: impl FnMut(crate::walker::SemanticNode<'document>),
) {
    let _: ControlFlow<()> = crate::walker::try_walk_ast(document, |node| {
        if sink.is_full() {
            return ControlFlow::Break(());
        }
        observe(node);
        let crate::walker::SemanticNode::Block(AstBlock::List(list)) = node else {
            return ControlFlow::Continue(());
        };
        for problem in &list.presentation_problems {
            if sink.is_full() {
                break;
            }
            let message = match problem.kind {
                crate::parser::ListPresentationProblemKind::InvalidStart => {
                    "ordered list start must be a positive integer"
                }
                crate::parser::ListPresentationProblemKind::InvalidExplicitNumber => {
                    "explicit ordered-list number must be a positive 32-bit integer"
                }
                crate::parser::ListPresentationProblemKind::InconsistentExplicitNumber => {
                    "explicit ordered-list numbers must be sequential"
                }
                crate::parser::ListPresentationProblemKind::UnknownOrderedStyle => {
                    "unsupported ordered list style"
                }
            };
            sink.emit(INVALID_LIST_PRESENTATION, problem.range, || {
                LintDiagnosticBody::new(message)
            });
            if sink.is_full() {
                return ControlFlow::Break(());
            }
        }
        ControlFlow::Continue(())
    });
}

fn lint_document_presentation(
    document: &crate::parser::AstDocument,
    sink: &mut LintDiagnosticSink<'_>,
) {
    if let Some(range) = document.presentation().toc_policy().invalid_level_range {
        sink.emit(INVALID_ATTRIBUTE, range, || {
            LintDiagnosticBody::new("toclevels must be an integer from 1 to 5")
        });
    }
}

fn lint_document_structure(
    document: &crate::parser::AstDocument,
    sink: &mut LintDiagnosticSink<'_>,
) {
    for problem in document.structure().problems() {
        if sink.is_full() {
            break;
        }
        let message = match problem.kind {
            crate::structure::StructureProblemKind::AppendixLevel => {
                "appendix must be a level-one section"
            }
            crate::structure::StructureProblemKind::AppendixDoctype => {
                "appendix is only valid for article or book documents"
            }
            crate::structure::StructureProblemKind::BibliographyNotSection => {
                "bibliography must be a section, not a document title or discrete heading"
            }
            crate::structure::StructureProblemKind::BibliographyScope => {
                "whole-book bibliography must be a level-zero section in a multipart book"
            }
            crate::structure::StructureProblemKind::BibliographyDoctype => {
                "bibliography is only valid for article or book documents"
            }
            crate::structure::StructureProblemKind::MissingManpageTitle => {
                "manpage document title is missing"
            }
            crate::structure::StructureProblemKind::InvalidManpageTitle => {
                "manpage title must use name(section)"
            }
            crate::structure::StructureProblemKind::MissingManpageNameSection => {
                "manpage NAME section is missing"
            }
            crate::structure::StructureProblemKind::InvalidManpagePurpose => {
                "manpage NAME paragraph must use name - purpose"
            }
        };
        sink.emit(INVALID_DOCUMENT_STRUCTURE, problem.range, || {
            LintDiagnosticBody::new(message)
        });
    }
}

fn lint_catalogs(document: &crate::parser::AstDocument, sink: &mut LintDiagnosticSink<'_>) {
    for problem in document.catalogs().problems() {
        if sink.is_full() {
            break;
        }
        let message = match problem.kind {
            crate::catalog::CatalogProblemKind::MissingFootnoteDefinition => {
                "named footnote definition does not exist"
            }
            crate::catalog::CatalogProblemKind::DuplicateFootnoteDefinition => {
                "duplicate named footnote definition"
            }
            crate::catalog::CatalogProblemKind::DuplicateBibliographyEntry => {
                "duplicate bibliography entry"
            }
            crate::catalog::CatalogProblemKind::EmptyIndexTerm => "index term is empty",
        };
        sink.emit(INVALID_CATALOG, problem.range, || {
            LintDiagnosticBody::new(message).with_related(
                problem
                    .related_range
                    .map(|range| RelatedInformation {
                        message: "first definition is here".to_owned(),
                        range,
                    })
                    .into_iter()
                    .collect(),
            )
        });
    }
}

fn lint_tables(document: &crate::parser::AstDocument, sink: &mut LintDiagnosticSink<'_>) {
    lint_tables_with_observer(document, sink, |_| {});
}

fn lint_tables_with_observer<'document>(
    document: &'document crate::parser::AstDocument,
    sink: &mut LintDiagnosticSink<'_>,
    mut observe: impl FnMut(crate::walker::SemanticNode<'document>),
) {
    let _: ControlFlow<()> = crate::walker::try_walk_ast(document, |node| {
        if sink.is_full() {
            return ControlFlow::Break(());
        }
        observe(node);
        let crate::walker::SemanticNode::Table(table) = node else {
            return ControlFlow::Continue(());
        };
        for problem in &table.problems {
            if sink.is_full() {
                break;
            }
            let message = match problem.kind {
                crate::table::TableProblemKind::InvalidFormat => "unsupported table format",
                crate::table::TableProblemKind::InvalidSeparator => {
                    "table separator must be one non-control character and match the delimiter"
                }
                crate::table::TableProblemKind::UnclosedQuotedCell => "unclosed quoted table cell",
                crate::table::TableProblemKind::InvalidPresentation => {
                    "invalid or conflicting table presentation attribute"
                }
            };
            sink.emit(INVALID_TABLE, problem.range, || {
                LintDiagnosticBody::new(message)
            });
            if sink.is_full() {
                return ControlFlow::Break(());
            }
        }
        ControlFlow::Continue(())
    });
}

fn lint_syntax_issues(syntax: &SyntaxTree, sink: &mut LintDiagnosticSink<'_>) {
    for issue in syntax.issues() {
        if sink.is_full() {
            break;
        }
        let rule = match issue.class {
            SyntaxIssueClass::HeadingMarkerSpace => HEADING_MARKER_SPACE,
            SyntaxIssueClass::InvalidHeadingLevel => INVALID_HEADING_LEVEL,
            SyntaxIssueClass::UnclosedInline => UNCLOSED_INLINE,
            SyntaxIssueClass::NestingLimitExceeded => NESTING_LIMIT_EXCEEDED,
            SyntaxIssueClass::UnclosedBlock => UNCLOSED_BLOCK,
            SyntaxIssueClass::MissingSourceLanguage => MISSING_SOURCE_LANGUAGE,
            SyntaxIssueClass::InvalidAttribute => INVALID_ATTRIBUTE,
            SyntaxIssueClass::InvalidUrl => INVALID_URL_SCHEME,
            SyntaxIssueClass::InvalidCrossReference => INVALID_CROSS_REFERENCE,
            SyntaxIssueClass::InconsistentList => INCONSISTENT_LIST,
            SyntaxIssueClass::InvalidStem => INVALID_STEM,
            SyntaxIssueClass::MacroBoundary => MACRO_BOUNDARY,
        };
        if issue.class == SyntaxIssueClass::MacroBoundary {
            let SyntaxIssueDetail::MacroBoundary { name } = issue.detail else {
                continue;
            };
            sink.emit(rule, issue.range, || {
                let source = syntax.source_document().source();
                let start = issue.range.start().to_usize();
                let fix = source[..start]
                    .chars()
                    .next_back()
                    .is_some_and(|character| character.is_ascii_alphanumeric())
                    .then(|| {
                        let range = TextRange::new(issue.range.start(), issue.range.start())
                            .expect("empty insertion range is ordered");
                        ("insert a space before the inline macro", range, " ")
                    });
                LintDiagnosticBody::new(format!(
                    "{name} inline macro must start at a token boundary"
                ))
                .with_optional_fix(fix, Applicability::Maybe)
            });
            continue;
        }
        sink.emit(rule, issue.range, || {
            let fix = issue.fix.map(|fix| (fix.label, fix.range, fix.replacement));
            LintDiagnosticBody::new(issue.message).with_optional_fix(fix, Applicability::Always)
        });
    }
}

fn lint_links_and_references(
    document: &crate::parser::AstDocument,
    config: &LintConfig,
    sink: &mut LintDiagnosticSink<'_>,
) {
    lint_links_and_references_with_observer(document, config, sink, |_| {});
}

fn lint_links_and_references_with_observer<'document>(
    document: &'document crate::parser::AstDocument,
    config: &LintConfig,
    sink: &mut LintDiagnosticSink<'_>,
    mut observe: impl FnMut(crate::walker::SemanticNode<'document>),
) {
    let targets = crate::document::reference_targets_ast(document);
    fn inspect(
        inline: &crate::inline::Inline,
        targets: &[crate::document::ReferenceTarget],
        config: &LintConfig,
        sink: &mut LintDiagnosticSink<'_>,
    ) -> ControlFlow<()> {
        if sink.is_full() {
            return ControlFlow::Break(());
        }
        use crate::inline::Inline;
        use crate::reference::ReferenceKey;
        match inline {
            Inline::Link(link) => {
                if !config.authored_url_policy.allows(&link.target) {
                    sink.emit(INVALID_URL_SCHEME, link.target_range, || {
                        LintDiagnosticBody::new("URL is rejected by the configured policy")
                    });
                }
                if sink.is_full() {
                    return ControlFlow::Break(());
                }
                if link.target_expansion_error.is_none()
                    && classify_file_target(&link.target)
                        .is_some_and(|target| is_asciidoc_extension(target.extension))
                    && let Some(range) = link.macro_name_range
                {
                    sink.emit(ASCIIDOC_FILE_LINK, range, || {
                        let fix = (link.target_attributes.is_empty()
                            && is_fixable_relative_target(&link.target))
                        .then_some(("replace link with xref", range, "xref"));
                        LintDiagnosticBody::new("use xref for an AsciiDoc document target")
                            .with_optional_fix(fix, Applicability::Always)
                    });
                }
            }
            Inline::Macro(node)
                if matches!(
                    node.kind,
                    crate::inline::StandardMacroKind::Image
                        | crate::inline::StandardMacroKind::Icon
                        | crate::inline::StandardMacroKind::Audio
                        | crate::inline::StandardMacroKind::Video
                ) && !config.authored_url_policy.allows(&node.target) =>
            {
                sink.emit(INVALID_URL_SCHEME, node.target_range, || {
                    LintDiagnosticBody::new("resource URL is rejected by the configured policy")
                });
            }
            Inline::Reference(reference) => match &reference.target {
                Some(ReferenceKey::Local { anchor }) => {
                    if !targets.iter().any(|target| target.id == *anchor) {
                        sink.emit(UNRESOLVED_CROSS_REFERENCE, reference.target_range, || {
                            LintDiagnosticBody::new("local cross reference target does not exist")
                        });
                    }
                }
                Some(ReferenceKey::Document { document, .. }) => {
                    if !valid_unresolved_relative_target(document) {
                        sink.emit(INVALID_CROSS_REFERENCE, reference.target_range, || {
                            LintDiagnosticBody::new("unsafe cross-document target")
                        });
                    }
                    if sink.is_full() {
                        return ControlFlow::Break(());
                    }
                    if reference.target_expansion_error.is_none()
                        && classify_file_target(&reference.expanded_target)
                            .is_some_and(|target| !is_asciidoc_extension(target.extension))
                        && let Some(range) = reference.macro_name_range
                    {
                        sink.emit(NON_ASCIIDOC_XREF, range, || {
                            let fix = (reference.target_attributes.is_empty()
                                && is_fixable_relative_target(&reference.expanded_target))
                            .then_some(("replace xref with link", range, "link"));
                            LintDiagnosticBody::new("use link for a non-AsciiDoc file target")
                                .with_optional_fix(fix, Applicability::Always)
                        });
                    }
                }
                Some(ReferenceKey::Scheme {
                    scheme, locator, ..
                }) => {
                    if scheme.is_empty()
                        || locator.is_empty()
                        || locator.chars().any(char::is_control)
                    {
                        sink.emit(INVALID_CROSS_REFERENCE, reference.target_range, || {
                            LintDiagnosticBody::new("invalid scheme-based cross reference")
                        });
                    }
                }
                None => {
                    sink.emit(INVALID_CROSS_REFERENCE, reference.target_range, || {
                        LintDiagnosticBody::new("invalid cross reference")
                    });
                }
            },
            Inline::Text(_)
            | Inline::Literal { .. }
            | Inline::Styled { .. }
            | Inline::AttributeReference { .. }
            | Inline::HardBreak { .. }
            | Inline::Passthrough { .. }
            | Inline::Macro(_)
            | Inline::Formula(_) => {}
        }
        if sink.is_full() {
            ControlFlow::Break(())
        } else {
            ControlFlow::Continue(())
        }
    }
    let _: ControlFlow<()> = crate::walker::try_walk_ast(document, |node| {
        if sink.is_full() {
            return ControlFlow::Break(());
        }
        observe(node);
        if let crate::walker::SemanticNode::Inline(inline) = node {
            inspect(inline, &targets, config, sink)
        } else {
            ControlFlow::Continue(())
        }
    });
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct FileTarget<'a> {
    extension: &'a str,
}

fn classify_file_target(target: &str) -> Option<FileTarget<'_>> {
    let path_end = target.find(['?', '#']).unwrap_or(target.len());
    let path = &target[..path_end];
    if path.starts_with("//")
        || path.contains([
            '\\', '\0', '\r', '\n', '\t', ' ', '[', ']', '<', '>', '"', '\'',
        ])
        || has_scheme(path)
    {
        return None;
    }
    let name = path.rsplit('/').next()?;
    let (stem, extension) = name.rsplit_once('.')?;
    if stem.is_empty() || extension.is_empty() {
        return None;
    }
    Some(FileTarget { extension })
}

fn is_asciidoc_extension(extension: &str) -> bool {
    extension.eq_ignore_ascii_case("adoc") || extension.eq_ignore_ascii_case("asciidoc")
}

fn is_fixable_relative_target(target: &str) -> bool {
    classify_file_target(target).is_some() && valid_unresolved_relative_target(target)
}

fn has_scheme(target: &str) -> bool {
    target.find(':').is_some()
}

/// Checks only syntax that is safe to retain for later host resolution.
///
/// Parent segments are valid here because linting performs no filesystem
/// access. Renderers and resource providers apply their own stricter policy
/// before turning the target into an active URL or local path.
fn valid_unresolved_relative_target(value: &str) -> bool {
    !value.is_empty()
        && !value.starts_with('/')
        && !value.contains(['\\', ':'])
        && !value.contains("//")
        && !value.chars().any(|character| {
            character.is_control()
                || character.is_whitespace()
                || matches!(character, '<' | '>' | '"' | '\'' | '`' | '{' | '}')
        })
        && valid_relative_percent_escapes(value)
}

fn valid_relative_percent_escapes(value: &str) -> bool {
    let bytes = value.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] != b'%' {
            index += 1;
            continue;
        }
        if index + 2 >= bytes.len() {
            return false;
        }
        let (Some(high), Some(low)) = (hex_digit(bytes[index + 1]), hex_digit(bytes[index + 2]))
        else {
            return false;
        };
        let decoded = high * 16 + low;
        if decoded <= 0x20 || decoded == 0x7f || matches!(decoded, b'.' | b'/' | b'\\') {
            return false;
        }
        index += 3;
    }
    true
}

const fn hex_digit(value: u8) -> Option<u8> {
    match value {
        b'0'..=b'9' => Some(value - b'0'),
        b'a'..=b'f' => Some(value - b'a' + 10),
        b'A'..=b'F' => Some(value - b'A' + 10),
        _ => None,
    }
}

fn lint_anchors(document: &crate::parser::AstDocument, sink: &mut LintDiagnosticSink<'_>) {
    let mut ids = BTreeMap::<String, TextRange>::new();
    for anchor in document.anchors() {
        if sink.is_full() {
            break;
        }
        if !anchor.valid {
            sink.emit(INVALID_ANCHOR, anchor.range, || {
                LintDiagnosticBody::new("invalid or unattached explicit anchor")
            });
        }
    }
    if sink.is_full() {
        return;
    }
    for target in crate::document::reference_targets_ast(document) {
        if sink.is_full() {
            break;
        }
        if let Some(first) = ids.insert(target.id.clone(), target.id_range) {
            sink.emit(DUPLICATE_ANCHOR, target.id_range, || {
                LintDiagnosticBody::new(format!("duplicate anchor ID `{}`", target.id))
                    .with_related(vec![RelatedInformation {
                        message: "first target with this ID".to_owned(),
                        range: first,
                    }])
            });
        }
    }
}

fn lint_attributes(
    document: &crate::parser::AstDocument,
    config: &LintConfig,
    sink: &mut LintDiagnosticSink<'_>,
) {
    use crate::attributes::{AttributeBindingId, DocumentAttributeOperation};

    for attribute in document.attributes() {
        if sink.is_full() {
            break;
        }
        if let Some(expected) = config.protected_attributes.get(&attribute.name) {
            let changed = match &attribute.operation {
                DocumentAttributeOperation::Set => expected
                    .as_ref()
                    .is_none_or(|expected| &attribute.value.folded_text != expected),
                DocumentAttributeOperation::Unset => expected.is_some(),
            };
            if changed {
                sink.emit(PROTECTED_ATTRIBUTE, attribute.range, || {
                    LintDiagnosticBody::new(format!(
                        "protected attribute `{}` cannot be changed",
                        attribute.name
                    ))
                });
            }
        }
    }

    if sink.is_full() {
        return;
    }
    let environment = document.attribute_environment();
    let references = document.resolved.facts().attribute_references();
    let inline_references = references
        .iter()
        .filter(|reference| {
            !environment.bindings().iter().any(|binding| {
                contains_range(
                    binding.occurrence().value.source_range,
                    reference.name_range,
                )
            })
        })
        .cloned()
        .collect();
    let mut used_bindings = BTreeSet::<AttributeBindingId>::new();
    lint_attribute_reference_uses(inline_references, &mut used_bindings, sink);
    if sink.is_full() {
        return;
    }
    for binding in environment.bindings() {
        if sink.is_full() {
            break;
        }
        let references = references
            .iter()
            .filter(|reference| {
                contains_range(
                    binding.occurrence().value.source_range,
                    reference.name_range,
                )
            })
            .collect::<Vec<_>>();
        for reference in &references {
            used_bindings.extend(reference.binding_id);
        }
        if let Err(error) = binding.value() {
            let range = references.first().map_or_else(
                || binding.occurrence().value.source_range,
                |reference| reference.name_range,
            );
            let undefined_reference =
                if error == crate::substitution::AttributeExpansionError::Undefined {
                    references.first().copied()
                } else {
                    None
                };
            let rule = if undefined_reference.is_some() {
                UNDEFINED_ATTRIBUTE
            } else {
                ATTRIBUTE_EXPANSION
            };
            sink.emit(rule, range, || {
                if let Some(reference) = undefined_reference {
                    LintDiagnosticBody::new(format!(
                        "undefined document attribute `{}`",
                        reference.name
                    ))
                } else {
                    LintDiagnosticBody::new(attribute_expansion_message(error))
                }
            });
        }
    }
    if sink.is_full() {
        return;
    }
    for binding in environment.bindings() {
        if sink.is_full() {
            break;
        }
        let occurrence = binding.occurrence();
        if binding.operation() == DocumentAttributeOperation::Set
            && !used_bindings.contains(&binding.id())
            && !config.protected_attributes.contains_key(&occurrence.name)
        {
            sink.emit(UNUSED_ATTRIBUTE, occurrence.name_range, || {
                LintDiagnosticBody::new(format!("unused document attribute `{}`", occurrence.name))
            });
        }
    }
}

fn lint_attribute_reference_uses(
    references: Vec<crate::attributes::AttributeReference>,
    used_bindings: &mut BTreeSet<crate::attributes::AttributeBindingId>,
    sink: &mut LintDiagnosticSink<'_>,
) {
    for reference in references {
        if sink.is_full() {
            break;
        }
        used_bindings.extend(reference.binding_id);
        match reference.value {
            Ok(Some(_)) => {}
            Ok(None) | Err(crate::substitution::AttributeExpansionError::Undefined) => {
                sink.emit(UNDEFINED_ATTRIBUTE, reference.name_range, || {
                    LintDiagnosticBody::new(format!(
                        "undefined document attribute `{}`",
                        reference.name
                    ))
                });
            }
            Err(error) => {
                sink.emit(ATTRIBUTE_EXPANSION, reference.range, || {
                    LintDiagnosticBody::new(attribute_expansion_message(error))
                });
            }
        }
    }
}

fn contains_range(outer: TextRange, inner: TextRange) -> bool {
    outer.start() <= inner.start() && inner.end() <= outer.end()
}

fn attribute_expansion_message(
    error: crate::substitution::AttributeExpansionError,
) -> &'static str {
    match error {
        crate::substitution::AttributeExpansionError::Undefined => {
            "document attribute expansion references an undefined attribute"
        }
        crate::substitution::AttributeExpansionError::Cycle => {
            "document attribute expansion contains a cycle"
        }
        crate::substitution::AttributeExpansionError::DepthLimitExceeded => {
            "document attribute expansion exceeds the depth limit"
        }
        crate::substitution::AttributeExpansionError::SizeLimitExceeded => {
            "document attribute expansion exceeds the size limit"
        }
    }
}

fn lint_headings(document: &crate::parser::AstDocument, sink: &mut LintDiagnosticSink<'_>) {
    let mut previous_level = None;
    let mut ids = BTreeMap::<String, TextRange>::new();

    for block in document.blocks() {
        if sink.is_full() {
            break;
        }
        let AstBlock::Heading(heading) = block else {
            continue;
        };

        let structurally_invalid = !heading.hierarchy_valid;
        match heading.kind {
            HeadingKind::DocumentTitle => {
                previous_level = None;
            }
            HeadingKind::Part => previous_level = None,
            HeadingKind::Discrete { .. } => {}
            HeadingKind::Section { level } => {
                let hierarchy_invalid =
                    previous_level.map_or(level > 1, |previous| level > previous + 1);
                if !structurally_invalid && hierarchy_invalid {
                    sink.emit(INVALID_HEADING_LEVEL, heading.marker_range, || {
                        LintDiagnosticBody::new("heading level skips the expected hierarchy")
                    });
                }
                previous_level = Some(level);
            }
        }

        let base = heading_id_base(&heading.text);
        if let Some(first_range) = ids.get(&base).copied() {
            sink.emit(DUPLICATE_HEADING_ID, heading.text_range, || {
                LintDiagnosticBody::new(format!("duplicate generated heading ID `{base}`"))
                    .with_related(vec![RelatedInformation {
                        message: "first heading with this ID".to_owned(),
                        range: first_range,
                    }])
            });
        } else {
            ids.insert(base, heading.text_range);
        }
    }
}

fn text_range(start: usize, end: usize) -> Result<TextRange, PositionError> {
    TextRange::new(TextSize::new(start)?, TextSize::new(end)?)
}

#[cfg(test)]
mod tests {
    use std::{cell::Cell, ops::ControlFlow};

    use super::{
        DUPLICATE_ANCHOR, DUPLICATE_HEADING_ID, INVALID_ANCHOR, INVALID_CATALOG, INVALID_TABLE,
        LINE_TOO_LONG, LINT_RULES, LintConfig, LintDiagnosticBody, LintDiagnosticSink, LintRuleId,
        MACRO_BOUNDARY, PROTECTED_ATTRIBUTE, RuleSettings, TRAILING_WHITESPACE, UNUSED_ATTRIBUTE,
        lint, lint_links_and_references_with_observer, lint_list_presentation_with_observer,
        lint_rule, lint_tables_with_observer, lint_with_analysis_limits,
        render_lint_rule_catalog_json, text_range,
    };
    use crate::diagnostic::{Applicability, RelatedInformation, Severity, TextEdit};

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
        assert_eq!(value["schemaVersion"], 1);
        assert_eq!(value["packageVersion"], crate::VERSION);
        let json_codes = value["rules"]
            .as_array()
            .expect("rules")
            .iter()
            .map(|rule| rule["code"].as_str().expect("code"))
            .collect::<Vec<_>>();
        assert_eq!(json_codes, codes);
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
        lint_list_presentation_with_observer(&list.ast, &mut sink, |node| {
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
        lint_tables_with_observer(&table.ast, &mut sink, |node| {
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
        lint_links_and_references_with_observer(&links.ast, &link_config, &mut sink, |node| {
            visited.push(node_kind(node));
        });
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

    #[test]
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
    #[should_panic(expected = "lint diagnostic rule is registered")]
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
            super::lint_syntax(&parsed.syntax, &parsed.ast, &config)
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
        let diagnostics =
            super::lint_syntax(&parsed.syntax, &parsed.ast, &LintConfig::default()).expect("lint");

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
