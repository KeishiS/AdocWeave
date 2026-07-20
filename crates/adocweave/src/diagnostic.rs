//! Diagnostics and safe source edits shared by all front ends.

use std::error::Error;
use std::fmt::{self, Write as _};

use crate::source::{LineIndex, PositionEncoding, PositionError, TextRange};

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct DiagnosticId(String);

impl DiagnosticId {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct DiagnosticCode(String);

impl DiagnosticCode {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum Severity {
    Error,
    Warning,
    Information,
    Hint,
}

impl Severity {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Error => "error",
            Self::Warning => "warning",
            Self::Information => "information",
            Self::Hint => "hint",
        }
    }
}

impl Applicability {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Always => "always",
            Self::Maybe => "maybe",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RelatedInformation {
    pub message: String,
    pub range: TextRange,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TextEdit {
    pub range: TextRange,
    pub replacement: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Applicability {
    Always,
    Maybe,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Fix {
    pub title: String,
    pub applicability: Applicability,
    edits: Vec<TextEdit>,
}

impl Fix {
    pub fn new(
        title: impl Into<String>,
        applicability: Applicability,
        mut edits: Vec<TextEdit>,
    ) -> Result<Self, EditConflict> {
        edits.sort_by_key(|edit| (edit.range.start(), edit.range.end()));
        validate_edits(&edits)?;

        Ok(Self {
            title: title.into(),
            applicability,
            edits,
        })
    }

    pub fn edits(&self) -> &[TextEdit] {
        &self.edits
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EditConflictKind {
    Duplicate,
    Overlap,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct EditConflict {
    pub first: TextRange,
    pub second: TextRange,
    pub kind: EditConflictKind,
}

impl fmt::Display for EditConflict {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "{:?} text edits at {:?} and {:?}",
            self.kind, self.first, self.second
        )
    }
}

impl Error for EditConflict {}

fn validate_edits(edits: &[TextEdit]) -> Result<(), EditConflict> {
    for pair in edits.windows(2) {
        let first = pair[0].range;
        let second = pair[1].range;
        if first == second {
            return Err(EditConflict {
                first,
                second,
                kind: EditConflictKind::Duplicate,
            });
        }
        if first.end() > second.start() {
            return Err(EditConflict {
                first,
                second,
                kind: EditConflictKind::Overlap,
            });
        }
    }
    Ok(())
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Diagnostic {
    pub id: DiagnosticId,
    pub code: DiagnosticCode,
    pub severity: Severity,
    pub message: String,
    pub range: TextRange,
    pub related: Vec<RelatedInformation>,
    pub fixes: Vec<Fix>,
}

/// Sorts diagnostics into the canonical order used by every front end.
pub fn sort_diagnostics(diagnostics: &mut [Diagnostic]) {
    diagnostics.sort_by(|left, right| {
        (
            left.range.start(),
            left.range.end(),
            &left.code,
            &left.id,
            left.severity,
            &left.message,
        )
            .cmp(&(
                right.range.start(),
                right.range.end(),
                &right.code,
                &right.id,
                right.severity,
                &right.message,
            ))
    });
}

/// Stable failure categories shared by CLI, LSP, Web API, and WASM adapters.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CoreErrorCode {
    InvalidInput,
    ParseFailed,
    LimitExceeded,
    Cancelled,
    InternalInvariant,
}

impl CoreErrorCode {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::InvalidInput => "invalid-input",
            Self::ParseFailed => "parse-failed",
            Self::LimitExceeded => "limit-exceeded",
            Self::Cancelled => "cancelled",
            Self::InternalInvariant => "internal-invariant",
        }
    }
}

pub fn render_human(
    diagnostics: &[Diagnostic],
    line_index: &LineIndex,
    encoding: PositionEncoding,
) -> Result<String, PositionError> {
    let mut diagnostics = diagnostics.to_vec();
    sort_diagnostics(&mut diagnostics);
    let mut output = String::new();

    for diagnostic in diagnostics {
        let start = line_index.offset_to_position(diagnostic.range.start(), encoding)?;
        writeln!(
            output,
            "{}:{}: {}[{}]: {}",
            start.line + 1,
            start.character + 1,
            diagnostic.severity.as_str(),
            diagnostic.code.as_str(),
            diagnostic.message
        )
        .expect("writing to a String cannot fail");
    }

    Ok(output)
}

pub fn render_json(diagnostics: &[Diagnostic]) -> String {
    let mut diagnostics = diagnostics.to_vec();
    sort_diagnostics(&mut diagnostics);
    let mut output = String::from("[");

    for (index, diagnostic) in diagnostics.iter().enumerate() {
        if index != 0 {
            output.push(',');
        }
        output.push('{');
        write_json_field(&mut output, "id", diagnostic.id.as_str());
        output.push(',');
        write_json_field(&mut output, "code", diagnostic.code.as_str());
        output.push_str(",\"severity\":");
        write_json_string(&mut output, diagnostic.severity.as_str());
        write!(
            output,
            ",\"range\":{{\"start\":{},\"end\":{}}}",
            diagnostic.range.start().to_u32(),
            diagnostic.range.end().to_u32()
        )
        .expect("writing to a String cannot fail");
        output.push_str(",\"message\":");
        write_json_string(&mut output, &diagnostic.message);
        output.push_str(",\"related\":[");
        for (related_index, related) in diagnostic.related.iter().enumerate() {
            if related_index != 0 {
                output.push(',');
            }
            write!(
                output,
                "{{\"range\":{{\"start\":{},\"end\":{}}},\"message\":",
                related.range.start().to_u32(),
                related.range.end().to_u32()
            )
            .expect("writing to a String cannot fail");
            write_json_string(&mut output, &related.message);
            output.push('}');
        }
        output.push_str("],\"fixes\":[");
        for (fix_index, fix) in diagnostic.fixes.iter().enumerate() {
            if fix_index != 0 {
                output.push(',');
            }
            output.push('{');
            write_json_field(&mut output, "title", &fix.title);
            output.push_str(",\"applicability\":");
            write_json_string(&mut output, fix.applicability.as_str());
            output.push_str(",\"edits\":[");
            for (edit_index, edit) in fix.edits().iter().enumerate() {
                if edit_index != 0 {
                    output.push(',');
                }
                write!(
                    output,
                    "{{\"range\":{{\"start\":{},\"end\":{}}},\"replacement\":",
                    edit.range.start().to_u32(),
                    edit.range.end().to_u32()
                )
                .expect("writing to a String cannot fail");
                write_json_string(&mut output, &edit.replacement);
                output.push('}');
            }
            output.push_str("]}");
        }
        output.push_str("]}");
    }

    output.push(']');
    output
}

fn write_json_field(output: &mut String, name: &str, value: &str) {
    write_json_string(output, name);
    output.push(':');
    write_json_string(output, value);
}

fn write_json_string(output: &mut String, value: &str) {
    output.push('"');
    for character in value.chars() {
        match character {
            '"' => output.push_str("\\\""),
            '\\' => output.push_str("\\\\"),
            '\u{08}' => output.push_str("\\b"),
            '\u{0c}' => output.push_str("\\f"),
            '\n' => output.push_str("\\n"),
            '\r' => output.push_str("\\r"),
            '\t' => output.push_str("\\t"),
            '\u{00}'..='\u{1f}' => {
                write!(output, "\\u{:04x}", u32::from(character))
                    .expect("writing to a String cannot fail");
            }
            _ => output.push(character),
        }
    }
    output.push('"');
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::source::TextSize;

    fn size(value: usize) -> TextSize {
        TextSize::new(value).expect("small test offset")
    }

    fn range(start: usize, end: usize) -> TextRange {
        TextRange::new(size(start), size(end)).expect("ordered test range")
    }

    fn diagnostic(id: &str, code: &str, severity: Severity, range: TextRange) -> Diagnostic {
        Diagnostic {
            id: DiagnosticId::new(id),
            code: DiagnosticCode::new(code),
            severity,
            message: format!("message for {id}"),
            range,
            related: Vec::new(),
            fixes: Vec::new(),
        }
    }

    #[test]
    fn diagnostic_sort_is_stable_and_canonical() {
        let mut diagnostics = vec![
            diagnostic("b", "z-code", Severity::Warning, range(4, 5)),
            diagnostic("c", "a-code", Severity::Error, range(1, 3)),
            diagnostic("a", "a-code", Severity::Warning, range(1, 2)),
        ];

        sort_diagnostics(&mut diagnostics);

        assert_eq!(
            diagnostics
                .iter()
                .map(|diagnostic| diagnostic.id.as_str())
                .collect::<Vec<_>>(),
            ["a", "c", "b"]
        );
    }

    #[test]
    fn diagnostic_fix_sorts_non_conflicting_edits() {
        let fix = Fix::new(
            "fix whitespace",
            Applicability::Always,
            vec![
                TextEdit {
                    range: range(5, 6),
                    replacement: String::new(),
                },
                TextEdit {
                    range: range(1, 2),
                    replacement: " ".to_owned(),
                },
            ],
        )
        .expect("non-overlapping edits");

        assert_eq!(fix.edits()[0].range, range(1, 2));
        assert_eq!(fix.edits()[1].range, range(5, 6));
    }

    #[test]
    fn diagnostic_fix_rejects_duplicate_and_overlapping_edits() {
        let duplicate = vec![
            TextEdit {
                range: range(1, 2),
                replacement: "a".to_owned(),
            },
            TextEdit {
                range: range(1, 2),
                replacement: "b".to_owned(),
            },
        ];
        assert_eq!(
            Fix::new("duplicate", Applicability::Always, duplicate),
            Err(EditConflict {
                first: range(1, 2),
                second: range(1, 2),
                kind: EditConflictKind::Duplicate,
            })
        );

        let overlapping = vec![
            TextEdit {
                range: range(1, 4),
                replacement: String::new(),
            },
            TextEdit {
                range: range(3, 5),
                replacement: String::new(),
            },
        ];
        assert_eq!(
            Fix::new("overlap", Applicability::Maybe, overlapping),
            Err(EditConflict {
                first: range(1, 4),
                second: range(3, 5),
                kind: EditConflictKind::Overlap,
            })
        );
    }

    #[test]
    fn diagnostic_human_output_uses_one_based_line_and_column() {
        let source = "日本語\nproblem\n";
        let line_index = LineIndex::new(source).expect("valid source");
        let diagnostics = [Diagnostic {
            message: "問題です".to_owned(),
            ..diagnostic("parse-1", "parse-error", Severity::Error, range(10, 17))
        }];

        assert_eq!(
            render_human(&diagnostics, &line_index, PositionEncoding::Utf16),
            Ok("2:1: error[parse-error]: 問題です\n".to_owned())
        );
    }

    #[test]
    fn diagnostic_json_is_escaped_and_deterministic() {
        let diagnostics = [
            Diagnostic {
                message: "quote: \" and newline\n".to_owned(),
                related: vec![RelatedInformation {
                    message: "関連".to_owned(),
                    range: range(0, 1),
                }],
                fixes: vec![
                    Fix::new(
                        "replace",
                        Applicability::Always,
                        vec![TextEdit {
                            range: range(3, 4),
                            replacement: "\"".to_owned(),
                        }],
                    )
                    .expect("valid fix"),
                ],
                ..diagnostic("second", "b", Severity::Hint, range(3, 4))
            },
            diagnostic("first", "a", Severity::Warning, range(1, 2)),
        ];

        assert_eq!(
            render_json(&diagnostics),
            concat!(
                "[",
                "{\"id\":\"first\",\"code\":\"a\",\"severity\":\"warning\",",
                "\"range\":{\"start\":1,\"end\":2},\"message\":\"message for first\",",
                "\"related\":[],\"fixes\":[]},",
                "{\"id\":\"second\",\"code\":\"b\",\"severity\":\"hint\",",
                "\"range\":{\"start\":3,\"end\":4},",
                "\"message\":\"quote: \\\" and newline\\n\",",
                "\"related\":[{\"range\":{\"start\":0,\"end\":1},\"message\":\"関連\"}],",
                "\"fixes\":[{\"title\":\"replace\",\"applicability\":\"always\",",
                "\"edits\":[{\"range\":{\"start\":3,\"end\":4},",
                "\"replacement\":\"\\\"\"}]}]}",
                "]"
            )
        );
    }

    #[test]
    fn diagnostic_core_error_codes_are_stable() {
        assert_eq!(CoreErrorCode::InvalidInput.as_str(), "invalid-input");
        assert_eq!(CoreErrorCode::ParseFailed.as_str(), "parse-failed");
        assert_eq!(CoreErrorCode::LimitExceeded.as_str(), "limit-exceeded");
        assert_eq!(CoreErrorCode::Cancelled.as_str(), "cancelled");
        assert_eq!(
            CoreErrorCode::InternalInvariant.as_str(),
            "internal-invariant"
        );
    }
}
