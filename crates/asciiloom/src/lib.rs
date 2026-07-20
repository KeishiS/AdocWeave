//! Core application boundary for AsciiLoom.
//!
//! Parsing and rendering will be implemented behind this API. The command-line
//! interface is intentionally kept in a separate module and only handles I/O.

use std::error::Error;
use std::fmt;

pub mod diagnostic;
pub mod source;
pub mod source_lines;

/// An operation supported by the AsciiLoom command-line application.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Operation {
    Convert,
    Check,
    Format,
}

/// An error produced while decoding or processing a document.
#[derive(Debug, Eq, PartialEq)]
pub enum ProcessError {
    InvalidUtf8 { valid_up_to: usize },
}

impl fmt::Display for ProcessError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidUtf8 { valid_up_to } => write!(
                formatter,
                "input is not valid UTF-8 (invalid byte starts at offset {valid_up_to})"
            ),
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
        Operation::Convert | Operation::Format => Ok(source.to_owned()),
        Operation::Check => Ok(String::new()),
    }
}

#[cfg(test)]
mod tests {
    use super::{Operation, ProcessError, process};

    #[test]
    fn convert_preserves_utf8_input() {
        let source = "= 日本語 😀\n";

        assert_eq!(
            process(Operation::Convert, source.as_bytes()),
            Ok(source.to_owned())
        );
    }

    #[test]
    fn check_accepts_valid_input_without_output() {
        assert_eq!(process(Operation::Check, b"paragraph\n"), Ok(String::new()));
    }

    #[test]
    fn format_preserves_input_until_formatter_is_implemented() {
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
