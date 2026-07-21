//! Core application boundary for AdocWeave.
//!
//! The command-line interface is a host adapter around this API and owns file
//! and standard-stream I/O. Parsing, diagnostics, formatting, and rendering
//! remain deterministic core operations over caller-provided input.

use std::error::Error;
use std::fmt;

pub mod attributes;
mod block_model;
mod budget;
pub mod catalog;
pub mod conformance;
pub mod core;
pub mod diagnostic;
pub mod document;
pub mod execution;
pub mod formatter;
pub mod html;
pub mod inline;
mod inline_model;
mod json;
pub mod limits;
pub mod lint;
mod lowering;
pub mod parser;
pub mod preprocessor;
pub mod projection;
pub mod reference;
pub mod resource;
pub mod source;
pub mod source_document;
pub mod structure;
pub mod substitution;
pub mod syntax;
mod syntax_builder;
mod syntax_diagnostics;
pub mod table;
pub mod url;
pub mod walker;

pub use core::{
    Analysis, CORE_API_VERSION, CORE_PROFILE_VERSION, CancellationCheck, CancellationToken, Engine,
    NeverCancel, ParseError, ParseOptions, SourceId,
};
pub use execution::{AnalysisRequest, AnalysisResult, DocumentRevision};

pub const PRODUCT_NAME: &str = "AdocWeave";
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// An operation supported by the AdocWeave command-line application.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Operation {
    Convert,
    Check,
    Format,
    Symbols,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CheckOutput {
    Human,
    Json,
}

/// An error produced while decoding or processing a document.
#[derive(Debug, Eq, PartialEq)]
pub enum ProcessError {
    InvalidUtf8 {
        valid_up_to: usize,
    },
    Position(source::PositionError),
    LimitExceeded {
        resource: &'static str,
        limit: u32,
        actual: u64,
    },
    UnsupportedSyntax,
    InternalInvariant,
}

impl fmt::Display for ProcessError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidUtf8 { valid_up_to } => write!(
                formatter,
                "input is not valid UTF-8 (invalid byte starts at offset {valid_up_to})"
            ),
            Self::Position(error) => error.fmt(formatter),
            Self::LimitExceeded {
                resource,
                limit,
                actual,
            } => write!(
                formatter,
                "{resource} limit exceeded (limit {limit}, actual {actual})"
            ),
            Self::UnsupportedSyntax => {
                formatter.write_str("unsupported syntax is forbidden in strict mode")
            }
            Self::InternalInvariant => formatter.write_str("internal processing invariant failed"),
        }
    }
}

impl Error for ProcessError {}

impl ProcessError {
    pub const fn code(&self) -> diagnostic::CoreErrorCode {
        match self {
            Self::InvalidUtf8 { .. } | Self::UnsupportedSyntax => {
                diagnostic::CoreErrorCode::InvalidInput
            }
            Self::Position(_) => diagnostic::CoreErrorCode::ParseFailed,
            Self::LimitExceeded { .. } => diagnostic::CoreErrorCode::LimitExceeded,
            Self::InternalInvariant => diagnostic::CoreErrorCode::InternalInvariant,
        }
    }
}

/// Decodes a document and performs the selected operation.
///
/// Resource limits and the configured unsupported-syntax policy apply
/// consistently to every operation.
pub fn process(operation: Operation, input: &[u8]) -> Result<String, ProcessError> {
    process_with_config(operation, input, &limits::ProcessConfig::default())
}

pub fn process_with_config(
    operation: Operation,
    input: &[u8],
    config: &limits::ProcessConfig,
) -> Result<String, ProcessError> {
    std::panic::catch_unwind(|| process_inner(operation, input, config))
        .unwrap_or(Err(ProcessError::InternalInvariant))
}

fn process_inner(
    operation: Operation,
    input: &[u8],
    config: &limits::ProcessConfig,
) -> Result<String, ProcessError> {
    enforce_limit("input bytes", config.limits.max_input_bytes, input.len())?;
    let source = std::str::from_utf8(input).map_err(|error| ProcessError::InvalidUtf8 {
        valid_up_to: error.valid_up_to(),
    })?;
    let longest_line = longest_line_bytes(source);
    enforce_limit("line bytes", config.limits.max_line_bytes, longest_line)?;

    let output = match operation {
        Operation::Convert => {
            let analysis = analyze_with_policy(source, config)?;
            Ok(html::render(analysis.ast(), &html::RenderPolicy::default()).html)
        }
        Operation::Format => {
            let analysis = analyze_with_policy(source, config)?;
            formatter::format_analysis(&analysis, &formatter::FormatConfig::default())
                .map(|output| output.formatted)
                .map_err(ProcessError::Position)
        }
        Operation::Symbols => {
            let parsed = analyze_with_policy(source, config)?;
            Ok(document::render_symbols_json(&document::document_symbols(
                parsed.ast(),
            )))
        }
        Operation::Check => process_check_source_with_config(source, CheckOutput::Human, config),
    }?;
    enforce_limit("output bytes", config.limits.max_output_bytes, output.len())?;
    Ok(output)
}

pub fn process_check(input: &[u8], output: CheckOutput) -> Result<String, ProcessError> {
    process_check_with_config(input, output, &limits::ProcessConfig::default())
}

pub fn process_check_with_config(
    input: &[u8],
    output: CheckOutput,
    config: &limits::ProcessConfig,
) -> Result<String, ProcessError> {
    enforce_limit("input bytes", config.limits.max_input_bytes, input.len())?;
    let source = std::str::from_utf8(input).map_err(|error| ProcessError::InvalidUtf8 {
        valid_up_to: error.valid_up_to(),
    })?;
    let longest_line = longest_line_bytes(source);
    enforce_limit("line bytes", config.limits.max_line_bytes, longest_line)?;
    let rendered =
        std::panic::catch_unwind(|| process_check_source_with_config(source, output, config))
            .unwrap_or(Err(ProcessError::InternalInvariant))?;
    enforce_limit(
        "output bytes",
        config.limits.max_output_bytes,
        rendered.len(),
    )?;
    Ok(rendered)
}

fn process_check_source_with_config(
    source: &str,
    output: CheckOutput,
    config: &limits::ProcessConfig,
) -> Result<String, ProcessError> {
    let analysis = analyze_with_policy(source, config)?;
    let diagnostics = &analysis.diagnostics();
    match output {
        CheckOutput::Human => diagnostic::render_human(
            diagnostics,
            analysis.source_document(),
            source::PositionEncoding::Utf16,
        )
        .map_err(ProcessError::Position),
        CheckOutput::Json => Ok(diagnostic::render_json(diagnostics)),
    }
}

fn analyze_with_policy(
    source: &str,
    config: &limits::ProcessConfig,
) -> Result<core::Analysis, ProcessError> {
    core::analyze(
        source,
        &core::ParseOptions {
            source_id: None,
            syntax_mode: config.syntax_mode,
            limits: config.limits,
            protected_attributes: std::collections::BTreeMap::new(),
            url_policy: crate::url::UrlPolicy::default(),
        },
    )
    .map_err(process_error_from_parse)
}

fn process_error_from_parse(error: core::ParseError) -> ProcessError {
    match error {
        core::ParseError::Position(error) => ProcessError::Position(error),
        core::ParseError::LimitExceeded {
            resource,
            limit,
            actual,
        } => ProcessError::LimitExceeded {
            resource,
            limit,
            actual,
        },
        core::ParseError::UnsupportedSyntax => ProcessError::UnsupportedSyntax,
        core::ParseError::Cancelled | core::ParseError::InternalInvariant => {
            ProcessError::InternalInvariant
        }
    }
}

fn enforce_limit(resource: &'static str, limit: u32, actual: usize) -> Result<(), ProcessError> {
    let comparable_limit = usize::try_from(limit).expect("u32 fits usize on supported targets");
    if actual > comparable_limit {
        Err(ProcessError::LimitExceeded {
            resource,
            limit,
            actual: u64::try_from(actual).expect("usize fits u64 on supported targets"),
        })
    } else {
        Ok(())
    }
}

fn longest_line_bytes(source: &str) -> usize {
    let bytes = source.as_bytes();
    let mut start = 0;
    let mut longest = 0;
    for (index, byte) in bytes.iter().enumerate() {
        if *byte == b'\n' {
            let end = if index > start && bytes[index - 1] == b'\r' {
                index - 1
            } else {
                index
            };
            longest = longest.max(end - start);
            start = index + 1;
        }
    }
    longest.max(bytes.len() - start)
}

#[cfg(test)]
mod tests {
    use super::{
        CheckOutput, Operation, ProcessError, process, process_check, process_check_with_config,
        process_with_config,
    };
    use crate::limits::{ProcessConfig, ProcessingLimits, SyntaxMode};

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

    fn limits(input: u32, output: u32, line: u32) -> ProcessConfig {
        ProcessConfig {
            limits: ProcessingLimits {
                max_input_bytes: input,
                max_output_bytes: output,
                max_line_bytes: line,
                ..ProcessingLimits::default()
            },
            syntax_mode: SyntaxMode::Permissive,
        }
    }

    #[test]
    fn limits_accept_below_and_exact_boundaries_then_reject_excess() {
        let expected = Ok("<p>abc</p>\n".to_owned());
        for config in [
            limits(4, 100, 100),
            limits(3, 100, 100),
            limits(100, 100, 4),
            limits(100, 100, 3),
            limits(100, 12, 100),
            limits(100, 11, 100),
        ] {
            assert_eq!(
                process_with_config(Operation::Convert, b"abc", &config),
                expected
            );
        }

        for (config, resource) in [
            (limits(2, 100, 100), "input bytes"),
            (limits(100, 100, 2), "line bytes"),
            (limits(100, 10, 100), "output bytes"),
        ] {
            assert!(matches!(
                process_with_config(Operation::Convert, b"abc", &config),
                Err(ProcessError::LimitExceeded { resource: found, .. })
                    if found == resource
            ));
        }
    }

    #[test]
    fn limits_cap_diagnostics_deterministically() {
        let mut config = limits(100, 1_000, 100);
        config.limits.max_diagnostics = 1;
        let output = process_check_with_config(b"one \ntwo \n", CheckOutput::Json, &config)
            .expect("within limits");

        assert_eq!(output.matches("\"code\"").count(), 1);
    }

    #[test]
    fn limits_apply_configured_inline_depth() {
        let mut config = limits(100, 1_000, 100);
        config.limits.max_inline_depth = 1;
        let output = process_check_with_config(b"*outer _inner_*", CheckOutput::Json, &config)
            .expect("within limits");

        assert!(output.contains("\"code\":\"nesting-limit-exceeded\""));
    }

    #[test]
    fn security_modes_escape_html_and_apply_strict_policy_to_every_operation() {
        assert_eq!(
            process(Operation::Convert, b"<script>alert(1)</script>"),
            Ok("<p>&lt;script&gt;alert(1)&lt;/script&gt;</p>\n".to_owned())
        );

        let strict = ProcessConfig {
            syntax_mode: SyntaxMode::Strict,
            ..ProcessConfig::default()
        };
        for operation in [Operation::Convert, Operation::Format, Operation::Symbols] {
            assert_eq!(
                process_with_config(operation, b"[role=raw]", &strict),
                Err(ProcessError::UnsupportedSyntax)
            );
        }
        assert_eq!(
            process_check_with_config(b"[role=raw]", CheckOutput::Human, &strict),
            Err(ProcessError::UnsupportedSyntax)
        );
        assert_eq!(
            process_with_config(Operation::Convert, b"[role=raw]", &ProcessConfig::default()),
            Ok("<p>[role=raw]</p>\n".to_owned())
        );
    }
}
