//! Command-line error categories and the exit status each reports.

use std::error::Error;
use std::fmt;
use std::io;
use std::path::PathBuf;

use adocweave::ParseError;
use adocweave_host::ExitStatus;

use crate::{commands, local_include, preview};

#[derive(Debug)]
pub(crate) enum CliError {
    Usage(String),
    Read {
        source_name: String,
        source: io::Error,
    },
    Write(io::Error),
    InvalidUtf8 {
        valid_up_to: usize,
    },
    Analysis(ParseError),
    Position(adocweave::text::PositionError),
    OutputLimit {
        limit: u32,
        actual: u64,
    },
    ResourceLimit(String),
    Include(local_include::LocalIncludeError),
    LocalTarget(adocweave_host::LocalTargetError),
    FormattingRequired,
    Stylesheet(String),
    Config(adocweave_config::ConfigError),
    ConfigAuthority(PathBuf),
    Path(String),
    ConcurrentModification(PathBuf),
    FixConflict(adocweave::output::diagnostics::EditConflict),
    Preview(preview::Error),
    Serialize(String),
}

impl fmt::Display for CliError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Usage(message) => formatter.write_str(message),
            Self::Read {
                source_name,
                source,
            } => write!(formatter, "could not read {source_name}: {source}"),
            Self::Write(source) => write!(formatter, "could not write output: {source}"),
            Self::InvalidUtf8 { valid_up_to } => write!(
                formatter,
                "input is not valid UTF-8 (invalid byte starts at offset {valid_up_to})"
            ),
            Self::Analysis(source) => source.fmt(formatter),
            Self::Position(source) => source.fmt(formatter),
            Self::OutputLimit { limit, actual } => {
                write!(
                    formatter,
                    "output bytes limit exceeded (limit {limit}, actual {actual})"
                )
            }
            Self::ResourceLimit(message) => formatter.write_str(message),
            Self::Include(source) => source.fmt(formatter),
            Self::LocalTarget(source) => source.fmt(formatter),
            Self::FormattingRequired => formatter.write_str("document is not formatted"),
            Self::Stylesheet(message) => formatter.write_str(message),
            Self::Config(source) => source.fmt(formatter),
            Self::ConfigAuthority(path) => write!(
                formatter,
                "project configuration cannot grant access outside the workspace: {}",
                path.display()
            ),
            Self::Path(message) => formatter.write_str(message),
            Self::ConcurrentModification(path) => write!(
                formatter,
                "input changed while preparing an atomic write: {}",
                path.display()
            ),
            Self::FixConflict(source) => write!(formatter, "conflicting automatic fixes: {source}"),
            Self::Preview(source) => source.fmt(formatter),
            Self::Serialize(message) => {
                write!(formatter, "cannot serialize diagnostics: {message}")
            }
        }
    }
}

impl Error for CliError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Read { source, .. } | Self::Write(source) => Some(source),
            Self::Analysis(source) => Some(source),
            Self::Position(source) => Some(source),
            Self::Include(source) => Some(source),
            Self::LocalTarget(source) => Some(source),
            Self::Config(source) => Some(source),
            Self::FixConflict(source) => Some(source),
            Self::Preview(source) => Some(source),
            Self::Usage(_)
            | Self::Serialize(_)
            | Self::InvalidUtf8 { .. }
            | Self::OutputLimit { .. }
            | Self::ResourceLimit(_)
            | Self::FormattingRequired
            | Self::Stylesheet(_)
            | Self::ConfigAuthority(_)
            | Self::Path(_)
            | Self::ConcurrentModification(_) => None,
        }
    }
}

impl CliError {
    /// Names the category a caller sees as the exit status.
    pub(crate) const fn exit_status(&self) -> ExitStatus {
        match self {
            // What the caller asked for cannot be acted on as written.
            Self::Usage(_) | Self::Path(_) | Self::Stylesheet(_) | Self::ConfigAuthority(_) => {
                ExitStatus::Usage
            }
            // A file, stream or resource could not be read or written. The input
            // may be fine; the surroundings were not.
            Self::Read { .. }
            | Self::Write(_)
            | Self::Include(_)
            | Self::LocalTarget(_)
            | Self::Config(_)
            | Self::ConcurrentModification(_)
            | Self::Preview(_) => ExitStatus::InputOutput,
            // A configured bound was reached, so the work stopped rather than
            // grew without limit.
            Self::OutputLimit { .. } | Self::ResourceLimit(_) => ExitStatus::LimitExceeded,
            // The document itself is the problem, which is what a diagnostic
            // reports. Everything left describes the document or the analysis of
            // it, so it shares the status diagnostics use.
            Self::InvalidUtf8 { .. }
            | Self::Analysis(_)
            | Self::Position(_)
            | Self::FormattingRequired
            | Self::FixConflict(_)
            | Self::Serialize(_) => ExitStatus::Diagnostics,
        }
    }
}

pub(crate) fn convert_error(error: commands::convert::Error) -> CliError {
    match error {
        commands::convert::Error::InvalidUtf8 { valid_up_to } => {
            CliError::InvalidUtf8 { valid_up_to }
        }
        commands::convert::Error::Analysis(source) => CliError::Analysis(source),
        commands::convert::Error::Html(source) => html_policy_error(source),
    }
}

pub(crate) fn check_error(error: commands::check::Error) -> CliError {
    match error {
        commands::check::Error::InvalidUtf8 { valid_up_to } => {
            CliError::InvalidUtf8 { valid_up_to }
        }
        commands::check::Error::Analysis(source) => CliError::Analysis(source),
        commands::check::Error::Position(source) => CliError::Position(source),
        commands::check::Error::Include(source) => CliError::Include(source),
        commands::check::Error::LocalTarget(source) => CliError::LocalTarget(source),
        commands::check::Error::FixConflict(source) => CliError::FixConflict(source),
        commands::check::Error::Serialize(message) => CliError::Serialize(message),
    }
}

pub(crate) fn format_error(error: commands::format::Error) -> CliError {
    match error {
        commands::format::Error::InvalidUtf8 { valid_up_to } => {
            CliError::InvalidUtf8 { valid_up_to }
        }
        commands::format::Error::Analysis(source) => CliError::Analysis(source),
        commands::format::Error::Position(source) => CliError::Position(source),
        commands::format::Error::FormattingRequired => CliError::FormattingRequired,
    }
}

pub(crate) fn preview_error(error: commands::preview::Error) -> CliError {
    match error {
        commands::preview::Error::Read {
            source_name,
            source,
        } => CliError::Read {
            source_name,
            source,
        },
        commands::preview::Error::Analysis(source) => CliError::Analysis(source),
        commands::preview::Error::Include(source) => CliError::Include(source),
        commands::preview::Error::Html(source) => html_policy_error(source),
        commands::preview::Error::Path(message) => CliError::Path(message),
        commands::preview::Error::Server(source) => CliError::Preview(source),
    }
}

pub(crate) fn html_policy_error(error: commands::html_policy::Error) -> CliError {
    match error {
        commands::html_policy::Error::Cancelled => CliError::Analysis(ParseError::Cancelled),
        commands::html_policy::Error::InvalidUtf8 { valid_up_to } => {
            CliError::InvalidUtf8 { valid_up_to }
        }
        commands::html_policy::Error::Read {
            source_name,
            source,
        } => CliError::Read {
            source_name,
            source,
        },
        commands::html_policy::Error::Stylesheet(message) => CliError::Stylesheet(message),
        commands::html_policy::Error::Usage(message) => CliError::Usage(message),
    }
}
