use std::env;
use std::error::Error;
use std::fmt;
use std::fs;
use std::io::{self, Read, Write};
use std::net::{IpAddr, Ipv4Addr};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::atomic::AtomicBool;

use adocweave::output::diagnostics as diagnostic;
use adocweave::{AnalysisOptions, OutputLimits, ParseError};

mod check_output;
mod commands;
mod file_workflow;
mod local_include;
mod local_target;
mod preview;

static PREVIEW_SHUTDOWN: AtomicBool = AtomicBool::new(false);

#[cfg(unix)]
fn install_preview_signal_handlers() {
    extern "C" fn shutdown(_: libc::c_int) {
        PREVIEW_SHUTDOWN.store(true, std::sync::atomic::Ordering::Release);
    }
    // SAFETY: the handler performs only a lock-free atomic store, and the
    // process retains the static flag for its entire lifetime.
    unsafe {
        let handler = shutdown as *const () as libc::sighandler_t;
        libc::signal(libc::SIGINT, handler);
        libc::signal(libc::SIGTERM, handler);
    }
}

#[cfg(not(unix))]
fn install_preview_signal_handlers() {}

use check_output::{
    CheckOutcome, DiagnosticCounts, DiagnosticFormat, FailOn, sarif_log, sarif_results,
};
use commands::check::Options as CheckOptions;
use commands::format::Options as FormatOptions;
use commands::html_policy::StylesheetArgument;
use commands::model::{CommandId, LookupError};
use file_workflow::{PendingWrite, atomic_write_all, colorize_lines};
const DEFAULT_PREVIEW_PORT: u16 = 4000;
const DEFAULT_PREVIEW_DEBOUNCE_MS: u64 = 100;

#[derive(Debug)]
enum CliError {
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
            | Self::InvalidUtf8 { .. }
            | Self::OutputLimit { .. }
            | Self::FormattingRequired
            | Self::Stylesheet(_)
            | Self::ConfigAuthority(_)
            | Self::Path(_)
            | Self::ConcurrentModification(_) => None,
        }
    }
}

fn convert_error(error: commands::convert::Error) -> CliError {
    match error {
        commands::convert::Error::InvalidUtf8 { valid_up_to } => {
            CliError::InvalidUtf8 { valid_up_to }
        }
        commands::convert::Error::Analysis(source) => CliError::Analysis(source),
        commands::convert::Error::Html(source) => html_policy_error(source),
    }
}

fn check_error(error: commands::check::Error) -> CliError {
    match error {
        commands::check::Error::InvalidUtf8 { valid_up_to } => {
            CliError::InvalidUtf8 { valid_up_to }
        }
        commands::check::Error::Analysis(source) => CliError::Analysis(source),
        commands::check::Error::Position(source) => CliError::Position(source),
        commands::check::Error::Include(source) => CliError::Include(source),
        commands::check::Error::LocalTarget(source) => CliError::LocalTarget(source),
        commands::check::Error::FixConflict(source) => CliError::FixConflict(source),
    }
}

fn format_error(error: commands::format::Error) -> CliError {
    match error {
        commands::format::Error::InvalidUtf8 { valid_up_to } => {
            CliError::InvalidUtf8 { valid_up_to }
        }
        commands::format::Error::Analysis(source) => CliError::Analysis(source),
        commands::format::Error::Position(source) => CliError::Position(source),
        commands::format::Error::FormattingRequired => CliError::FormattingRequired,
    }
}

fn preview_error(error: commands::preview::Error) -> CliError {
    match error {
        commands::preview::Error::Read {
            source_name,
            source,
        } => CliError::Read {
            source_name,
            source,
        },
        commands::preview::Error::InvalidUtf8 { valid_up_to } => {
            CliError::InvalidUtf8 { valid_up_to }
        }
        commands::preview::Error::Analysis(source) => CliError::Analysis(source),
        commands::preview::Error::Include(source) => CliError::Include(source),
        commands::preview::Error::Html(source) => html_policy_error(source),
        commands::preview::Error::Path(message) => CliError::Path(message),
        commands::preview::Error::Server(source) => CliError::Preview(source),
    }
}

fn html_policy_error(error: commands::html_policy::Error) -> CliError {
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

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
enum ColorChoice {
    #[default]
    Auto,
    Always,
    Never,
}

#[derive(Debug)]
enum CommandOptions {
    Convert {
        complete: bool,
        css: Vec<StylesheetArgument>,
    },
    Preview {
        css: Vec<StylesheetArgument>,
        bind: IpAddr,
        port: u16,
        debounce_ms: u64,
    },
    Check(CheckOptions),
    Format(FormatOptions),
    Symbols,
    ConfigShow,
}

impl CommandOptions {
    const fn command_id(&self) -> CommandId {
        match self {
            Self::Convert { .. } => CommandId::Convert,
            Self::Preview { .. } => CommandId::Preview,
            Self::Check(_) => CommandId::Check,
            Self::Format(_) => CommandId::Format,
            Self::Symbols => CommandId::Symbols,
            Self::ConfigShow => CommandId::ConfigShow,
        }
    }
}

struct Arguments {
    command: CommandOptions,
    input: Option<PathBuf>,
    additional_inputs: Vec<PathBuf>,
    glob_patterns: Vec<String>,
    include: bool,
    base_dir: Option<PathBuf>,
    allowed_roots: Vec<PathBuf>,
    project_root: Option<PathBuf>,
    config_path: Option<PathBuf>,
    no_config: bool,
    color: ColorChoice,
}

enum Action {
    Run(Box<Arguments>),
    Help { command: Option<CommandId> },
    Version { json: bool },
    Completion { shell: CompletionShell },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum CompletionShell {
    Bash,
    Zsh,
    Fish,
    PowerShell,
}

fn parse_arguments(arguments: impl Iterator<Item = String>) -> Result<Action, CliError> {
    let arguments = arguments.collect::<Vec<_>>();
    let Some(command) = arguments.first() else {
        return Err(CliError::Usage("a command is required".to_owned()));
    };

    if matches!(command.as_str(), "-h" | "--help") {
        return Ok(Action::Help { command: None });
    }
    if matches!(command.as_str(), "-V" | "--version") {
        let mut arguments = arguments.into_iter().skip(1);
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
    let (command_id, consumed) = commands::model::lookup(&arguments).map_err(|error| {
        CliError::Usage(match error {
            LookupError::UnknownCommand(value) => format!("unknown command: {value}"),
            LookupError::MissingSubcommand(parent) => {
                format!("{parent} requires a command")
            }
            LookupError::UnknownSubcommand { parent, value } => {
                format!("unknown {parent} command: {value}")
            }
        })
    })?;
    let mut arguments = arguments.into_iter().skip(consumed);
    if command_id == CommandId::Help {
        return Ok(Action::Help { command: None });
    }
    if command_id == CommandId::Completion {
        let shell = match arguments.next().as_deref() {
            Some("bash") => CompletionShell::Bash,
            Some("zsh") => CompletionShell::Zsh,
            Some("fish") => CompletionShell::Fish,
            Some("powershell") => CompletionShell::PowerShell,
            Some(value) => {
                return Err(CliError::Usage(format!(
                    "unknown completion shell: {value}"
                )));
            }
            None => return Err(CliError::Usage("completion requires a shell".to_owned())),
        };
        if let Some(argument) = arguments.next() {
            return Err(CliError::Usage(format!(
                "unexpected completion argument: {argument}"
            )));
        }
        return Ok(Action::Completion { shell });
    }

    let mut input = None;
    let mut additional_inputs = Vec::new();
    let mut glob_patterns = Vec::new();
    let mut stdin_selected = false;
    let mut diagnostic_format = DiagnosticFormat::Human;
    let mut format_selected = false;
    let mut fail_on = FailOn::Error;
    let mut summary = false;
    let mut fix = false;
    let mut list_rules = false;
    let mut enabled_rules = Vec::new();
    let mut format_check = false;
    let mut format_write = false;
    let mut format_diff = false;
    let mut dry_run = false;
    let mut include = false;
    let mut base_dir = None;
    let mut allowed_roots = Vec::new();
    let mut local_targets = false;
    let mut project_root = None;
    let mut complete = false;
    let mut css = Vec::new();
    let mut bind = IpAddr::V4(Ipv4Addr::LOCALHOST);
    let mut port = DEFAULT_PREVIEW_PORT;
    let mut debounce_ms = DEFAULT_PREVIEW_DEBOUNCE_MS;
    let mut allow_external = false;
    let mut config_path = None;
    let mut no_config = false;
    let mut color = ColorChoice::Auto;
    while let Some(argument) = arguments.next() {
        match argument.as_str() {
            "-h" | "--help" => {
                return Ok(Action::Help {
                    command: Some(command_id),
                });
            }
            "--config" => {
                if no_config {
                    return Err(CliError::Usage(
                        "--config cannot be combined with --no-config".to_owned(),
                    ));
                }
                let value = arguments
                    .next()
                    .ok_or_else(|| CliError::Usage("--config requires a file".to_owned()))?;
                if config_path.replace(PathBuf::from(value)).is_some() {
                    return Err(CliError::Usage(
                        "--config cannot be specified more than once".to_owned(),
                    ));
                }
            }
            "--no-config" => {
                if config_path.is_some() {
                    return Err(CliError::Usage(
                        "--no-config cannot be combined with --config".to_owned(),
                    ));
                }
                no_config = true;
            }
            "--color" => {
                let value = arguments
                    .next()
                    .ok_or_else(|| CliError::Usage("--color requires a value".to_owned()))?;
                color = match value.as_str() {
                    "auto" => ColorChoice::Auto,
                    "always" => ColorChoice::Always,
                    "never" => ColorChoice::Never,
                    _ => return Err(CliError::Usage(format!("unknown color choice: {value}"))),
                };
            }
            "--glob" if matches!(command_id, CommandId::Check | CommandId::Format) => {
                let value = arguments
                    .next()
                    .ok_or_else(|| CliError::Usage("--glob requires a pattern".to_owned()))?;
                glob_patterns.push(value);
            }
            "--json" if command_id == CommandId::Check => {
                if format_selected && diagnostic_format != DiagnosticFormat::Json {
                    return Err(CliError::Usage(
                        "--json conflicts with another --format value".to_owned(),
                    ));
                }
                diagnostic_format = DiagnosticFormat::Json;
                format_selected = true;
            }
            "--format" if command_id == CommandId::Check => {
                let value = arguments
                    .next()
                    .ok_or_else(|| CliError::Usage("--format requires a value".to_owned()))?;
                let parsed = DiagnosticFormat::parse(&value)?;
                if format_selected && parsed != diagnostic_format {
                    return Err(CliError::Usage(
                        "--format cannot be specified with conflicting values".to_owned(),
                    ));
                }
                diagnostic_format = parsed;
                format_selected = true;
            }
            "--fail-on" if command_id == CommandId::Check => {
                let value = arguments
                    .next()
                    .ok_or_else(|| CliError::Usage("--fail-on requires a level".to_owned()))?;
                fail_on = FailOn::parse(&value)?;
            }
            "--summary" if matches!(command_id, CommandId::Check | CommandId::Format) => {
                summary = true
            }
            "--fix" if command_id == CommandId::Check => fix = true,
            "--dry-run" if matches!(command_id, CommandId::Check | CommandId::Format) => {
                dry_run = true
            }
            "--list-rules" if command_id == CommandId::Check => list_rules = true,
            "--enable-rule" if command_id == CommandId::Check => {
                let code = arguments
                    .next()
                    .ok_or_else(|| CliError::Usage("--enable-rule requires a code".to_owned()))?;
                let descriptor = diagnostic::lint_rule(&code).ok_or_else(|| {
                    CliError::Usage(format!("unknown or non-enableable rule: {code}"))
                })?;
                if descriptor.default_enabled {
                    return Err(CliError::Usage(format!(
                        "rule is already enabled by default: {code}"
                    )));
                }
                if !enabled_rules.contains(&descriptor.id) {
                    enabled_rules.push(descriptor.id);
                }
            }
            "--check" if command_id == CommandId::Format => format_check = true,
            "--write" if command_id == CommandId::Format => format_write = true,
            "--diff" if command_id == CommandId::Format => format_diff = true,
            "--include" => include = true,
            "--local-targets" if command_id == CommandId::Check => local_targets = true,
            "--project-root" if command_id == CommandId::Check => {
                let value = arguments.next().ok_or_else(|| {
                    CliError::Usage("--project-root requires a directory".to_owned())
                })?;
                project_root = Some(PathBuf::from(value));
            }
            "--complete" if command_id == CommandId::Convert => complete = true,
            "--css" if matches!(command_id, CommandId::Convert | CommandId::Preview) => {
                let value = arguments
                    .next()
                    .ok_or_else(|| CliError::Usage("--css requires a file".to_owned()))?;
                css.push(StylesheetArgument::File(PathBuf::from(value)));
            }
            "--css-url" if matches!(command_id, CommandId::Convert | CommandId::Preview) => {
                let value = arguments
                    .next()
                    .ok_or_else(|| CliError::Usage("--css-url requires a URL".to_owned()))?;
                css.push(StylesheetArgument::Url(value));
            }
            "--bind" if command_id == CommandId::Preview => {
                let value = arguments
                    .next()
                    .ok_or_else(|| CliError::Usage("--bind requires an address".to_owned()))?;
                bind = value
                    .parse()
                    .map_err(|_| CliError::Usage(format!("invalid bind address: {value}")))?;
            }
            "--port" if command_id == CommandId::Preview => {
                let value = arguments
                    .next()
                    .ok_or_else(|| CliError::Usage("--port requires a value".to_owned()))?;
                port = value
                    .parse()
                    .map_err(|_| CliError::Usage(format!("invalid port: {value}")))?;
            }
            "--debounce-ms" if command_id == CommandId::Preview => {
                let value = arguments
                    .next()
                    .ok_or_else(|| CliError::Usage("--debounce-ms requires a value".to_owned()))?;
                debounce_ms = value
                    .parse()
                    .map_err(|_| CliError::Usage(format!("invalid debounce interval: {value}")))?;
                if debounce_ms == 0 {
                    return Err(CliError::Usage(
                        "--debounce-ms must be greater than zero".to_owned(),
                    ));
                }
            }
            "--allow-external" if command_id == CommandId::Preview => allow_external = true,
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
            "-" => {
                return Err(CliError::Usage(
                    "standard input cannot be combined with file paths".to_owned(),
                ));
            }
            _ if input.is_none() && !stdin_selected => input = Some(PathBuf::from(argument)),
            _ if matches!(command_id, CommandId::Check | CommandId::Format) && !stdin_selected => {
                additional_inputs.push(PathBuf::from(argument));
            }
            _ => {
                return Err(CliError::Usage(format!(
                    "unexpected argument after input: {argument}"
                )));
            }
        }
    }
    if usize::from(format_check) + usize::from(format_write) + usize::from(format_diff) > 1 {
        return Err(CliError::Usage(
            "--check, --write, and --diff are mutually exclusive".to_owned(),
        ));
    }
    if stdin_selected && !glob_patterns.is_empty() {
        return Err(CliError::Usage(
            "standard input cannot be combined with --glob".to_owned(),
        ));
    }
    if dry_run
        && !matches!(
            command_id,
            CommandId::Format if format_write
        )
        && !(command_id == CommandId::Check && fix)
    {
        return Err(CliError::Usage(
            "--dry-run requires format --write or check --fix".to_owned(),
        ));
    }
    if local_targets != project_root.is_some() {
        return Err(CliError::Usage(
            "--local-targets and --project-root must be used together".to_owned(),
        ));
    }
    if command_id == CommandId::Preview {
        if stdin_selected || input.is_none() || !additional_inputs.is_empty() {
            return Err(CliError::Usage(
                "preview requires exactly one input file".to_owned(),
            ));
        }
        if !bind.is_loopback() && !allow_external {
            return Err(CliError::Usage(
                "a non-loopback --bind requires --allow-external".to_owned(),
            ));
        }
    }
    if local_targets && !allowed_roots.is_empty() {
        return Err(CliError::Usage(
            "--allow-root cannot be combined with --local-targets; --project-root is the boundary"
                .to_owned(),
        ));
    }
    if command_id == CommandId::ConfigShow
        && (input.is_some()
            || !additional_inputs.is_empty()
            || !glob_patterns.is_empty()
            || stdin_selected
            || include
            || base_dir.is_some()
            || !allowed_roots.is_empty()
            || project_root.is_some()
            || complete
            || !css.is_empty()
            || color != ColorChoice::Auto)
    {
        return Err(CliError::Usage(
            "config show only accepts --config or --no-config".to_owned(),
        ));
    }
    if list_rules {
        if diagnostic_format != DiagnosticFormat::Json {
            return Err(CliError::Usage("--list-rules requires --json".to_owned()));
        }
        if input.is_some()
            || !additional_inputs.is_empty()
            || !glob_patterns.is_empty()
            || stdin_selected
            || include
            || base_dir.is_some()
            || !allowed_roots.is_empty()
            || local_targets
            || project_root.is_some()
            || !enabled_rules.is_empty()
            || fix
            || dry_run
        {
            return Err(CliError::Usage(
                "--list-rules cannot be combined with document input or include options".to_owned(),
            ));
        }
    }

    let command = match command_id {
        CommandId::Convert => CommandOptions::Convert { complete, css },
        CommandId::Preview => CommandOptions::Preview {
            css,
            bind,
            port,
            debounce_ms,
        },
        CommandId::Check => CommandOptions::Check(CheckOptions {
            format: diagnostic_format,
            fail_on,
            summary,
            fix,
            dry_run,
            list_rules,
            enabled_rules,
        }),
        CommandId::Format => CommandOptions::Format(FormatOptions {
            check: format_check,
            write: format_write,
            diff: format_diff,
            dry_run,
            summary,
        }),
        CommandId::Symbols => CommandOptions::Symbols,
        CommandId::ConfigShow => CommandOptions::ConfigShow,
        CommandId::Completion | CommandId::Help => {
            unreachable!("public utility commands are handled before option parsing")
        }
    };
    Ok(Action::Run(Box::new(Arguments {
        command,
        input,
        additional_inputs,
        glob_patterns,
        include,
        base_dir,
        allowed_roots,
        project_root,
        config_path,
        no_config,
        color,
    })))
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

fn decode_input(input: &[u8]) -> Result<&str, CliError> {
    std::str::from_utf8(input).map_err(|error| CliError::InvalidUtf8 {
        valid_up_to: error.valid_up_to(),
    })
}

fn finish_output(output: String) -> Result<String, CliError> {
    let limit = OutputLimits::default().max_output_bytes;
    if output.len() > usize::try_from(limit).expect("u32 fits usize on supported targets") {
        return Err(CliError::OutputLimit {
            limit,
            actual: u64::try_from(output.len()).expect("usize fits u64"),
        });
    }
    Ok(output)
}

struct IncludePreparation<'request> {
    source: &'request str,
    source_id: String,
    base_dir: &'request Path,
    source_base: &'request Path,
    project_root: Option<&'request Path>,
    allowed_roots: &'request [PathBuf],
    limits: adocweave_host::ResourceLimits,
    preprocess: &'request adocweave::preprocess::PreprocessOptions,
}

fn prepare_includes(
    request: IncludePreparation<'_>,
) -> Result<local_include::PreparedInput, local_include::LocalIncludeError> {
    if let Some(project_root) = request.project_root {
        local_include::prepare_local(
            request.source,
            request.source_id,
            request.base_dir,
            request.source_base,
            project_root,
            request.limits,
            request.preprocess,
        )
    } else {
        local_include::prepare(
            request.source,
            Some(request.source_id),
            request.base_dir,
            request.allowed_roots,
            request.limits,
            request.preprocess,
        )
    }
}

fn process_check(
    input: &[u8],
    check: &CheckOptions,
    source_id: &str,
    analysis_options: &AnalysisOptions,
    preprocess_options: &adocweave::preprocess::PreprocessOptions,
    resource_limits: adocweave_host::ResourceLimits,
    local: Option<(&std::path::Path, &std::path::Path, &str)>,
) -> Result<CheckOutcome, CliError> {
    commands::check::process(
        input,
        check,
        source_id,
        analysis_options,
        preprocess_options,
        resource_limits,
        local.map(|(base, root, source_id)| commands::check::LocalContext {
            base,
            root,
            source_id,
        }),
    )
    .map_err(check_error)
}

fn load_project_config(
    arguments: &Arguments,
) -> Result<Option<adocweave_config::ConfigSnapshot>, CliError> {
    let boundary = env::current_dir().map_err(|source| CliError::Read {
        source_name: "current directory".to_owned(),
        source,
    })?;
    let start = arguments.input.as_deref().unwrap_or(&boundary);
    load_project_config_at(arguments, start, &boundary)
}

fn load_project_config_at(
    arguments: &Arguments,
    start: &std::path::Path,
    boundary: &std::path::Path,
) -> Result<Option<adocweave_config::ConfigSnapshot>, CliError> {
    if arguments.no_config {
        return Ok(None);
    }
    if let Some(path) = &arguments.config_path {
        return adocweave_config::ConfigSnapshot::load(path)
            .map(Some)
            .map_err(CliError::Config);
    }
    if !start.exists() {
        return Ok(None);
    }
    match adocweave_config::discover_and_load(start, boundary) {
        Ok(snapshot) => Ok(snapshot),
        Err(error) if error.code == adocweave_config::ConfigErrorCode::OutsideBoundary => Ok(None),
        Err(error) => Err(CliError::Config(error)),
    }
}

fn validate_project_config_authority(
    config: &adocweave_config::ResolvedProjectConfig,
    resources: bool,
    local_targets: bool,
    stylesheets: bool,
) -> Result<(), CliError> {
    let boundary = env::current_dir()
        .and_then(fs::canonicalize)
        .map_err(|source| CliError::Read {
            source_name: "current directory".to_owned(),
            source,
        })?;
    let paths = config
        .resources
        .roots
        .iter()
        .filter(|_| resources)
        .chain(
            config
                .local_targets
                .project_root
                .iter()
                .filter(|_| local_targets),
        )
        .chain(config.html.stylesheet_files.iter().filter(|_| stylesheets));
    for path in paths {
        let canonical = fs::canonicalize(path).map_err(|source| CliError::Read {
            source_name: path.display().to_string(),
            source,
        })?;
        if !canonical.starts_with(&boundary) {
            return Err(CliError::ConfigAuthority(path.clone()));
        }
    }
    Ok(())
}

fn collect_input_paths(arguments: &Arguments) -> Result<Vec<PathBuf>, CliError> {
    const MAX_SCAN_ENTRIES: usize = 100_000;

    let mut pending = arguments
        .input
        .iter()
        .chain(&arguments.additional_inputs)
        .cloned()
        .collect::<Vec<_>>();
    for pattern in &arguments.glob_patterns {
        let matches = glob::glob(pattern)
            .map_err(|error| CliError::Path(format!("invalid glob pattern {pattern}: {error}")))?;
        for path in matches {
            pending.push(
                path.map_err(|error| CliError::Path(format!("cannot read glob match: {error}")))?,
            );
        }
    }
    pending.sort();
    let mut files = std::collections::BTreeSet::new();
    let mut scanned_entries = 0_usize;
    while let Some(path) = pending.pop() {
        let metadata = fs::symlink_metadata(&path).map_err(|source| CliError::Read {
            source_name: path.display().to_string(),
            source,
        })?;
        if metadata.file_type().is_symlink() {
            return Err(CliError::Path(format!(
                "input paths must not be symbolic links: {}",
                path.display()
            )));
        }
        if metadata.is_dir() {
            let mut children = Vec::new();
            for child in fs::read_dir(&path).map_err(|source| CliError::Read {
                source_name: path.display().to_string(),
                source,
            })? {
                children.push(child.map_err(|source| CliError::Read {
                    source_name: path.display().to_string(),
                    source,
                })?);
                scanned_entries += 1;
                if scanned_entries > MAX_SCAN_ENTRIES {
                    return Err(CliError::Path(
                        "directory scan entry limit exceeded".to_owned(),
                    ));
                }
            }
            children.sort_by_key(fs::DirEntry::file_name);
            for entry in children.into_iter().rev() {
                let path = entry.path();
                let metadata = fs::symlink_metadata(&path).map_err(|source| CliError::Read {
                    source_name: path.display().to_string(),
                    source,
                })?;
                if metadata.file_type().is_symlink() {
                    continue;
                } else if metadata.is_dir()
                    || path.extension().and_then(|value| value.to_str()) == Some("adoc")
                {
                    pending.push(path);
                }
            }
            continue;
        }
        if !metadata.is_file() {
            return Err(CliError::Path(format!(
                "input is not a regular file: {}",
                path.display()
            )));
        }
        files.insert(path.canonicalize().map_err(|source| CliError::Read {
            source_name: path.display().to_string(),
            source,
        })?);
        if files.len() > adocweave_host::ResourceLimits::default().max_files {
            return Err(CliError::Path(
                "input file limit exceeded while scanning directories".to_owned(),
            ));
        }
    }
    Ok(files.into_iter().collect())
}

fn apply_safe_fixes(
    input: &[u8],
    check: &CheckOptions,
    analysis_options: &AnalysisOptions,
) -> Result<Vec<u8>, CliError> {
    commands::check::apply_safe_fixes(input, check, analysis_options).map_err(check_error)
}

fn run_multi_path(arguments: &Arguments) -> Result<Option<ExitCode>, CliError> {
    let paths = collect_input_paths(arguments)?;
    let directory_selected = arguments.input.as_ref().is_some_and(|path| path.is_dir());
    let explicit_path_mode = matches!(
        arguments.command,
        CommandOptions::Format(options) if options.uses_explicit_path_mode()
    ) || matches!(
        arguments.command,
        CommandOptions::Check(CheckOptions { fix: true, .. })
    );
    if paths.len() <= 1
        && arguments.additional_inputs.is_empty()
        && arguments.glob_patterns.is_empty()
        && !directory_selected
        && !explicit_path_mode
    {
        return Ok(None);
    }
    if paths.is_empty() {
        return Err(CliError::Path(
            "no AsciiDoc files matched the input paths".to_owned(),
        ));
    }
    let boundary = env::current_dir().map_err(|source| CliError::Read {
        source_name: "current directory".to_owned(),
        source,
    })?;
    match &arguments.command {
        CommandOptions::Format(options) => {
            if !options.supports_multiple_inputs() {
                return Err(CliError::Usage(
                    "multiple format inputs require --check, --write, or --diff".to_owned(),
                ));
            }
            let mut workflow = commands::format::BatchWorkflow::new(*options, paths.len());
            for path in &paths {
                let snapshot = load_project_config_at(arguments, path, &boundary)?;
                let config = snapshot.as_ref().map_or_else(
                    adocweave_config::ResolvedProjectConfig::default,
                    |snapshot| snapshot.config.clone(),
                );
                let original = fs::read(path).map_err(|source| CliError::Read {
                    source_name: path.display().to_string(),
                    source,
                })?;
                let include = arguments.include || config.resources.include;
                if !include && (arguments.base_dir.is_some() || !arguments.allowed_roots.is_empty())
                {
                    return Err(CliError::Usage(
                        "--base-dir and --allow-root require include processing".to_owned(),
                    ));
                }
                if include {
                    validate_project_config_authority(
                        &config,
                        arguments.allowed_roots.is_empty(),
                        false,
                        false,
                    )?;
                    let source = decode_input(&original)?;
                    let source_base = path.parent().expect("canonical input path has a parent");
                    let base_dir = arguments.base_dir.as_deref().unwrap_or(source_base);
                    let allowed_roots = if arguments.allowed_roots.is_empty() {
                        &config.resources.roots
                    } else {
                        &arguments.allowed_roots
                    };
                    local_include::prepare(
                        source,
                        Some(path.to_string_lossy().into_owned()),
                        base_dir,
                        allowed_roots,
                        config.resources.limits,
                        &config.preprocess,
                    )
                    .map_err(CliError::Include)?;
                }
                let format_config = commands::format::format_config(*options, &original, &config);
                let formatted =
                    commands::format::process(&original, &config.analysis, &format_config)
                        .map_err(format_error)?
                        .into_bytes();
                workflow
                    .record(path.clone(), original, formatted)
                    .map_err(format_error)?;
            }
            let outcome = workflow.finish();
            let summary = options.summary.then(|| outcome.summary());
            if !outcome.pending_writes.is_empty() {
                atomic_write_all(
                    outcome
                        .pending_writes
                        .into_iter()
                        .map(|write| PendingWrite {
                            path: write.path,
                            original: write.original,
                            replacement: write.replacement,
                        })
                        .collect(),
                )?;
            }
            if !outcome.output.is_empty() {
                let output = finish_output(colorize_lines(&outcome.output, arguments.color))?;
                print!("{output}");
            }
            if let Some(summary) = summary {
                eprintln!("{summary}");
            }
            Ok(Some(if outcome.formatting_required {
                ExitCode::FAILURE
            } else {
                ExitCode::SUCCESS
            }))
        }
        CommandOptions::Check(check) => {
            let mut output = String::new();
            let mut machine_results = Vec::new();
            let mut counts = DiagnosticCounts::default();
            let mut pending = Vec::new();
            let mut changed = 0_usize;
            for path in &paths {
                let snapshot = load_project_config_at(arguments, path, &boundary)?;
                let config = snapshot.as_ref().map_or_else(
                    adocweave_config::ResolvedProjectConfig::default,
                    |snapshot| snapshot.config.clone(),
                );
                let original = fs::read(path).map_err(|source| CliError::Read {
                    source_name: path.display().to_string(),
                    source,
                })?;
                let checked = if check.fix {
                    apply_safe_fixes(&original, check, &config.analysis)?
                } else {
                    original.clone()
                };
                if check.fix && checked != original {
                    changed += 1;
                    if !check.dry_run {
                        pending.push(PendingWrite {
                            path: path.clone(),
                            original,
                            replacement: checked.clone(),
                        });
                    }
                }
                let source_id = path.to_string_lossy();
                let project_root = arguments.project_root.clone().or_else(|| {
                    config
                        .local_targets
                        .enabled
                        .then(|| config.local_targets.project_root.clone())
                        .flatten()
                });
                let include = arguments.include || config.resources.include;
                if !include && (arguments.base_dir.is_some() || !arguments.allowed_roots.is_empty())
                {
                    return Err(CliError::Usage(
                        "--base-dir and --allow-root require include processing".to_owned(),
                    ));
                }
                validate_project_config_authority(
                    &config,
                    include && arguments.allowed_roots.is_empty(),
                    project_root.is_some() && arguments.project_root.is_none(),
                    false,
                )?;
                let source_base = path
                    .parent()
                    .expect("canonical input path has a parent")
                    .to_path_buf();
                let local_context = project_root
                    .as_ref()
                    .map(|root| (source_base.as_path(), root.as_path(), source_id.as_ref()));
                let outcome = if include {
                    let source = decode_input(&checked)?;
                    let base_dir = arguments
                        .base_dir
                        .as_deref()
                        .unwrap_or(source_base.as_path());
                    let allowed_roots = if arguments.allowed_roots.is_empty() {
                        &config.resources.roots
                    } else {
                        &arguments.allowed_roots
                    };
                    let mut prepared = prepare_includes(IncludePreparation {
                        source,
                        source_id: source_id.to_string(),
                        base_dir,
                        source_base: &source_base,
                        project_root: project_root.as_deref(),
                        allowed_roots,
                        limits: config.resources.limits,
                        preprocess: &config.preprocess,
                    })
                    .map_err(CliError::Include)?;
                    check_preprocessed(&mut prepared, check, &config.analysis)?
                } else {
                    process_check(
                        &checked,
                        check,
                        &source_id,
                        &config.analysis,
                        &config.preprocess,
                        config.resources.limits,
                        local_context,
                    )?
                };
                counts.merge(outcome.counts);
                if check.format == DiagnosticFormat::Json {
                    let mut values =
                        serde_json::from_str::<Vec<serde_json::Value>>(&outcome.output)
                            .expect("check JSON is an array");
                    for value in &mut values {
                        if let Some(object) = value.as_object_mut() {
                            object.entry("sourceId").or_insert_with(|| {
                                serde_json::Value::String(source_id.to_string())
                            });
                        }
                    }
                    machine_results.extend(values);
                } else if check.format == DiagnosticFormat::Sarif {
                    machine_results.extend(sarif_results(&outcome.output));
                } else {
                    output.push_str(&outcome.output);
                }
            }
            if !pending.is_empty() {
                atomic_write_all(pending)?;
            }
            if check.format == DiagnosticFormat::Json {
                output =
                    serde_json::to_string(&machine_results).expect("diagnostics are serializable");
            } else if check.format == DiagnosticFormat::Sarif {
                output = sarif_log(machine_results);
            }
            if check.format == DiagnosticFormat::Human {
                let output = finish_output(colorize_lines(&output, arguments.color))?;
                print!("{output}");
            } else {
                let output = finish_output(output)?;
                print!("{output}");
            }
            if check.summary {
                if check.fix {
                    eprintln!("adocweave check: {}, changed={changed}", counts.summary());
                } else {
                    eprintln!("adocweave check: {}", counts.summary());
                }
            }
            Ok(Some(if counts.fails(check.fail_on) {
                ExitCode::FAILURE
            } else {
                ExitCode::SUCCESS
            }))
        }
        _ => Err(CliError::Usage(
            "multiple paths are supported only by check and format".to_owned(),
        )),
    }
}

fn completion_script(shell: CompletionShell) -> String {
    render_completion_script(shell, &commands::model::completion_tree())
}

fn render_completion_script(
    shell: CompletionShell,
    tree: &commands::model::CompletionTree,
) -> String {
    let shell_words = |values: &[&str]| values.join(" ");
    let powershell_words = |values: &[&str]| {
        values
            .iter()
            .map(|value| format!("'{value}'"))
            .collect::<Vec<_>>()
            .join(",")
    };
    let mut contract = format!("# adocweave-command-tree root={}\n", tree.roots.join(","));
    for group in &tree.nested {
        contract.push_str(&format!(
            "# adocweave-command-tree parent={} children={}\n",
            group.parent.join("/"),
            group.children.join(",")
        ));
    }
    let rendered = match shell {
        CompletionShell::Bash => {
            let declarations = tree
                .nested
                .iter()
                .enumerate()
                .map(|(index, group)| {
                    format!(
                        "  local nested_{index}=\"{}\"",
                        shell_words(&group.children)
                    )
                })
                .collect::<Vec<_>>()
                .join("\n");
            let branches = tree
                .nested
                .iter()
                .enumerate()
                .map(|(index, group)| {
                    let conditions = group
                        .parent
                        .iter()
                        .enumerate()
                        .map(|(position, token)| {
                            format!("${{COMP_WORDS[{position_plus_one}]}} == {token}", position_plus_one = position + 1)
                        })
                        .collect::<Vec<_>>()
                        .join(" && ");
                    format!(
                        "  elif [[ ${{COMP_CWORD}} -eq {word_index} && {conditions} ]]; then\n    COMPREPLY=( $(compgen -W \"${{nested_{index}}}\" -- \"${{COMP_WORDS[COMP_CWORD]}}\") )",
                        word_index = group.parent.len() + 1,
                    )
                })
                .collect::<Vec<_>>()
                .join("\n");
            r#"_adocweave() {
  local commands="@ROOTS@"
@DECLARATIONS@
  local options="--format --fail-on --summary --fix --check --write --diff --dry-run --config --no-config --include --base-dir --allow-root --local-targets --project-root --complete --css --css-url --bind --port --debounce-ms --allow-external --help --version"
  if [[ ${COMP_CWORD} -eq 1 ]]; then
    COMPREPLY=( $(compgen -W "${commands}" -- "${COMP_WORDS[COMP_CWORD]}") )
@BRANCHES@
  else
    COMPREPLY=( $(compgen -W "${options}" -f -- "${COMP_WORDS[COMP_CWORD]}") )
  fi
}
complete -F _adocweave adocweave
"#
            .replace("@ROOTS@", &shell_words(&tree.roots))
            .replace("@DECLARATIONS@", &declarations)
            .replace("@BRANCHES@", &branches)
        }
        CompletionShell::Zsh => {
            let branches = tree
                .nested
                .iter()
                .map(|group| {
                    let conditions = group
                        .parent
                        .iter()
                        .enumerate()
                        .map(|(position, token)| {
                            format!("$words[{}] == {token}", position + 2)
                        })
                        .collect::<Vec<_>>()
                        .join(" && ");
                    format!(
                        "  elif [[ $CURRENT -eq {current} && {conditions} ]]; then\n    _values 'commands below {parent}' {children}",
                        current = group.parent.len() + 2,
                        parent = group.parent.join(" "),
                        children = shell_words(&group.children),
                    )
                })
                .collect::<Vec<_>>()
                .join("\n");
            r#"#compdef adocweave
_adocweave() {
  if (( CURRENT == 2 )); then
    _values 'commands' @ROOTS@
@BRANCHES@
  else
    _values 'arguments' \
      --format --fail-on --summary --fix --check --write --diff --dry-run \
      --config --no-config --include --base-dir --allow-root --local-targets \
      --project-root --complete --css --css-url --bind --port --debounce-ms --allow-external
  fi
}
compdef _adocweave adocweave
"#
            .replace("@ROOTS@", &shell_words(&tree.roots))
            .replace("@BRANCHES@", &branches)
        }
        CompletionShell::Fish => {
            let nested = tree
                .nested
                .iter()
                .map(|group| {
                    format!(
                        "complete -c adocweave -f -n '__adocweave_at_path {}' -a '{}'",
                        group.parent.join(" "),
                        shell_words(&group.children),
                    )
                })
                .collect::<Vec<_>>()
                .join("\n");
            r#"function __adocweave_at_path
  set -l expected $argv
  set -l words (commandline -opc)
  test (count $words) -eq (math (count $expected) + 1); or return 1
  for index in (seq (count $expected))
    test $words[(math $index + 1)] = $expected[$index]; or return 1
  end
end
complete -c adocweave -f -n '__fish_use_subcommand' -a '@ROOTS@'
@NESTED@
complete -c adocweave -l format -x -a 'human json github sarif'
complete -c adocweave -l fail-on -x -a 'error warning never'
complete -c adocweave -l config -r
complete -c adocweave -l write
complete -c adocweave -l diff
complete -c adocweave -l fix
"#
            .replace("@ROOTS@", &shell_words(&tree.roots))
            .replace("@NESTED@", &nested)
        }
        CompletionShell::PowerShell => {
            let mut groups = tree.nested.iter().collect::<Vec<_>>();
            groups.sort_by_key(|group| std::cmp::Reverse(group.parent.len()));
            let nested = groups
                .into_iter()
                .map(|group| {
                    let conditions = group
                        .parent
                        .iter()
                        .enumerate()
                        .map(|(position, token)| {
                            format!("$words[{}] -eq '{token}'", position + 1)
                        })
                        .collect::<Vec<_>>()
                        .join(" -and ");
                    format!(
                        "  }} elseif ({conditions} -and ($words.Count -eq {parent_count} -or ($words.Count -eq {child_count} -and $wordToComplete -ne ''))) {{\n    @({children})",
                        parent_count = group.parent.len() + 1,
                        child_count = group.parent.len() + 2,
                        children = powershell_words(&group.children),
                    )
                })
                .collect::<Vec<_>>()
                .join("\n");
            r#"Register-ArgumentCompleter -Native -CommandName adocweave -ScriptBlock {
  param($wordToComplete, $commandAst, $cursorPosition)
  $words = @($commandAst.CommandElements | ForEach-Object { $_.Value })
  $candidates = if ($false) {
    @()
@NESTED@
  } elseif ($words.Count -le 2) {
    @(@ROOTS@)
  } else {
    @('--format','--fail-on','--summary','--fix','--check','--write','--diff',
      '--dry-run','--config','--no-config','--include','--base-dir','--allow-root',
      '--bind','--port','--debounce-ms','--allow-external')
  }
  $candidates |
    Where-Object { $_ -like "$wordToComplete*" } |
    ForEach-Object { [System.Management.Automation.CompletionResult]::new($_, $_, 'ParameterValue', $_) }
}
"#
            .replace("@ROOTS@", &powershell_words(&tree.roots))
            .replace("@NESTED@", &nested)
        }
    };
    format!("{contract}{rendered}")
}

fn run() -> Result<ExitCode, CliError> {
    match parse_arguments(env::args().skip(1))? {
        Action::Help { command } => {
            let help = command.map_or_else(commands::model::root_help, |id| {
                commands::model::spec(id)
                    .help
                    .expect("document commands have command help")
                    .to_owned()
            });
            print!("{help}");
            Ok(ExitCode::SUCCESS)
        }
        Action::Version { json } => {
            if json {
                println!(
                    "{}",
                    serde_json::json!({
                        "name": "adocweave",
                        "packageVersion": adocweave::VERSION,
                    })
                );
            } else {
                println!("adocweave {}", adocweave::VERSION);
            }
            Ok(ExitCode::SUCCESS)
        }
        Action::Completion { shell } => {
            print!("{}", completion_script(shell));
            Ok(ExitCode::SUCCESS)
        }
        Action::Run(arguments) => {
            if let Some(exit_code) = run_multi_path(&arguments)? {
                return Ok(exit_code);
            }
            let config_snapshot = load_project_config(&arguments)?;
            if matches!(arguments.command, CommandOptions::ConfigShow) {
                let outcome = commands::config::run(config_snapshot.as_ref());
                println!("{}", outcome.output);
                return Ok(ExitCode::SUCCESS);
            }
            let project_config = config_snapshot.as_ref().map_or_else(
                adocweave_config::ResolvedProjectConfig::default,
                |snapshot| snapshot.config.clone(),
            );
            let command_id = arguments.command.command_id();
            let include = arguments.include || project_config.resources.include;
            if !include && (arguments.base_dir.is_some() || !arguments.allowed_roots.is_empty()) {
                return Err(CliError::Usage(
                    "--base-dir and --allow-root require include processing".to_owned(),
                ));
            }
            let allowed_roots = if arguments.allowed_roots.is_empty() {
                project_config.resources.roots.clone()
            } else {
                arguments.allowed_roots.clone()
            };
            let project_root = arguments.project_root.clone().or_else(|| {
                project_config
                    .local_targets
                    .enabled
                    .then(|| project_config.local_targets.project_root.clone())
                    .flatten()
            });
            validate_project_config_authority(
                &project_config,
                include && arguments.allowed_roots.is_empty(),
                project_root.is_some() && arguments.project_root.is_none(),
                matches!(command_id, CommandId::Convert | CommandId::Preview),
            )?;
            if matches!(
                &arguments.command,
                CommandOptions::Check(CheckOptions {
                    list_rules: true,
                    ..
                })
            ) {
                let output = diagnostic::render_lint_rule_catalog_json();
                io::stdout()
                    .write_all(output.as_bytes())
                    .map_err(CliError::Write)?;
                return Ok(ExitCode::SUCCESS);
            }
            if let CommandOptions::Preview {
                css,
                bind,
                port,
                debounce_ms,
            } = &arguments.command
            {
                let input_path = arguments
                    .input
                    .as_deref()
                    .expect("preview parser requires an input path");
                PREVIEW_SHUTDOWN.store(false, std::sync::atomic::Ordering::Release);
                install_preview_signal_handlers();
                commands::preview::run(
                    commands::preview::RunRequest {
                        input_path,
                        include,
                        base_dir: arguments.base_dir.as_deref(),
                        allowed_roots: &allowed_roots,
                        project_root: project_root.as_deref(),
                        project: &project_config,
                        css,
                        server: commands::preview::ServerOptions {
                            bind: *bind,
                            port: *port,
                            debounce_ms: *debounce_ms,
                        },
                    },
                    &PREVIEW_SHUTDOWN,
                )
                .map_err(preview_error)?;
                return Ok(ExitCode::SUCCESS);
            }
            let input_path = arguments.input.clone();
            let canonical_input = input_path
                .as_ref()
                .map(|path| path.canonicalize().unwrap_or_else(|_| path.clone()));
            let source_id = input_path.as_ref().map_or_else(
                || "<stdin>".to_owned(),
                |path| path.to_string_lossy().into_owned(),
            );
            let local_context = project_root.as_ref().map(|project_root| {
                let base = canonical_input
                    .as_deref()
                    .and_then(std::path::Path::parent)
                    .map_or_else(|| project_root.clone(), PathBuf::from);
                (base, project_root.clone(), source_id.clone())
            });
            let input = read_input(arguments.input)?;
            let mut prepared = None;
            let processed = if include {
                let source = decode_input(&input)?;
                let base_dir = match arguments.base_dir.clone() {
                    Some(base_dir) => base_dir,
                    None => input_path
                        .as_ref()
                        .and_then(|path| path.canonicalize().ok())
                        .and_then(|path| path.parent().map(PathBuf::from))
                        .ok_or_else(|| {
                            CliError::Usage(
                                "--include with standard input requires --base-dir".to_owned(),
                            )
                        })?,
                };
                let source_id = input_path.as_ref().map_or_else(
                    || "<stdin>".to_owned(),
                    |path| path.to_string_lossy().into_owned(),
                );
                let source_base = local_context
                    .as_ref()
                    .map(|(base, _, _)| base.as_path())
                    .unwrap_or(&base_dir);
                let include_input = prepare_includes(IncludePreparation {
                    source,
                    source_id,
                    base_dir: &base_dir,
                    source_base,
                    project_root: project_root.as_deref(),
                    allowed_roots: &allowed_roots,
                    limits: project_config.resources.limits,
                    preprocess: &project_config.preprocess,
                })
                .map_err(CliError::Include)?;
                let processed = if command_id == CommandId::Format {
                    input.clone()
                } else {
                    include_input
                        .projection()
                        .document()
                        .source
                        .as_bytes()
                        .to_vec()
                };
                prepared = Some(include_input);
                processed
            } else {
                input.clone()
            };
            let (output, exit_code) = if let CommandOptions::Check(check) = &arguments.command {
                let outcome = if let Some(prepared) = prepared.as_mut() {
                    check_preprocessed(prepared, check, &project_config.analysis)
                } else {
                    process_check(
                        &processed,
                        check,
                        &source_id,
                        &project_config.analysis,
                        &project_config.preprocess,
                        project_config.resources.limits,
                        local_context.as_ref().map(|(base, root, source_id)| {
                            (base.as_path(), root.as_path(), source_id.as_str())
                        }),
                    )
                }?;
                if check.summary {
                    eprintln!("adocweave check: {}", outcome.counts.summary());
                }
                let exit_code = outcome.exit_code();
                Ok((outcome.output, exit_code))
            } else if matches!(
                &arguments.command,
                CommandOptions::Format(FormatOptions { check: true, .. })
            ) {
                let CommandOptions::Format(options) = &arguments.command else {
                    unreachable!("format check matched above")
                };
                let outcome = commands::format::run_single(&input, *options, &project_config)
                    .map_err(format_error)?;
                Ok((outcome.output, ExitCode::SUCCESS))
            } else {
                let output = match &arguments.command {
                    CommandOptions::Convert { complete, css } => commands::convert::run(
                        &processed,
                        &project_config.analysis,
                        &project_config.html,
                        *complete,
                        css,
                        |path| fs::read(path),
                    )
                    .map_err(convert_error)?,
                    CommandOptions::Format(options) => {
                        commands::format::run_single(&processed, *options, &project_config)
                            .map_err(format_error)?
                            .output
                    }
                    CommandOptions::Symbols => commands::symbols::process(
                        &processed,
                        &project_config.analysis,
                    )
                    .map_err(|error| match error {
                        commands::symbols::Error::InvalidUtf8 { valid_up_to } => {
                            CliError::InvalidUtf8 { valid_up_to }
                        }
                        commands::symbols::Error::Analysis(source) => CliError::Analysis(source),
                    })?,
                    CommandOptions::ConfigShow => unreachable!("config show handled above"),
                    CommandOptions::Preview { .. } => unreachable!("preview handled above"),
                    CommandOptions::Check(_) => unreachable!("check handled above"),
                };
                Ok((output, ExitCode::SUCCESS))
            }?;
            let output = if matches!(
                &arguments.command,
                CommandOptions::Check(CheckOptions {
                    format: DiagnosticFormat::Human,
                    ..
                })
            ) {
                colorize_lines(&output, arguments.color)
            } else {
                output
            };
            let output = finish_output(output)?;
            io::stdout()
                .write_all(output.as_bytes())
                .map_err(CliError::Write)?;
            Ok(exit_code)
        }
    }
}

fn check_preprocessed(
    prepared: &mut local_include::PreparedInput,
    check: &CheckOptions,
    analysis_options: &AnalysisOptions,
) -> Result<CheckOutcome, CliError> {
    commands::check::process_preprocessed(prepared, check, analysis_options).map_err(check_error)
}

fn main() -> ExitCode {
    match run() {
        Ok(exit_code) => exit_code,
        Err(error) => {
            eprintln!("adocweave: {error}");
            eprintln!("Try 'adocweave --help' for more information.");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        Action, CommandOptions, CompletionShell, DEFAULT_PREVIEW_DEBOUNCE_MS, DEFAULT_PREVIEW_PORT,
        DiagnosticFormat, FormatOptions, parse_arguments, render_completion_script,
    };
    use crate::commands::model::{self, CommandId};

    fn arguments(values: &[&str]) -> impl Iterator<Item = String> {
        values.iter().map(ToString::to_string)
    }

    #[test]
    fn completion_renderers_use_the_model_command_tree() {
        fn assert_tree(shell: CompletionShell, tree: &model::CompletionTree) {
            let output = render_completion_script(shell, tree);
            let expected_contract = std::iter::once(format!(
                "# adocweave-command-tree root={}",
                tree.roots.join(",")
            ))
            .chain(tree.nested.iter().map(|group| {
                format!(
                    "# adocweave-command-tree parent={} children={}",
                    group.parent.join("/"),
                    group.children.join(",")
                )
            }))
            .collect::<Vec<_>>();
            assert_eq!(
                output
                    .lines()
                    .take(expected_contract.len())
                    .collect::<Vec<_>>(),
                expected_contract
            );
            for group in &tree.nested {
                for token in group.parent.iter().chain(&group.children) {
                    assert!(
                        output.matches(token).count() >= 2,
                        "{shell:?} did not render nested token {token}"
                    );
                }
            }
        }

        const ALTERNATE: &[model::CommandSpec] = &[
            model::CommandSpec {
                id: CommandId::ConfigShow,
                path: &["workspace", "inspect", "show"],
                root_usage: "",
                summary: "inspect workspace",
                help: None,
            },
            model::CommandSpec {
                id: CommandId::Help,
                path: &["project", "status"],
                root_usage: "",
                summary: "show project status",
                help: None,
            },
        ];
        let trees = [
            model::completion_tree(),
            model::completion_tree_for_tests(ALTERNATE),
        ];
        for tree in &trees {
            for shell in [
                CompletionShell::Bash,
                CompletionShell::Zsh,
                CompletionShell::Fish,
                CompletionShell::PowerShell,
            ] {
                assert_tree(shell, tree);
            }
        }

        let powershell = render_completion_script(CompletionShell::PowerShell, &trees[1]);
        let deep = "$words[1] -eq 'workspace' -and $words[2] -eq 'inspect' -and ($words.Count -eq 3 -or ($words.Count -eq 4 -and $wordToComplete -ne ''))";
        let shallow = "$words[1] -eq 'workspace' -and ($words.Count -eq 2 -or ($words.Count -eq 3 -and $wordToComplete -ne ''))";
        let deep_position = powershell.find(deep).expect("deep PowerShell branch");
        let shallow_position = powershell.find(shallow).expect("shallow PowerShell branch");
        assert!(
            deep_position < shallow_position,
            "PowerShell must test the deepest parent first"
        );
        assert!(
            powershell[deep_position..shallow_position].contains("@('show')"),
            "the deepest parent must offer its child"
        );

        let repository_powershell =
            render_completion_script(CompletionShell::PowerShell, &trees[0]);
        let config = "$words[1] -eq 'config' -and ($words.Count -eq 2 -or ($words.Count -eq 3 -and $wordToComplete -ne ''))";
        assert!(
            repository_powershell.contains(config),
            "config show must use the parent/partial-child guard"
        );

        let nested_position_matches =
            |parent_len: usize, words_count: usize, partial_child: bool| {
                words_count == parent_len + 1 || (words_count == parent_len + 2 && partial_child)
            };
        for parent_len in [1, 2] {
            assert!(nested_position_matches(parent_len, parent_len + 1, false));
            assert!(nested_position_matches(parent_len, parent_len + 2, true));
            assert!(!nested_position_matches(parent_len, parent_len + 2, false));
        }
    }

    #[test]
    fn parses_file_input() {
        let Action::Run(parsed) =
            parse_arguments(arguments(&["convert", "document.adoc"])).expect("valid arguments")
        else {
            panic!("expected run action");
        };

        assert_eq!(parsed.command.command_id(), CommandId::Convert);
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

        assert_eq!(parsed.command.command_id(), CommandId::Check);
        assert!(parsed.input.is_none());
    }

    #[test]
    fn all_commands_support_help() {
        for command in ["convert", "preview", "check", "format", "symbols"] {
            assert!(matches!(
                parse_arguments(arguments(&[command, "--help"])),
                Ok(Action::Help { .. })
            ));
        }
        assert!(matches!(
            parse_arguments(arguments(&["config", "show", "--help"])),
            Ok(Action::Help {
                command: Some(CommandId::ConfigShow)
            })
        ));
    }

    #[test]
    fn preview_help_explains_options_defaults_and_external_access() {
        let help = model::spec(CommandId::Preview)
            .help
            .expect("preview has command help");
        let root_help = model::root_help();
        let port = DEFAULT_PREVIEW_PORT.to_string();
        let debounce = DEFAULT_PREVIEW_DEBOUNCE_MS.to_string();
        for expected in [
            "--bind ADDRESS",
            "127.0.0.1",
            "--port PORT",
            "--debounce-ms MILLISECONDS",
            "--allow-external",
            "--include",
            "--base-dir DIR",
            "--allow-root DIR",
            "--css FILE",
            "--css-url URL",
            "--config FILE",
            "--no-config",
            "--color WHEN",
            "auto",
            "利用者認証",
            "TLS",
        ] {
            assert!(
                help.contains(expected),
                "preview helpに{expected}がありません"
            );
        }
        for (name, value) in [("port", port), ("debounce", debounce)] {
            assert!(
                help.contains(&value),
                "preview helpの{name}既定値が実装と異なります"
            );
            assert!(
                root_help.contains(&value),
                "全体helpの{name}既定値が実装と異なります"
            );
        }

        let Action::Run(parsed) =
            parse_arguments(arguments(&["preview", "document.adoc"])).expect("preview defaults")
        else {
            panic!("expected run action");
        };
        assert!(matches!(
            parsed.command,
            CommandOptions::Preview {
                port: DEFAULT_PREVIEW_PORT,
                debounce_ms: DEFAULT_PREVIEW_DEBOUNCE_MS,
                ..
            }
        ));
    }

    #[test]
    fn preview_requires_a_file_and_explicit_external_authority() {
        assert!(parse_arguments(arguments(&["preview"])).is_err());
        assert!(parse_arguments(arguments(&["preview", "-"])).is_err());
        assert!(
            parse_arguments(arguments(&[
                "preview",
                "--bind",
                "0.0.0.0",
                "document.adoc"
            ]))
            .is_err()
        );
        let Action::Run(parsed) = parse_arguments(arguments(&[
            "preview",
            "--bind",
            "0.0.0.0",
            "--allow-external",
            "--port",
            "8080",
            "--debounce-ms",
            "25",
            "document.adoc",
        ]))
        .expect("explicit external preview") else {
            panic!("expected run action");
        };
        assert!(matches!(
            parsed.command,
            CommandOptions::Preview {
                bind,
                port: 8080,
                debounce_ms: 25,
                ..
            } if bind == "0.0.0.0".parse::<std::net::IpAddr>().expect("address")
        ));
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
            assert!(matches!(
                parsed.command,
                CommandOptions::Check(options) if options.format == DiagnosticFormat::Json
            ));
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
        assert!(matches!(
            parsed.command,
            CommandOptions::Format(FormatOptions { check: true, .. })
        ));
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
