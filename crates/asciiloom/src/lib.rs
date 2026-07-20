//! Core application boundary for AsciiLoom.
//!
//! Parsing and rendering will be implemented behind this API. The command-line
//! interface is intentionally kept in a separate module and only handles I/O.

use std::error::Error;
use std::fmt;

pub mod diagnostic;
pub mod formatter;
pub mod html;
pub mod lint;
pub mod parser;
pub mod source;
pub mod source_lines;

/// An operation supported by the AsciiLoom command-line application.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Operation {
    Convert,
    Check,
    Format,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CheckOutput {
    Human,
    Json,
}

/// An error produced while decoding or processing a document.
#[derive(Debug, Eq, PartialEq)]
pub enum ProcessError {
    InvalidUtf8 { valid_up_to: usize },
    Position(source::PositionError),
}

impl fmt::Display for ProcessError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidUtf8 { valid_up_to } => write!(
                formatter,
                "input is not valid UTF-8 (invalid byte starts at offset {valid_up_to})"
            ),
            Self::Position(error) => error.fmt(formatter),
        }
    }
}

impl Error for ProcessError {}

/// Decodes a document and performs the selected operation.
///
/// This first implementation deliberately preserves the input for conversion
/// and formatting. Later issues will replace these placeholders with the
/// parser, renderer, linter, and formatter.
pub fn process(operation: Operation, input: &[u8]) -> Result<String, ProcessError> {
    let source = std::str::from_utf8(input).map_err(|error| ProcessError::InvalidUtf8 {
        valid_up_to: error.valid_up_to(),
    })?;

    match operation {
        Operation::Convert => {
            let parsed = parser::parse(source).map_err(ProcessError::Position)?;
            Ok(html::render(&parsed.ast, &html::HtmlOptions::default()).html)
        }
        Operation::Format => formatter::format(source, &formatter::FormatConfig::default())
            .map(|output| output.formatted)
            .map_err(ProcessError::Position),
        Operation::Check => process_check_source(source, CheckOutput::Human),
    }
}

pub fn process_check(input: &[u8], output: CheckOutput) -> Result<String, ProcessError> {
    let source = std::str::from_utf8(input).map_err(|error| ProcessError::InvalidUtf8 {
        valid_up_to: error.valid_up_to(),
    })?;
    process_check_source(source, output)
}

fn process_check_source(source: &str, output: CheckOutput) -> Result<String, ProcessError> {
    let diagnostics =
        lint::lint(source, &lint::LintConfig::default()).map_err(ProcessError::Position)?;
    match output {
        CheckOutput::Human => {
            let line_index = source::LineIndex::new(source).map_err(ProcessError::Position)?;
            diagnostic::render_human(&diagnostics, &line_index, source::PositionEncoding::Utf16)
                .map_err(ProcessError::Position)
        }
        CheckOutput::Json => Ok(diagnostic::render_json(&diagnostics)),
    }
}

#[cfg(test)]
mod tests {
    use super::{CheckOutput, Operation, ProcessError, process, process_check};

    #[test]
    fn convert_renders_html() {
        let source = "日本語 😀\n";

        assert_eq!(
            process(Operation::Convert, source.as_bytes()),
            Ok("<p>日本語 😀</p>\n".to_owned())
        );
    }

    #[test]
    fn check_accepts_valid_input_without_output() {
        assert_eq!(process(Operation::Check, b"paragraph\n"), Ok(String::new()));
    }

    #[test]
    fn check_can_render_json() {
        assert_eq!(
            process_check(b"trailing \n", CheckOutput::Json),
            Ok(concat!(
                "[{\"id\":\"trailing-whitespace@8:9\",",
                "\"code\":\"trailing-whitespace\",\"severity\":\"warning\",",
                "\"range\":{\"start\":8,\"end\":9},",
                "\"message\":\"trailing whitespace\",\"related\":[],",
                "\"fixes\":[{\"title\":\"remove trailing whitespace\",",
                "\"applicability\":\"always\",\"edits\":[{\"range\":",
                "{\"start\":8,\"end\":9},\"replacement\":\"\"}]}]}]"
            )
            .to_owned())
        );
    }

    #[test]
    fn format_leaves_clean_input_unchanged() {
        assert_eq!(
            process(Operation::Format, b"paragraph\n"),
            Ok("paragraph\n".to_owned())
        );
    }

    #[test]
    fn invalid_utf8_reports_the_byte_offset() {
        assert_eq!(
            process(Operation::Convert, &[b'a', 0xff]),
            Err(ProcessError::InvalidUtf8 { valid_up_to: 1 })
        );
    }
}
