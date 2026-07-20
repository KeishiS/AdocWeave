use std::env;
use std::error::Error;
use std::fmt;
use std::fs;
use std::io::{self, Read, Write};
use std::path::PathBuf;
use std::process::ExitCode;

use asciiloom::{Operation, process};

const HELP: &str = "\
AsciiLoom command-line interface

Usage:
  asciiloom <COMMAND> [FILE]

Commands:
  convert  Convert an AsciiDoc document
  check    Check an AsciiDoc document
  format   Format an AsciiDoc document
  help     Print this message

Arguments:
  [FILE]   Input file; omit it or use '-' to read standard input

Options:
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
        }
    }
}

impl Error for CliError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Read { source, .. } | Self::Write(source) => Some(source),
            Self::Process(source) => Some(source),
            Self::Usage(_) => None,
        }
    }
}

struct Arguments {
    operation: Operation,
    input: Option<PathBuf>,
}

enum Action {
    Run(Arguments),
    Help,
}

fn parse_arguments(mut arguments: impl Iterator<Item = String>) -> Result<Action, CliError> {
    let Some(command) = arguments.next() else {
        return Err(CliError::Usage("a command is required".to_owned()));
    };

    if matches!(command.as_str(), "-h" | "--help" | "help") {
        return Ok(Action::Help);
    }

    let operation = match command.as_str() {
        "convert" => Operation::Convert,
        "check" => Operation::Check,
        "format" => Operation::Format,
        _ => return Err(CliError::Usage(format!("unknown command: {command}"))),
    };

    let input = match arguments.next() {
        Some(argument) if matches!(argument.as_str(), "-h" | "--help") => {
            return Ok(Action::Help);
        }
        Some(argument) if argument == "-" => None,
        Some(argument) => Some(PathBuf::from(argument)),
        None => None,
    };

    if let Some(argument) = arguments.next() {
        return Err(CliError::Usage(format!(
            "unexpected argument after input: {argument}"
        )));
    }

    Ok(Action::Run(Arguments { operation, input }))
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
        Action::Run(arguments) => {
            let input = read_input(arguments.input)?;
            let output = process(arguments.operation, &input).map_err(CliError::Process)?;
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
        for command in ["convert", "check", "format"] {
            assert!(matches!(
                parse_arguments(arguments(&[command, "--help"])),
                Ok(Action::Help)
            ));
        }
    }
}
