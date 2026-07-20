//! Output-independent lint rules over the original source.

use std::collections::BTreeMap;

use crate::diagnostic::{
    Applicability, Diagnostic, DiagnosticCode, DiagnosticId, Fix, Severity, TextEdit,
    sort_diagnostics,
};
use crate::source::{PositionError, TextRange, TextSize};
use crate::source_lines::{LineEnding, SourceLines};

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum LintRule {
    TrailingWhitespace,
    ExcessiveBlankLines,
    LineTooLong,
}

impl LintRule {
    pub const ALL: [Self; 3] = [
        Self::TrailingWhitespace,
        Self::ExcessiveBlankLines,
        Self::LineTooLong,
    ];

    pub const fn code(self) -> &'static str {
        match self {
            Self::TrailingWhitespace => "trailing-whitespace",
            Self::ExcessiveBlankLines => "excessive-blank-lines",
            Self::LineTooLong => "line-too-long",
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

    sort_diagnostics(&mut diagnostics);
    Ok(diagnostics)
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
    use super::{LintConfig, LintRule, RuleSettings, lint};
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
}
