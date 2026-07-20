//! Output-independent lint rules over the original source.

use std::collections::BTreeMap;

use crate::diagnostic::{
    Applicability, Diagnostic, DiagnosticCode, DiagnosticId, Fix, RelatedInformation, Severity,
    TextEdit, sort_diagnostics,
};
use crate::document::heading_id_base;
use crate::inline::InlineProblemKind;
use crate::parser::{
    AstBlock, BlockProblemKind, HeadingKind, HeadingProblem, ParseConfig, parse_with_config,
};
use crate::source::{PositionError, TextRange, TextSize};
use crate::source_lines::{LineEnding, SourceLines};

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum LintRule {
    TrailingWhitespace,
    ExcessiveBlankLines,
    LineTooLong,
    InvalidHeadingLevel,
    DuplicateHeadingId,
    HeadingMarkerSpace,
    UnclosedInline,
    NestingLimitExceeded,
    UnclosedBlock,
    MissingSourceLanguage,
    InvalidAttribute,
    DuplicateAttribute,
    UndefinedAttribute,
    UnusedAttribute,
    ProtectedAttribute,
    InvalidAnchor,
    DuplicateAnchor,
    InvalidUrlScheme,
    InvalidCrossReference,
    UnresolvedCrossReference,
    InconsistentList,
    InvalidStem,
}

impl LintRule {
    pub const ALL: [Self; 22] = [
        Self::TrailingWhitespace,
        Self::ExcessiveBlankLines,
        Self::LineTooLong,
        Self::InvalidHeadingLevel,
        Self::DuplicateHeadingId,
        Self::HeadingMarkerSpace,
        Self::UnclosedInline,
        Self::NestingLimitExceeded,
        Self::UnclosedBlock,
        Self::MissingSourceLanguage,
        Self::InvalidAttribute,
        Self::DuplicateAttribute,
        Self::UndefinedAttribute,
        Self::UnusedAttribute,
        Self::ProtectedAttribute,
        Self::InvalidAnchor,
        Self::DuplicateAnchor,
        Self::InvalidUrlScheme,
        Self::InvalidCrossReference,
        Self::UnresolvedCrossReference,
        Self::InconsistentList,
        Self::InvalidStem,
    ];

    pub const fn code(self) -> &'static str {
        match self {
            Self::TrailingWhitespace => "trailing-whitespace",
            Self::ExcessiveBlankLines => "excessive-blank-lines",
            Self::LineTooLong => "line-too-long",
            Self::InvalidHeadingLevel => "invalid-heading-level",
            Self::DuplicateHeadingId => "duplicate-heading-id",
            Self::HeadingMarkerSpace => "heading-marker-space",
            Self::UnclosedInline => "unclosed-inline",
            Self::NestingLimitExceeded => "nesting-limit-exceeded",
            Self::UnclosedBlock => "unclosed-block",
            Self::MissingSourceLanguage => "missing-source-language",
            Self::InvalidAttribute => "invalid-attribute",
            Self::DuplicateAttribute => "duplicate-attribute",
            Self::UndefinedAttribute => "undefined-attribute",
            Self::UnusedAttribute => "unused-attribute",
            Self::ProtectedAttribute => "protected-attribute",
            Self::InvalidAnchor => "invalid-anchor",
            Self::DuplicateAnchor => "duplicate-anchor",
            Self::InvalidUrlScheme => "invalid-url-scheme",
            Self::InvalidCrossReference => "invalid-cross-reference",
            Self::UnresolvedCrossReference => "unresolved-cross-reference",
            Self::InconsistentList => "inconsistent-list",
            Self::InvalidStem => "invalid-stem",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RuleSettings {
    pub enabled: bool,
    pub severity: Severity,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LintConfig {
    rules: BTreeMap<LintRule, RuleSettings>,
    pub max_line_length: usize,
    pub max_consecutive_blank_lines: usize,
    pub max_diagnostics: usize,
    pub max_inline_depth: usize,
    pub max_list_depth: usize,
    pub max_formula_bytes: usize,
    pub protected_attributes: BTreeMap<String, String>,
    pub protected_attribute_severity: Severity,
    pub url_policy: crate::url::UrlPolicy,
}

impl Default for LintConfig {
    fn default() -> Self {
        Self {
            rules: LintRule::ALL
                .into_iter()
                .map(|rule| {
                    (
                        rule,
                        RuleSettings {
                            enabled: true,
                            severity: Severity::Warning,
                        },
                    )
                })
                .collect(),
            max_line_length: 100,
            max_consecutive_blank_lines: 2,
            max_diagnostics: 1_000,
            max_inline_depth: 32,
            max_list_depth: 8,
            max_formula_bytes: 1024 * 1024,
            protected_attributes: BTreeMap::new(),
            protected_attribute_severity: Severity::Error,
            url_policy: crate::url::UrlPolicy::default(),
        }
    }
}

impl LintConfig {
    pub fn set_rule(&mut self, rule: LintRule, settings: RuleSettings) {
        self.rules.insert(rule, settings);
    }

    pub fn rule(&self, rule: LintRule) -> RuleSettings {
        self.rules.get(&rule).copied().unwrap_or(RuleSettings {
            enabled: false,
            severity: Severity::Warning,
        })
    }
}

pub fn lint(source: &str, config: &LintConfig) -> Result<Vec<Diagnostic>, PositionError> {
    let parsed = parse_with_config(
        source,
        &ParseConfig {
            max_inline_depth: config.max_inline_depth,
            max_list_depth: config.max_list_depth,
            max_formula_bytes: config.max_formula_bytes,
        },
    )?;
    lint_parsed(source, &parsed.ast, config)
}

pub fn lint_parsed(
    source: &str,
    document: &crate::parser::AstDocument,
    config: &LintConfig,
) -> Result<Vec<Diagnostic>, PositionError> {
    let source_lines = SourceLines::new(source)?;
    let mut diagnostics = Vec::new();
    let mut blank_count = 0;

    for line in source_lines.lines() {
        let content = source_lines
            .text(line.content_range())
            .expect("line ranges are valid");
        let is_virtual_final_line =
            line.full_range().is_empty() && line.ending() == LineEnding::None;
        let is_blank = content.trim_matches([' ', '\t']).is_empty();

        if is_blank && !is_virtual_final_line {
            blank_count += 1;
            if blank_count > config.max_consecutive_blank_lines {
                push_diagnostic(
                    &mut diagnostics,
                    config,
                    LintRule::ExcessiveBlankLines,
                    line.full_range(),
                    "excessive blank line",
                    Some(("remove excessive blank line", line.full_range(), "")),
                );
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
            push_diagnostic(
                &mut diagnostics,
                config,
                LintRule::TrailingWhitespace,
                range,
                "trailing whitespace",
                Some(("remove trailing whitespace", range, "")),
            );
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
            push_diagnostic(
                &mut diagnostics,
                config,
                LintRule::LineTooLong,
                range,
                &format!(
                    "line has {character_count} characters; maximum is {}",
                    config.max_line_length
                ),
                None,
            );
        }
    }

    lint_headings(document, config, &mut diagnostics);
    lint_attributes(document, config, &mut diagnostics);
    lint_anchors(document, config, &mut diagnostics);
    lint_links_and_references(document, config, &mut diagnostics);
    sort_diagnostics(&mut diagnostics);
    diagnostics.truncate(config.max_diagnostics);
    Ok(diagnostics)
}

fn lint_links_and_references(
    document: &crate::parser::AstDocument,
    config: &LintConfig,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let targets = crate::document::reference_targets(document);
    fn inspect(
        inlines: &[crate::inline::Inline],
        targets: &[crate::document::ReferenceTarget],
        config: &LintConfig,
        diagnostics: &mut Vec<Diagnostic>,
    ) {
        use crate::inline::{Inline, ReferenceDestination};
        for inline in inlines {
            match inline {
                Inline::Link(link) => {
                    if !config.url_policy.allows(&link.target) {
                        push_diagnostic(
                            diagnostics,
                            config,
                            LintRule::InvalidUrlScheme,
                            link.target_range,
                            "URL is rejected by the configured policy",
                            None,
                        );
                    }
                    inspect(&link.label, targets, config, diagnostics);
                }
                Inline::Reference(reference) => {
                    match &reference.destination {
                        ReferenceDestination::Local { anchor, .. } => {
                            if !targets.iter().any(|target| target.id == *anchor) {
                                push_diagnostic(
                                    diagnostics,
                                    config,
                                    LintRule::UnresolvedCrossReference,
                                    reference.target_range,
                                    "local cross reference target does not exist",
                                    None,
                                );
                            }
                        }
                        ReferenceDestination::Document { document, .. } => {
                            if !valid_document_target(document) {
                                push_diagnostic(
                                    diagnostics,
                                    config,
                                    LintRule::InvalidCrossReference,
                                    reference.target_range,
                                    "unsafe cross-document target",
                                    None,
                                );
                            }
                        }
                        ReferenceDestination::Scheme {
                            scheme, locator, ..
                        } => {
                            if scheme.is_empty()
                                || locator.is_empty()
                                || locator.chars().any(char::is_control)
                            {
                                push_diagnostic(
                                    diagnostics,
                                    config,
                                    LintRule::InvalidCrossReference,
                                    reference.target_range,
                                    "invalid scheme-based cross reference",
                                    None,
                                );
                            }
                        }
                        ReferenceDestination::Invalid => push_diagnostic(
                            diagnostics,
                            config,
                            LintRule::InvalidCrossReference,
                            reference.target_range,
                            "invalid cross reference",
                            None,
                        ),
                    }
                    inspect(&reference.label, targets, config, diagnostics);
                }
                Inline::Styled { children, .. } => {
                    inspect(children, targets, config, diagnostics);
                }
                _ => {}
            }
        }
    }
    document.visit_inline_sequences(|inlines| {
        inspect(inlines, &targets, config, diagnostics);
    });
}

fn valid_document_target(value: &str) -> bool {
    !value.is_empty()
        && !value.starts_with('/')
        && !value.starts_with('\\')
        && !value.contains('\\')
        && !value.contains("://")
        && !value.split('/').any(|segment| segment == "..")
        && !value
            .chars()
            .any(|character| character.is_control() || character.is_whitespace())
}

fn lint_anchors(
    document: &crate::parser::AstDocument,
    config: &LintConfig,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let mut ids = BTreeMap::<String, TextRange>::new();
    for anchor in &document.anchors {
        if !anchor.valid {
            push_diagnostic(
                diagnostics,
                config,
                LintRule::InvalidAnchor,
                anchor.range,
                "invalid or unattached explicit anchor",
                None,
            );
        }
    }
    for target in crate::document::reference_targets(document) {
        if let Some(first) = ids.insert(target.id.clone(), target.id_range) {
            let settings = config.rule(LintRule::DuplicateAnchor);
            if settings.enabled {
                diagnostics.push(Diagnostic {
                    id: DiagnosticId::new(format!(
                        "{}@{}:{}",
                        LintRule::DuplicateAnchor.code(),
                        target.id_range.start().to_u32(),
                        target.id_range.end().to_u32()
                    )),
                    code: DiagnosticCode::new(LintRule::DuplicateAnchor.code()),
                    severity: settings.severity,
                    message: format!("duplicate anchor ID `{}`", target.id),
                    range: target.id_range,
                    related: vec![RelatedInformation {
                        message: "first target with this ID".to_owned(),
                        range: first,
                    }],
                    fixes: Vec::new(),
                });
            }
        }
    }
}

fn lint_attributes(
    document: &crate::parser::AstDocument,
    config: &LintConfig,
    diagnostics: &mut Vec<Diagnostic>,
) {
    use crate::attributes::{AttributeOperation, AttributeProblemKind};

    let mut definitions = BTreeMap::<String, TextRange>::new();
    let mut used = BTreeMap::<String, Vec<TextRange>>::new();
    for problem in &document.attribute_problems {
        let message = match problem.kind {
            AttributeProblemKind::InvalidName => "invalid document attribute name",
            AttributeProblemKind::InvalidValue => "invalid document attribute value",
        };
        push_diagnostic(
            diagnostics,
            config,
            LintRule::InvalidAttribute,
            problem.range,
            message,
            None,
        );
    }
    for attribute in &document.attributes {
        if let Some(first) = definitions.insert(attribute.name.clone(), attribute.name_range) {
            let settings = config.rule(LintRule::DuplicateAttribute);
            if settings.enabled {
                diagnostics.push(Diagnostic {
                    id: DiagnosticId::new(format!(
                        "{}@{}:{}",
                        LintRule::DuplicateAttribute.code(),
                        attribute.name_range.start().to_u32(),
                        attribute.name_range.end().to_u32()
                    )),
                    code: DiagnosticCode::new(LintRule::DuplicateAttribute.code()),
                    severity: settings.severity,
                    message: format!("duplicate document attribute `{}`", attribute.name),
                    range: attribute.name_range,
                    related: vec![RelatedInformation {
                        message: "previous definition".to_owned(),
                        range: first,
                    }],
                    fixes: Vec::new(),
                });
            }
        }
        if let Some(expected) = config.protected_attributes.get(&attribute.name) {
            let changed = match &attribute.operation {
                AttributeOperation::Set => &attribute.raw_value != expected,
                AttributeOperation::Unset => true,
            };
            if changed && config.rule(LintRule::ProtectedAttribute).enabled {
                diagnostics.push(Diagnostic {
                    id: DiagnosticId::new(format!(
                        "{}@{}:{}",
                        LintRule::ProtectedAttribute.code(),
                        attribute.range.start().to_u32(),
                        attribute.range.end().to_u32()
                    )),
                    code: DiagnosticCode::new(LintRule::ProtectedAttribute.code()),
                    severity: config.protected_attribute_severity,
                    message: format!("protected attribute `{}` cannot be changed", attribute.name),
                    range: attribute.range,
                    related: Vec::new(),
                    fixes: Vec::new(),
                });
            }
        }
    }
    collect_attribute_references(document, &mut used);
    for (name, ranges) in &used {
        if !definitions.contains_key(name) {
            for range in ranges {
                push_diagnostic(
                    diagnostics,
                    config,
                    LintRule::UndefinedAttribute,
                    *range,
                    &format!("undefined document attribute `{name}`"),
                    None,
                );
            }
        }
    }
    for (name, range) in definitions {
        if !used.contains_key(&name) && !config.protected_attributes.contains_key(&name) {
            push_diagnostic(
                diagnostics,
                config,
                LintRule::UnusedAttribute,
                range,
                &format!("unused document attribute `{name}`"),
                None,
            );
        }
    }
}

fn collect_attribute_references(
    document: &crate::parser::AstDocument,
    used: &mut BTreeMap<String, Vec<TextRange>>,
) {
    fn collect(inlines: &[crate::inline::Inline], used: &mut BTreeMap<String, Vec<TextRange>>) {
        for inline in inlines {
            match inline {
                crate::inline::Inline::AttributeReference {
                    name, name_range, ..
                } => used.entry(name.clone()).or_default().push(*name_range),
                crate::inline::Inline::Styled { children, .. } => collect(children, used),
                crate::inline::Inline::Link(link) => {
                    for attribute in &link.target_attributes {
                        used.entry(attribute.name.clone())
                            .or_default()
                            .push(attribute.name_range);
                    }
                    collect(&link.label, used);
                }
                crate::inline::Inline::Reference(reference) => collect(&reference.label, used),
                _ => {}
            }
        }
    }
    document.visit_inline_sequences(|inlines| collect(inlines, used));
}

fn lint_headings(
    document: &crate::parser::AstDocument,
    config: &LintConfig,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let mut previous_level = None;
    let mut ids = BTreeMap::<String, TextRange>::new();

    for block in &document.blocks {
        match block {
            AstBlock::Heading(heading) => {
                push_inline_problems(diagnostics, config, &heading.inline_problems);
            }
            AstBlock::Paragraph(paragraph) => {
                for line in &paragraph.lines {
                    push_inline_problems(diagnostics, config, &line.inline_problems);
                }
            }
            AstBlock::Literal(literal) => {
                for problem in &literal.problems {
                    match problem.kind {
                        BlockProblemKind::UnclosedBlock => push_diagnostic(
                            diagnostics,
                            config,
                            LintRule::UnclosedBlock,
                            problem.range,
                            "unclosed literal block",
                            None,
                        ),
                        BlockProblemKind::MissingSourceLanguage => {}
                    }
                }
            }
            AstBlock::Source(source) => {
                for problem in &source.problems {
                    let (rule, message) = match problem.kind {
                        BlockProblemKind::UnclosedBlock => {
                            (LintRule::UnclosedBlock, "unclosed source block")
                        }
                        BlockProblemKind::MissingSourceLanguage => (
                            LintRule::MissingSourceLanguage,
                            "source block requires a language",
                        ),
                    };
                    push_diagnostic(diagnostics, config, rule, problem.range, message, None);
                }
            }
            AstBlock::List(list) => lint_list(list, config, diagnostics),
            AstBlock::Math(math) => {
                for problem in &math.problems {
                    let message = match problem.kind {
                        crate::parser::MathProblemKind::Unclosed => "unclosed STEM block",
                        crate::parser::MathProblemKind::Empty => "STEM block is empty",
                        crate::parser::MathProblemKind::SizeLimitExceeded => {
                            "STEM block exceeds the size limit"
                        }
                    };
                    push_diagnostic(
                        diagnostics,
                        config,
                        LintRule::InvalidStem,
                        problem.range,
                        message,
                        None,
                    );
                }
            }
            AstBlock::Unsupported(_) => {}
        }
        let AstBlock::Heading(heading) = block else {
            continue;
        };

        if heading.problems.contains(&HeadingProblem::MissingSpace) {
            let insertion = TextRange::new(heading.marker_range.end(), heading.marker_range.end())
                .expect("empty insertion range is ordered");
            push_diagnostic(
                diagnostics,
                config,
                LintRule::HeadingMarkerSpace,
                insertion,
                "heading marker must be followed by a space",
                Some(("insert a space after heading marker", insertion, " ")),
            );
        }

        let structurally_invalid = heading.problems.iter().any(|problem| {
            matches!(
                problem,
                HeadingProblem::LevelTooDeep | HeadingProblem::MisplacedDocumentTitle
            )
        });
        match heading.kind {
            HeadingKind::DocumentTitle => {
                previous_level = None;
                if structurally_invalid {
                    push_diagnostic(
                        diagnostics,
                        config,
                        LintRule::InvalidHeadingLevel,
                        heading.marker_range,
                        "document title is only allowed before document content",
                        None,
                    );
                }
            }
            HeadingKind::Section { level } => {
                let hierarchy_invalid =
                    previous_level.map_or(level > 1, |previous| level > previous + 1);
                if structurally_invalid || hierarchy_invalid {
                    push_diagnostic(
                        diagnostics,
                        config,
                        LintRule::InvalidHeadingLevel,
                        heading.marker_range,
                        "heading level skips the expected hierarchy",
                        None,
                    );
                }
                previous_level = Some(level);
            }
        }

        let base = heading_id_base(&heading.text);
        if let Some(first_range) = ids.get(&base).copied() {
            let settings = config.rule(LintRule::DuplicateHeadingId);
            if settings.enabled {
                diagnostics.push(Diagnostic {
                    id: DiagnosticId::new(format!(
                        "{}@{}:{}",
                        LintRule::DuplicateHeadingId.code(),
                        heading.text_range.start().to_u32(),
                        heading.text_range.end().to_u32()
                    )),
                    code: DiagnosticCode::new(LintRule::DuplicateHeadingId.code()),
                    severity: settings.severity,
                    message: format!("duplicate generated heading ID `{base}`"),
                    range: heading.text_range,
                    related: vec![RelatedInformation {
                        message: "first heading with this ID".to_owned(),
                        range: first_range,
                    }],
                    fixes: Vec::new(),
                });
            }
        } else {
            ids.insert(base, heading.text_range);
        }
    }
}

fn lint_list(
    list: &crate::parser::ListBlock,
    config: &LintConfig,
    diagnostics: &mut Vec<Diagnostic>,
) {
    use crate::parser::ListProblemKind;
    for item in &list.items {
        push_inline_problems(diagnostics, config, &item.inline_problems);
        for problem in &item.problems {
            let (message, fix) = match problem.kind {
                ListProblemKind::EmptyItem => ("list item is empty", None),
                ListProblemKind::InconsistentMarker => {
                    ("list marker kind changes at the same depth", None)
                }
                ListProblemKind::InvalidNesting => ("list nesting skips a depth", None),
                ListProblemKind::DepthLimitExceeded => {
                    ("list nesting exceeds the configured limit", None)
                }
                ListProblemKind::NonCanonicalSeparator => (
                    "list marker must be followed by one space",
                    Some(("replace the separator with a space", problem.range, " ")),
                ),
            };
            push_diagnostic(
                diagnostics,
                config,
                LintRule::InconsistentList,
                problem.range,
                message,
                fix,
            );
        }
        for child in &item.children {
            lint_list(child, config, diagnostics);
        }
        for continuation in &item.continuations {
            match continuation {
                AstBlock::Literal(block) => {
                    for problem in &block.problems {
                        if problem.kind == BlockProblemKind::UnclosedBlock {
                            push_diagnostic(
                                diagnostics,
                                config,
                                LintRule::UnclosedBlock,
                                problem.range,
                                "unclosed literal block",
                                None,
                            );
                        }
                    }
                }
                AstBlock::Source(block) => {
                    for problem in &block.problems {
                        let (rule, message) = match problem.kind {
                            BlockProblemKind::UnclosedBlock => {
                                (LintRule::UnclosedBlock, "unclosed source block")
                            }
                            BlockProblemKind::MissingSourceLanguage => (
                                LintRule::MissingSourceLanguage,
                                "source block requires a language",
                            ),
                        };
                        push_diagnostic(diagnostics, config, rule, problem.range, message, None);
                    }
                }
                _ => {}
            }
        }
    }
}

fn push_inline_problems(
    diagnostics: &mut Vec<Diagnostic>,
    config: &LintConfig,
    problems: &[crate::inline::InlineProblem],
) {
    for problem in problems {
        match problem.kind {
            InlineProblemKind::UnclosedMonospace => push_diagnostic(
                diagnostics,
                config,
                LintRule::UnclosedInline,
                problem.range,
                "unclosed monospace span",
                None,
            ),
            InlineProblemKind::UnclosedStrong => push_diagnostic(
                diagnostics,
                config,
                LintRule::UnclosedInline,
                problem.range,
                "unclosed strong span",
                None,
            ),
            InlineProblemKind::UnclosedEmphasis => push_diagnostic(
                diagnostics,
                config,
                LintRule::UnclosedInline,
                problem.range,
                "unclosed emphasis span",
                None,
            ),
            InlineProblemKind::NestingLimitExceeded => push_diagnostic(
                diagnostics,
                config,
                LintRule::NestingLimitExceeded,
                problem.range,
                "inline nesting limit exceeded",
                None,
            ),
            InlineProblemKind::UnclosedAttributeReference => push_diagnostic(
                diagnostics,
                config,
                LintRule::UnclosedInline,
                problem.range,
                "unclosed attribute reference",
                None,
            ),
            InlineProblemKind::IncompleteLink => push_diagnostic(
                diagnostics,
                config,
                LintRule::InvalidUrlScheme,
                problem.range,
                "incomplete link macro",
                None,
            ),
            InlineProblemKind::IncompleteCrossReference
            | InlineProblemKind::InvalidCrossReference => push_diagnostic(
                diagnostics,
                config,
                LintRule::InvalidCrossReference,
                problem.range,
                "incomplete or invalid cross reference",
                None,
            ),
            InlineProblemKind::UnclosedStem => push_diagnostic(
                diagnostics,
                config,
                LintRule::InvalidStem,
                problem.range,
                "unclosed inline STEM",
                None,
            ),
            InlineProblemKind::EmptyStem => push_diagnostic(
                diagnostics,
                config,
                LintRule::InvalidStem,
                problem.range,
                "inline STEM is empty",
                None,
            ),
            InlineProblemKind::StemSizeLimitExceeded => push_diagnostic(
                diagnostics,
                config,
                LintRule::InvalidStem,
                problem.range,
                "inline STEM exceeds the size limit",
                None,
            ),
        }
    }
}

fn push_diagnostic(
    diagnostics: &mut Vec<Diagnostic>,
    config: &LintConfig,
    rule: LintRule,
    range: TextRange,
    message: &str,
    fix: Option<(&str, TextRange, &str)>,
) {
    let settings = config.rule(rule);
    if !settings.enabled {
        return;
    }
    let fixes = fix
        .map(|(title, edit_range, replacement)| {
            vec![
                Fix::new(
                    title,
                    Applicability::Always,
                    vec![TextEdit {
                        range: edit_range,
                        replacement: replacement.to_owned(),
                    }],
                )
                .expect("a single edit cannot conflict"),
            ]
        })
        .unwrap_or_default();
    diagnostics.push(Diagnostic {
        id: DiagnosticId::new(format!(
            "{}@{}:{}",
            rule.code(),
            range.start().to_u32(),
            range.end().to_u32()
        )),
        code: DiagnosticCode::new(rule.code()),
        severity: settings.severity,
        message: message.to_owned(),
        range,
        related: Vec::new(),
        fixes,
    });
}

fn text_range(start: usize, end: usize) -> Result<TextRange, PositionError> {
    TextRange::new(TextSize::new(start)?, TextSize::new(end)?)
}

#[cfg(test)]
mod tests {
    use super::{
        InlineProblemKind, LintConfig, LintRule, RuleSettings, lint, push_inline_problems,
        text_range,
    };
    use crate::diagnostic::Severity;

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
            LintRule::TrailingWhitespace,
            RuleSettings {
                enabled: false,
                severity: Severity::Error,
            },
        );
        config.set_rule(
            LintRule::LineTooLong,
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
    fn monospace_lint_reports_unclosed_span() {
        let diagnostics = lint("before `open\nnext", &LintConfig::default()).expect("valid source");

        assert!(
            diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code.as_str() == "unclosed-inline")
        );
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
        let problem = crate::inline::InlineProblem {
            kind: InlineProblemKind::NestingLimitExceeded,
            range: text_range(0, 1).expect("valid range"),
        };
        let mut diagnostics = Vec::new();
        push_inline_problems(&mut diagnostics, &LintConfig::default(), &[problem]);

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
    fn document_attributes_report_duplicate_undefined_unused_and_invalid_names() {
        let diagnostics = lint(
            "= Note\n\
             :bad name: value\n\
             :unused: value\n\
             :name: first\n\
             :name: second\n\
             \n\
             {name} {missing}\n",
            &LintConfig::default(),
        )
        .expect("lint");
        let codes = diagnostics
            .iter()
            .map(|diagnostic| diagnostic.code.as_str())
            .collect::<Vec<_>>();
        assert!(codes.contains(&"invalid-attribute"));
        assert!(codes.contains(&"duplicate-attribute"));
        assert!(codes.contains(&"undefined-attribute"));
        assert!(codes.contains(&"unused-attribute"));
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
    fn lint_parsed_reuses_ast_without_changing_diagnostics() {
        let source = "= Note\n:name: value\n\n{name}  \n";
        let parsed = crate::parser::parse(source).expect("parse");
        let config = LintConfig::default();

        assert_eq!(
            lint(source, &config).expect("standalone lint"),
            super::lint_parsed(source, &parsed.ast, &config).expect("lint existing AST")
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

        assert!(
            codes
                .iter()
                .filter(|code| **code == "invalid-url-scheme")
                .count()
                >= 2
        );
        assert!(
            codes
                .iter()
                .filter(|code| **code == "invalid-cross-reference")
                .count()
                >= 2
        );
        assert!(codes.contains(&"unresolved-cross-reference"));
    }

    #[test]
    fn url_policy_checks_the_semantically_expanded_link_target() {
        let source = ":scheme: https\n\n{scheme}://example.com[label]\n";
        let parsed = crate::parser::parse(source).expect("parse");
        let diagnostics =
            super::lint_parsed(source, &parsed.ast, &LintConfig::default()).expect("lint");

        assert!(
            !diagnostics
                .iter()
                .any(|diagnostic| { diagnostic.code.as_str() == "invalid-url-scheme" })
        );
    }

    #[test]
    fn cross_references_resolve_local_targets_but_leave_documents_for_hosts() {
        let diagnostics = lint(
            "[[target]]\n== Target\n\n<<target>> xref:#target[] xref:other.adoc#part[]",
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

        assert_eq!(parsed.ast.blocks.len(), 1);
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
            "x".repeat(LintConfig::default().max_formula_bytes + 1)
        );
        let diagnostics = lint(&source, &LintConfig::default()).expect("lint");

        assert!(diagnostics.iter().any(|diagnostic| {
            diagnostic.code.as_str() == "invalid-stem" && diagnostic.message.contains("size limit")
        }));
    }
}
