use crate::diagnostic::Applicability;
use crate::source::TextRange;
use crate::syntax::{SyntaxIssueClass, SyntaxIssueDetail};

use super::{
    HEADING_MARKER_SPACE, INCONSISTENT_LIST, INVALID_ATTRIBUTE, INVALID_CROSS_REFERENCE,
    INVALID_HEADING_LEVEL, INVALID_STEM, INVALID_URL_SCHEME, LintContext, LintDiagnosticBody,
    LintDiagnosticSink, MACRO_BOUNDARY, MISSING_SOURCE_LANGUAGE, NESTING_LIMIT_EXCEEDED,
    UNCLOSED_BLOCK, UNCLOSED_INLINE,
};

pub(super) fn lint_syntax_issues(context: &LintContext<'_>, sink: &mut LintDiagnosticSink<'_>) {
    let syntax = context.syntax();
    for issue in syntax.issues() {
        if sink.should_stop() {
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
