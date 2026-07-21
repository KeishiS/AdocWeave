use std::env;
use std::error::Error;
use std::fmt;
use std::fs;
use std::io::{self, Read, Write};
use std::path::PathBuf;
use std::process::ExitCode;

use adocweave::preprocessor::{PreprocessedAnalysis, ProjectionLimits};
use adocweave::source::{PositionEncoding, SourceDocument};
use adocweave::{CheckOutput, Operation, process, process_check};

mod local_include;

const HELP: &str = "\
AdocWeave command-line interface

Usage:
  adocweave <COMMAND> [FILE]

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
  --include   Enable bounded local include processing
  --base-dir DIR    Resolve root document includes from DIR
  --allow-root DIR  Permit include resources below DIR; repeatable
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
    Process(adocweave::ProcessError),
    Include(local_include::LocalIncludeError),
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
            Self::Include(source) => source.fmt(formatter),
            Self::FormattingRequired => formatter.write_str("document is not formatted"),
        }
    }
}

impl Error for CliError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Read { source, .. } | Self::Write(source) => Some(source),
            Self::Process(source) => Some(source),
            Self::Include(source) => Some(source),
            Self::Usage(_) | Self::FormattingRequired => None,
        }
    }
}

struct Arguments {
    operation: Operation,
    input: Option<PathBuf>,
    json: bool,
    format_check: bool,
    include: bool,
    base_dir: Option<PathBuf>,
    allowed_roots: Vec<PathBuf>,
}

enum Action {
    Run(Arguments),
    Help,
    Version { json: bool },
}

fn parse_arguments(mut arguments: impl Iterator<Item = String>) -> Result<Action, CliError> {
    let Some(command) = arguments.next() else {
        return Err(CliError::Usage("a command is required".to_owned()));
    };

    if matches!(command.as_str(), "-h" | "--help" | "help") {
        return Ok(Action::Help);
    }
    if matches!(command.as_str(), "-V" | "--version") {
        let json = match arguments.next().as_deref() {
            None => false,
            Some("--json") if arguments.next().is_none() => true,
            Some(argument) => {
                return Err(CliError::Usage(format!(
                    "unexpected version argument: {argument}"
                )));
            }
        };
        return Ok(Action::Version { json });
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
    let mut include = false;
    let mut base_dir = None;
    let mut allowed_roots = Vec::new();
    while let Some(argument) = arguments.next() {
        match argument.as_str() {
            "-h" | "--help" => return Ok(Action::Help),
            "--json" if operation == Operation::Check => json = true,
            "--check" if operation == Operation::Format => format_check = true,
            "--include" => include = true,
            "--base-dir" => {
                let value = arguments
                    .next()
                    .ok_or_else(|| CliError::Usage("--base-dir requires a directory".to_owned()))?;
                base_dir = Some(PathBuf::from(value));
            }
            "--allow-root" => {
                let value = arguments.next().ok_or_else(|| {
                    CliError::Usage("--allow-root requires a directory".to_owned())
                })?;
                allowed_roots.push(PathBuf::from(value));
            }
            "-" if input.is_none() && !stdin_selected => stdin_selected = true,
            _ if input.is_none() && !stdin_selected => input = Some(PathBuf::from(argument)),
            _ => {
                return Err(CliError::Usage(format!(
                    "unexpected argument after input: {argument}"
                )));
            }
        }
    }
    if !include && (base_dir.is_some() || !allowed_roots.is_empty()) {
        return Err(CliError::Usage(
            "--base-dir and --allow-root require --include".to_owned(),
        ));
    }

    Ok(Action::Run(Arguments {
        operation,
        input,
        json,
        format_check,
        include,
        base_dir,
        allowed_roots,
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
        Action::Version { json } => {
            if json {
                println!(
                    "{}",
                    serde_json::json!({
                        "name": "adocweave",
                        "packageVersion": adocweave::VERSION,
                        "contracts": {
                            "coreProfile": adocweave::CORE_PROFILE_VERSION,
                            "coreApi": adocweave::CORE_API_VERSION,
                            "html": adocweave::html::HTML_CONTRACT_VERSION,
                            "projection": adocweave::projection::PROJECTION_CONTRACT_VERSION,
                            "conformance": adocweave::conformance::CONFORMANCE_CONTRACT_VERSION,
                        }
                    })
                );
            } else {
                println!("adocweave {}", adocweave::VERSION);
            }
            Ok(())
        }
        Action::Run(arguments) => {
            let input_path = arguments.input.clone();
            let input = read_input(arguments.input)?;
            let mut prepared = None;
            let processed = if arguments.include {
                let source = std::str::from_utf8(&input).map_err(|error| {
                    CliError::Process(adocweave::ProcessError::InvalidUtf8 {
                        valid_up_to: error.valid_up_to(),
                    })
                })?;
                let base_dir = match arguments.base_dir {
                    Some(base_dir) => base_dir,
                    None => input_path
                        .as_deref()
                        .and_then(std::path::Path::parent)
                        .filter(|path| !path.as_os_str().is_empty())
                        .map(PathBuf::from)
                        .ok_or_else(|| {
                            CliError::Usage(
                                "--include with standard input requires --base-dir".to_owned(),
                            )
                        })?,
                };
                let source_id = input_path.as_ref().map_or_else(
                    || "<stdin>".to_owned(),
                    |path| {
                        path.canonicalize()
                            .unwrap_or_else(|_| path.clone())
                            .to_string_lossy()
                            .into_owned()
                    },
                );
                let include_input = local_include::prepare(
                    source,
                    Some(source_id),
                    &base_dir,
                    &arguments.allowed_roots,
                )
                .map_err(CliError::Include)?;
                let processed = if arguments.operation == Operation::Format {
                    input.clone()
                } else {
                    include_input.document.source.as_bytes().to_vec()
                };
                prepared = Some(include_input);
                processed
            } else {
                input.clone()
            };
            let output = if arguments.operation == Operation::Check
                && let Some(prepared) = prepared.as_ref()
            {
                check_preprocessed(prepared, arguments.json).map_err(CliError::Include)
            } else if arguments.operation == Operation::Check {
                process_check(
                    &processed,
                    if arguments.json {
                        CheckOutput::Json
                    } else {
                        CheckOutput::Human
                    },
                )
                .map_err(CliError::Process)
            } else if arguments.operation == Operation::Format && arguments.format_check {
                let source = std::str::from_utf8(&input).map_err(|error| {
                    CliError::Process(adocweave::ProcessError::InvalidUtf8 {
                        valid_up_to: error.valid_up_to(),
                    })
                })?;
                let output = process(Operation::Format, &input).map_err(CliError::Process)?;
                if output != source {
                    return Err(CliError::FormattingRequired);
                }
                Ok(String::new())
            } else {
                process(arguments.operation, &processed).map_err(CliError::Process)
            }?;
            io::stdout()
                .write_all(output.as_bytes())
                .map_err(CliError::Write)
        }
    }
}

fn check_preprocessed(
    prepared: &local_include::PreparedInput,
    json: bool,
) -> Result<String, local_include::LocalIncludeError> {
    let engine = adocweave::Engine::new(adocweave::ParseOptions::default());
    let analysis = engine
        .analyze(&prepared.document.source)
        .map_err(|error| local_include::LocalIncludeError::Analysis(error.to_string()))?;
    let projected = PreprocessedAnalysis {
        document: prepared.document.clone(),
        analysis,
    }
    .project_origins(ProjectionLimits::default())
    .map_err(|error| local_include::LocalIncludeError::Analysis(error.to_string()))?;
    if json {
        let values = projected
            .diagnostics
            .iter()
            .flat_map(|diagnostic| {
                diagnostic.origins.iter().map(move |origin| {
                    serde_json::json!({
                        "id": diagnostic.diagnostic.id.as_str(),
                        "code": diagnostic.diagnostic.code.as_str(),
                        "severity": diagnostic.diagnostic.severity.as_str(),
                        "message": diagnostic.diagnostic.message,
                        "sourceId": origin.source_id.as_ref().map(adocweave::SourceId::as_str),
                        "range": {
                            "start": origin.range.start().to_u32(),
                            "end": origin.range.end().to_u32()
                        }
                    })
                })
            })
            .collect::<Vec<_>>();
        return serde_json::to_string(&values)
            .map_err(|error| local_include::LocalIncludeError::Analysis(error.to_string()));
    }

    let mut output = String::new();
    for diagnostic in &projected.diagnostics {
        for origin in &diagnostic.origins {
            let source_id = origin
                .source_id
                .as_ref()
                .map_or("<unknown>", adocweave::SourceId::as_str);
            let source = prepared.sources.get(source_id).ok_or_else(|| {
                local_include::LocalIncludeError::MissingSource(source_id.to_owned())
            })?;
            let index =
                SourceDocument::new(source).map_err(local_include::LocalIncludeError::Position)?;
            let position = index
                .offset_to_position(origin.range.start(), PositionEncoding::Utf8)
                .map_err(local_include::LocalIncludeError::Position)?;
            use std::fmt::Write as _;
            writeln!(
                output,
                "{}:{}:{}: {}[{}]: {}",
                source_id,
                position.line + 1,
                position.character + 1,
                diagnostic.diagnostic.severity.as_str(),
                diagnostic.diagnostic.code.as_str(),
                diagnostic.diagnostic.message,
            )
            .expect("writing to a String cannot fail");
        }
    }
    Ok(output)
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("adocweave: {error}");
            eprintln!("Try 'adocweave --help' for more information.");
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

    #[test]
    fn include_options_are_explicit_and_repeatable() {
        let Action::Run(parsed) = parse_arguments(arguments(&[
            "convert",
            "--include",
            "--base-dir",
            "docs",
            "--allow-root",
            ".",
            "--allow-root",
            "vendor",
            "manual.adoc",
        ]))
        .expect("valid arguments") else {
            panic!("expected run action");
        };
        assert!(parsed.include);
        assert_eq!(
            parsed.base_dir.as_deref(),
            Some(std::path::Path::new("docs"))
        );
        assert_eq!(parsed.allowed_roots.len(), 2);
    }
}
