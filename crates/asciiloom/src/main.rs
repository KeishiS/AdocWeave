use std::env;
use std::error::Error;
use std::fmt;
use std::fs;
use std::io::{self, Read, Write};
use std::path::PathBuf;
use std::process::ExitCode;

use asciiloom::{CheckOutput, Operation, process, process_check};

const HELP: &str = "\
AsciiLoom command-line interface

Usage:
  asciiloom <COMMAND> [FILE]

Commands:
  convert  Convert an AsciiDoc document
  check    Check an AsciiDoc document
  format   Format an AsciiDoc document
  symbols  Print document symbols as JSON
  help     Print this message

Arguments:
  [FILE]   Input file; omit it or use '-' to read standard input

Options:
  --json      Emit check diagnostics as JSON
  --check     Check formatting without writing formatted text
  -V, --version  Print version
  -h, --help  Print help
";

#[derive(Debug)]
enum CliError {
    Usage(String),
    Read {
        source_name: String,
        source: io::Error,
    },
    Write(io::Error),
    Process(asciiloom::ProcessError),
    FormattingRequired,
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
            Self::Process(source) => source.fmt(formatter),
            Self::FormattingRequired => formatter.write_str("document is not formatted"),
        }
    }
}

impl Error for CliError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Read { source, .. } | Self::Write(source) => Some(source),
            Self::Process(source) => Some(source),
            Self::Usage(_) | Self::FormattingRequired => None,
        }
    }
}

struct Arguments {
    operation: Operation,
    input: Option<PathBuf>,
    json: bool,
    format_check: bool,
}

enum Action {
    Run(Arguments),
    Help,
    Version,
}

fn parse_arguments(mut arguments: impl Iterator<Item = String>) -> Result<Action, CliError> {
    let Some(command) = arguments.next() else {
        return Err(CliError::Usage("a command is required".to_owned()));
    };

    if matches!(command.as_str(), "-h" | "--help" | "help") {
        return Ok(Action::Help);
    }
    if matches!(command.as_str(), "-V" | "--version") {
        return Ok(Action::Version);
    }

    let operation = match command.as_str() {
        "convert" => Operation::Convert,
        "check" => Operation::Check,
        "format" => Operation::Format,
        "symbols" => Operation::Symbols,
        _ => return Err(CliError::Usage(format!("unknown command: {command}"))),
    };

    let mut input = None;
    let mut stdin_selected = false;
    let mut json = false;
    let mut format_check = false;
    for argument in arguments {
        match argument.as_str() {
            "-h" | "--help" => return Ok(Action::Help),
            "--json" if operation == Operation::Check => json = true,
            "--check" if operation == Operation::Format => format_check = true,
            "-" if input.is_none() && !stdin_selected => stdin_selected = true,
            _ if input.is_none() && !stdin_selected => input = Some(PathBuf::from(argument)),
            _ => {
                return Err(CliError::Usage(format!(
                    "unexpected argument after input: {argument}"
                )));
            }
        }
    }

    Ok(Action::Run(Arguments {
        operation,
        input,
        json,
        format_check,
    }))
}

fn read_input(path: Option<PathBuf>) -> Result<Vec<u8>, CliError> {
    match path {
        Some(path) => fs::read(&path).map_err(|source| CliError::Read {
            source_name: path.display().to_string(),
            source,
        }),
        None => {
            let mut input = Vec::new();
            io::stdin()
                .read_to_end(&mut input)
                .map_err(|source| CliError::Read {
                    source_name: "standard input".to_owned(),
                    source,
                })?;
            Ok(input)
        }
    }
}

fn run() -> Result<(), CliError> {
    match parse_arguments(env::args().skip(1))? {
        Action::Help => {
            print!("{HELP}");
            Ok(())
        }
        Action::Version => {
            println!("asciiloom {}", asciiloom::VERSION);
            Ok(())
        }
        Action::Run(arguments) => {
            let input = read_input(arguments.input)?;
            let output = if arguments.operation == Operation::Check {
                process_check(
                    &input,
                    if arguments.json {
                        CheckOutput::Json
                    } else {
                        CheckOutput::Human
                    },
                )
            } else if arguments.operation == Operation::Format && arguments.format_check {
                let source = std::str::from_utf8(&input).map_err(|error| {
                    CliError::Process(asciiloom::ProcessError::InvalidUtf8 {
                        valid_up_to: error.valid_up_to(),
                    })
                })?;
                let output = asciiloom::formatter::format(
                    source,
                    &asciiloom::formatter::FormatConfig::default(),
                )
                .map_err(|error| CliError::Process(asciiloom::ProcessError::Position(error)))?;
                if output.changed() {
                    return Err(CliError::FormattingRequired);
                }
                Ok(String::new())
            } else {
                process(arguments.operation, &input)
            }
            .map_err(CliError::Process)?;
            io::stdout()
                .write_all(output.as_bytes())
                .map_err(CliError::Write)
        }
    }
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("asciiloom: {error}");
            eprintln!("Try 'asciiloom --help' for more information.");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Action, Operation, parse_arguments};

    fn arguments(values: &[&str]) -> impl Iterator<Item = String> {
        values.iter().map(ToString::to_string)
    }

    #[test]
    fn parses_file_input() {
        let Action::Run(parsed) =
            parse_arguments(arguments(&["convert", "document.adoc"])).expect("valid arguments")
        else {
            panic!("expected run action");
        };

        assert_eq!(parsed.operation, Operation::Convert);
        assert_eq!(
            parsed.input.as_deref(),
            Some(std::path::Path::new("document.adoc"))
        );
    }

    #[test]
    fn dash_selects_standard_input() {
        let Action::Run(parsed) =
            parse_arguments(arguments(&["check", "-"])).expect("valid arguments")
        else {
            panic!("expected run action");
        };

        assert_eq!(parsed.operation, Operation::Check);
        assert!(parsed.input.is_none());
    }

    #[test]
    fn all_commands_support_help() {
        for command in ["convert", "check", "format", "symbols"] {
            assert!(matches!(
                parse_arguments(arguments(&[command, "--help"])),
                Ok(Action::Help)
            ));
        }
    }

    #[test]
    fn check_accepts_json_before_or_after_input() {
        for values in [
            ["check", "--json", "document.adoc"],
            ["check", "document.adoc", "--json"],
        ] {
            let Action::Run(parsed) = parse_arguments(arguments(&values)).expect("valid arguments")
            else {
                panic!("expected run action");
            };
            assert!(parsed.json);
            assert_eq!(
                parsed.input.as_deref(),
                Some(std::path::Path::new("document.adoc"))
            );
        }
    }

    #[test]
    fn format_accepts_check_flag() {
        let Action::Run(parsed) =
            parse_arguments(arguments(&["format", "--check", "document.adoc"]))
                .expect("valid arguments")
        else {
            panic!("expected run action");
        };
        assert!(parsed.format_check);
    }
}
