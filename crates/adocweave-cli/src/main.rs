use std::env;
use std::error::Error;
use std::fmt;
use std::fs;
use std::io::{self, Read, Write};
use std::net::{IpAddr, Ipv4Addr};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::atomic::AtomicBool;
use std::time::Duration;

use adocweave::output::diagnostics as diagnostic;
use adocweave::output::formatter::{FormatConfig, format_analysis};
use adocweave::output::html::{
    HtmlDocumentMode, RenderPolicy, StylesheetPolicy, StylesheetSource, render,
};
use adocweave::preprocess::{PreprocessedAnalysis, ProjectionLimits};
use adocweave::text::{PositionEncoding, SourceDocument};
use adocweave::{AnalysisOptions, Engine, OutputLimits, ParseError};

mod check_output;
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
    CheckOutcome, DiagnosticCounts, DiagnosticFormat, FailOn, github_annotation,
    prefix_human_source, sarif_log, sarif_result, sarif_results,
};
use file_workflow::{
    PendingWrite, atomic_write_all, colorize_lines, safe_write_format_config, unified_diff,
};

const HELP: &str = "\
AdocWeave command-line interface

Usage:
  adocweave <COMMAND> [FILE]

Commands:
  convert  Convert an AsciiDoc document
  preview  Serve a live, loopback-only document preview
  check    Check an AsciiDoc document
  format   Format an AsciiDoc document
  symbols  Print document symbols as JSON
  config show  Print the resolved project configuration as JSON
  completion SHELL  Print Bash, Zsh, Fish, or PowerShell completion
  help     Print this message

Arguments:
  [FILE]   Input file; omit it or use '-' to read standard input

Options:
  --format FORMAT  Emit check diagnostics as human, json, github, or sarif
  --json      Emit check diagnostics as JSON (deprecated alias)
  --fail-on LEVEL  Fail check on error, warning, or never (default: error)
  --summary   Emit check diagnostic counts to standard error
  --fix       Apply non-conflicting, always-safe check fixes
  --config FILE  Use an explicit project configuration
  --no-config    Disable project configuration discovery
  --list-rules  List available check rules; requires --json
  --enable-rule CODE  Enable an opt-in check rule; repeatable
  --check     Check formatting without writing formatted text
  --write     Atomically replace formatted input files
  --diff      Print unified formatting differences
  --dry-run   Report changes without writing them
  --glob PATTERN  Add files matching a glob pattern
  --color WHEN  Use auto, always, or never for terminal colors
  --include   Enable bounded local include processing
  --base-dir DIR    Resolve root document includes from DIR
  --allow-root DIR  Permit include resources below DIR; repeatable
  --local-targets     Check local file targets; check only
  --project-root DIR  Restrict local targets below DIR; requires --local-targets
  --complete  Convert to a complete HTML document instead of a fragment
  --css FILE      Embed CSS from FILE into the complete document; repeatable
  --css-url URL   Link an allowed stylesheet URL; repeatable
  --bind ADDRESS  Preview listen address (default: 127.0.0.1)
  --port PORT     Preview listen port (default: 4000)
  --debounce-ms MILLISECONDS  Preview rebuild debounce (default: 100)
  --allow-external  Permit an explicitly selected non-loopback address
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Operation {
    Convert,
    Preview,
    Check,
    Format,
    Symbols,
    ConfigShow,
}

/// A stylesheet argument in command-line order; files are embedded, URLs are
/// linked, and both apply only to complete document output.
#[derive(Clone, Debug, Eq, PartialEq)]
enum CssArgument {
    File(PathBuf),
    Url(String),
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
enum ColorChoice {
    #[default]
    Auto,
    Always,
    Never,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct CheckOptions {
    format: DiagnosticFormat,
    fail_on: FailOn,
    summary: bool,
    fix: bool,
    dry_run: bool,
    list_rules: bool,
    enabled_rules: Vec<diagnostic::LintRuleId>,
}

#[derive(Debug)]
enum CommandOptions {
    Convert {
        complete: bool,
        css: Vec<CssArgument>,
    },
    Preview {
        css: Vec<CssArgument>,
        bind: IpAddr,
        port: u16,
        debounce_ms: u64,
    },
    Check(CheckOptions),
    Format {
        check: bool,
        write: bool,
        diff: bool,
        dry_run: bool,
        summary: bool,
    },
    Symbols,
    ConfigShow,
}

impl CommandOptions {
    const fn operation(&self) -> Operation {
        match self {
            Self::Convert { .. } => Operation::Convert,
            Self::Preview { .. } => Operation::Preview,
            Self::Check(_) => Operation::Check,
            Self::Format { .. } => Operation::Format,
            Self::Symbols => Operation::Symbols,
            Self::ConfigShow => Operation::ConfigShow,
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
    Help { operation: Option<Operation> },
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

fn parse_arguments(mut arguments: impl Iterator<Item = String>) -> Result<Action, CliError> {
    let Some(command) = arguments.next() else {
        return Err(CliError::Usage("a command is required".to_owned()));
    };

    if matches!(command.as_str(), "-h" | "--help" | "help") {
        return Ok(Action::Help { operation: None });
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
    if command == "completion" {
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

    let operation = match command.as_str() {
        "convert" => Operation::Convert,
        "preview" => Operation::Preview,
        "check" => Operation::Check,
        "format" => Operation::Format,
        "symbols" => Operation::Symbols,
        "config" => match arguments.next().as_deref() {
            Some("show") => Operation::ConfigShow,
            Some(value) => return Err(CliError::Usage(format!("unknown config command: {value}"))),
            None => return Err(CliError::Usage("config requires a command".to_owned())),
        },
        _ => return Err(CliError::Usage(format!("unknown command: {command}"))),
    };

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
    let mut port = 4000;
    let mut debounce_ms = 100;
    let mut allow_external = false;
    let mut config_path = None;
    let mut no_config = false;
    let mut color = ColorChoice::Auto;
    while let Some(argument) = arguments.next() {
        match argument.as_str() {
            "-h" | "--help" => {
                return Ok(Action::Help {
                    operation: Some(operation),
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
            "--glob" if matches!(operation, Operation::Check | Operation::Format) => {
                let value = arguments
                    .next()
                    .ok_or_else(|| CliError::Usage("--glob requires a pattern".to_owned()))?;
                glob_patterns.push(value);
            }
            "--json" if operation == Operation::Check => {
                if format_selected && diagnostic_format != DiagnosticFormat::Json {
                    return Err(CliError::Usage(
                        "--json conflicts with another --format value".to_owned(),
                    ));
                }
                diagnostic_format = DiagnosticFormat::Json;
                format_selected = true;
            }
            "--format" if operation == Operation::Check => {
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
            "--fail-on" if operation == Operation::Check => {
                let value = arguments
                    .next()
                    .ok_or_else(|| CliError::Usage("--fail-on requires a level".to_owned()))?;
                fail_on = FailOn::parse(&value)?;
            }
            "--summary" if matches!(operation, Operation::Check | Operation::Format) => {
                summary = true
            }
            "--fix" if operation == Operation::Check => fix = true,
            "--dry-run" if matches!(operation, Operation::Check | Operation::Format) => {
                dry_run = true
            }
            "--list-rules" if operation == Operation::Check => list_rules = true,
            "--enable-rule" if operation == Operation::Check => {
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
            "--check" if operation == Operation::Format => format_check = true,
            "--write" if operation == Operation::Format => format_write = true,
            "--diff" if operation == Operation::Format => format_diff = true,
            "--include" => include = true,
            "--local-targets" if operation == Operation::Check => local_targets = true,
            "--project-root" if operation == Operation::Check => {
                let value = arguments.next().ok_or_else(|| {
                    CliError::Usage("--project-root requires a directory".to_owned())
                })?;
                project_root = Some(PathBuf::from(value));
            }
            "--complete" if operation == Operation::Convert => complete = true,
            "--css" if matches!(operation, Operation::Convert | Operation::Preview) => {
                let value = arguments
                    .next()
                    .ok_or_else(|| CliError::Usage("--css requires a file".to_owned()))?;
                css.push(CssArgument::File(PathBuf::from(value)));
            }
            "--css-url" if matches!(operation, Operation::Convert | Operation::Preview) => {
                let value = arguments
                    .next()
                    .ok_or_else(|| CliError::Usage("--css-url requires a URL".to_owned()))?;
                css.push(CssArgument::Url(value));
            }
            "--bind" if operation == Operation::Preview => {
                let value = arguments
                    .next()
                    .ok_or_else(|| CliError::Usage("--bind requires an address".to_owned()))?;
                bind = value
                    .parse()
                    .map_err(|_| CliError::Usage(format!("invalid bind address: {value}")))?;
            }
            "--port" if operation == Operation::Preview => {
                let value = arguments
                    .next()
                    .ok_or_else(|| CliError::Usage("--port requires a value".to_owned()))?;
                port = value
                    .parse()
                    .map_err(|_| CliError::Usage(format!("invalid port: {value}")))?;
            }
            "--debounce-ms" if operation == Operation::Preview => {
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
            "--allow-external" if operation == Operation::Preview => allow_external = true,
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
            _ if matches!(operation, Operation::Check | Operation::Format) && !stdin_selected => {
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
            operation,
            Operation::Format if format_write
        )
        && !(operation == Operation::Check && fix)
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
    if operation == Operation::Preview {
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
    if operation == Operation::ConfigShow
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

    let command = match operation {
        Operation::Convert => CommandOptions::Convert { complete, css },
        Operation::Preview => CommandOptions::Preview {
            css,
            bind,
            port,
            debounce_ms,
        },
        Operation::Check => CommandOptions::Check(CheckOptions {
            format: diagnostic_format,
            fail_on,
            summary,
            fix,
            dry_run,
            list_rules,
            enabled_rules,
        }),
        Operation::Format => CommandOptions::Format {
            check: format_check,
            write: format_write,
            diff: format_diff,
            dry_run,
            summary,
        },
        Operation::Symbols => CommandOptions::Symbols,
        Operation::ConfigShow => CommandOptions::ConfigShow,
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

fn analyze(source: &str, options: &AnalysisOptions) -> Result<adocweave::Analysis, CliError> {
    Engine::new(options.clone())
        .analyze(source)
        .map_err(CliError::Analysis)
}

fn check_analysis_options(
    base: &AnalysisOptions,
    enabled_rules: &[diagnostic::LintRuleId],
) -> AnalysisOptions {
    let mut options = base.clone();
    for rule in enabled_rules {
        let current = options.diagnostics.lint.rule(*rule);
        options.diagnostics.lint.set_rule(
            *rule,
            diagnostic::RuleSettings {
                enabled: true,
                ..current
            },
        );
    }
    options
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

/// Builds the convert render policy from command-line stylesheet arguments.
/// CSS files are read here so a missing or oversized file fails before any
/// output is produced; the renderer revalidates every source.
fn convert_policy(
    project: &adocweave_config::HtmlSettings,
    complete: bool,
    css: &[CssArgument],
    mut dependency_snapshots: Option<
        &mut std::collections::BTreeMap<PathBuf, preview::Fingerprint>,
    >,
) -> Result<RenderPolicy, CliError> {
    let limits = StylesheetPolicy::default();
    let mut sources = Vec::new();
    for path in &project.stylesheet_files {
        let bytes = if let Some(snapshots) = dependency_snapshots.as_deref_mut() {
            let (bytes, fingerprint) =
                preview::read_dependency(path).map_err(|source| CliError::Read {
                    source_name: path.display().to_string(),
                    source,
                })?;
            snapshots.insert(path.clone(), fingerprint);
            bytes
        } else {
            fs::read(path).map_err(|source| CliError::Read {
                source_name: path.display().to_string(),
                source,
            })?
        };
        if bytes.len()
            > usize::try_from(limits.max_inline_bytes).expect("u32 fits usize on supported targets")
        {
            return Err(CliError::Stylesheet(format!(
                "stylesheet {} exceeds the limit of {} bytes",
                path.display(),
                limits.max_inline_bytes
            )));
        }
        let text = String::from_utf8(bytes).map_err(|error| CliError::InvalidUtf8 {
            valid_up_to: error.utf8_error().valid_up_to(),
        })?;
        sources.push(StylesheetSource::Inline(text));
    }
    sources.extend(
        project
            .stylesheet_urls
            .iter()
            .cloned()
            .map(StylesheetSource::External),
    );
    for argument in css {
        match argument {
            CssArgument::File(path) => {
                let bytes = if let Some(snapshots) = dependency_snapshots.as_deref_mut() {
                    let (bytes, fingerprint) =
                        preview::read_dependency(path).map_err(|source| CliError::Read {
                            source_name: path.display().to_string(),
                            source,
                        })?;
                    snapshots.insert(path.clone(), fingerprint);
                    bytes
                } else {
                    fs::read(path).map_err(|source| CliError::Read {
                        source_name: path.display().to_string(),
                        source,
                    })?
                };
                if bytes.len()
                    > usize::try_from(limits.max_inline_bytes)
                        .expect("u32 fits usize on supported targets")
                {
                    return Err(CliError::Stylesheet(format!(
                        "stylesheet {} exceeds the limit of {} bytes",
                        path.display(),
                        limits.max_inline_bytes
                    )));
                }
                let text = String::from_utf8(bytes).map_err(|error| CliError::InvalidUtf8 {
                    valid_up_to: error.utf8_error().valid_up_to(),
                })?;
                sources.push(StylesheetSource::Inline(text));
            }
            CssArgument::Url(url) => sources.push(StylesheetSource::External(url.clone())),
        }
    }
    let document_mode = if complete {
        HtmlDocumentMode::Complete
    } else {
        project.policy.document_mode
    };
    if document_mode != HtmlDocumentMode::Complete && !sources.is_empty() {
        return Err(CliError::Usage(
            "--css and --css-url require --complete".to_owned(),
        ));
    }
    Ok(RenderPolicy {
        document_mode: if complete {
            HtmlDocumentMode::Complete
        } else {
            project.policy.document_mode
        },
        stylesheets: StylesheetPolicy { sources, ..limits },
        ..RenderPolicy::default()
    })
}

fn process_convert(
    input: &[u8],
    analysis_options: &AnalysisOptions,
    render_policy: &RenderPolicy,
) -> Result<String, CliError> {
    let source = decode_input(input)?;
    let analysis = analyze(source, analysis_options)?;
    let output = render(analysis.document(), render_policy);
    if let Some(diagnostic) = output.diagnostics.iter().find(|diagnostic| {
        matches!(
            diagnostic.code.as_str(),
            "invalid-stylesheet-url"
                | "invalid-stylesheet-content"
                | "stylesheet-limit-exceeded"
                | "stylesheet-not-applicable"
        )
    }) {
        return Err(CliError::Stylesheet(diagnostic.message.clone()));
    }
    Ok(output.html)
}

struct PreviewBuildRequest<'request> {
    input_path: &'request Path,
    include: bool,
    base_dir: &'request Path,
    project_root: &'request Path,
    project: &'request adocweave_config::ResolvedProjectConfig,
    css: &'request [CssArgument],
}

fn preview_build(
    request: PreviewBuildRequest<'_>,
    cancellation: &adocweave::CancellationToken,
    dependencies: &mut std::collections::BTreeMap<PathBuf, preview::Fingerprint>,
) -> Result<preview::Build, CliError> {
    let PreviewBuildRequest {
        input_path,
        include,
        base_dir,
        project_root,
        project,
        css,
    } = request;
    let (input, input_fingerprint) =
        preview::read_dependency(input_path).map_err(|source| CliError::Read {
            source_name: input_path.display().to_string(),
            source,
        })?;
    let source = decode_input(&input)?;
    let source_id = input_path.to_string_lossy().into_owned();
    dependencies.insert(input_path.to_owned(), input_fingerprint);

    let (processed, include_diagnostics) = if include {
        let mut observed_dependencies = std::collections::BTreeSet::new();
        let prepared = local_include::prepare_local_tracking(
            source,
            source_id,
            base_dir,
            base_dir,
            project_root,
            project.resources.limits,
            &project.preprocess,
            &mut observed_dependencies,
        );
        for path in observed_dependencies {
            dependencies
                .entry(path.clone())
                .or_insert_with(|| preview::Fingerprint::read(&path));
        }
        let prepared = prepared.map_err(CliError::Include)?;
        dependencies.extend(prepared.dependency_snapshots);
        for path in prepared.dependency_paths {
            dependencies
                .entry(path.clone())
                .or_insert_with(|| preview::Fingerprint::read(&path));
        }
        let include_diagnostics = prepared
            .include_errors
            .iter()
            .map(|(target, error)| {
                serde_json::json!({
                    "code": error.diagnostic_code(),
                    "message": error.to_string(),
                    "target": target,
                })
            })
            .collect::<Vec<_>>();
        (prepared.document.source.to_string(), include_diagnostics)
    } else {
        (source.to_owned(), Vec::new())
    };
    let analysis = Engine::new(project.analysis.clone())
        .analyze_cancellable(&processed, cancellation)
        .map_err(CliError::Analysis)?;
    let output = render(
        analysis.document(),
        &convert_policy(&project.html, true, css, Some(dependencies))?,
    );
    if let Some(item) = output.diagnostics.iter().find(|item| {
        matches!(
            item.code.as_str(),
            "invalid-stylesheet-url"
                | "invalid-stylesheet-content"
                | "stylesheet-limit-exceeded"
                | "stylesheet-not-applicable"
        )
    }) {
        return Err(CliError::Stylesheet(item.message.clone()));
    }
    let mut diagnostics = serde_json::from_str::<Vec<serde_json::Value>>(&diagnostic::render_json(
        analysis.diagnostics(),
    ))
    .expect("core diagnostic renderer returns a JSON array");
    diagnostics.extend(
        serde_json::from_str::<Vec<serde_json::Value>>(&diagnostic::render_json(
            &output.diagnostics,
        ))
        .expect("render diagnostic renderer returns a JSON array"),
    );
    diagnostics.extend(include_diagnostics);
    let style_origins = project
        .html
        .stylesheet_urls
        .iter()
        .chain(css.iter().filter_map(|argument| match argument {
            CssArgument::Url(url) => Some(url),
            CssArgument::File(_) => None,
        }))
        .filter_map(|value| url::Url::parse(value).ok())
        .filter(|url| matches!(url.scheme(), "http" | "https"))
        .map(|url| url.origin().ascii_serialization())
        .collect();
    Ok(preview::Build::new(
        output.html,
        serde_json::to_string(&diagnostics).expect("diagnostics are serializable"),
        dependencies.clone(),
    )
    .with_style_origins(style_origins))
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
    let source = decode_input(input)?;
    let analysis = Engine::new(check_analysis_options(
        analysis_options,
        &check.enabled_rules,
    ))
    .analyze(source)
    .map_err(CliError::Analysis)?;
    let mut host = if let Some((base, root, source_id)) = local {
        let mut targets = analysis.local_targets();
        let snapshot =
            std::iter::empty::<(String, adocweave::preprocess::ResourceDocument)>().collect();
        let mut local_preprocess_options = preprocess_options.clone();
        local_preprocess_options.source_id = Some(adocweave::SourceId::new(source_id));
        local_preprocess_options.enable_includes = false;
        let include_document =
            adocweave::preprocess::preprocess(source, &snapshot, &local_preprocess_options)
                .map_err(|error| {
                    CliError::Include(local_include::LocalIncludeError::Preprocess(error))
                })?;
        let includes = include_document
            .directives
            .iter()
            .filter(|directive| directive.kind == adocweave::preprocess::DirectiveKind::Include)
            .collect::<Vec<_>>();
        let optional_ranges = includes
            .iter()
            .filter(|include| include.optional)
            .map(|include| include.target_range)
            .collect::<Vec<_>>();
        targets.extend(includes.iter().filter_map(|include| include.local_target()));
        let mut diagnostics =
            local_target::validate(&targets, base, root, source_id, source, resource_limits)
                .map_err(CliError::LocalTarget)?;
        diagnostics.retain(|diagnostic| {
            diagnostic.code != "local-target-missing"
                || !optional_ranges.contains(&diagnostic.range)
        });
        diagnostics
    } else {
        Vec::new()
    };
    host.sort_by(|left, right| {
        (
            left.range.start(),
            left.range.end(),
            left.code,
            left.target.as_str(),
        )
            .cmp(&(
                right.range.start(),
                right.range.end(),
                right.code,
                right.target.as_str(),
            ))
    });
    let mut counts = DiagnosticCounts::default();
    for item in analysis.diagnostics() {
        counts.add(item.severity);
    }
    counts.add_host_errors(host.len());
    let output = match check.format {
        DiagnosticFormat::Json => {
            let core = diagnostic::render_json(analysis.diagnostics());
            if host.is_empty() {
                core
            } else {
                let mut values = serde_json::from_str::<Vec<serde_json::Value>>(&core)
                    .expect("core diagnostic renderer returns a JSON array");
                values.extend(local_target::json_values(&host));
                serde_json::to_string(&values).expect("diagnostics are serializable")
            }
        }
        DiagnosticFormat::Human => {
            let core = diagnostic::render_human(
                analysis.diagnostics(),
                analysis.source_document(),
                PositionEncoding::Utf8,
            )
            .map_err(CliError::Position)?;
            prefix_human_source(&core, source_id)
                + &local_target::render_human(&host, source).map_err(CliError::Position)?
        }
        DiagnosticFormat::Github => {
            let document = SourceDocument::new(source).map_err(CliError::Position)?;
            let mut output = String::new();
            for item in analysis.diagnostics() {
                let position = document
                    .offset_to_position(item.range.start(), PositionEncoding::Utf8)
                    .map_err(CliError::Position)?;
                output.push_str(&github_annotation(
                    item.severity,
                    item.code.as_str(),
                    &item.message,
                    source_id,
                    position.line + 1,
                    position.character + 1,
                ));
            }
            for item in &host {
                output.push_str(&github_annotation(
                    diagnostic::Severity::Error,
                    item.code,
                    item.message,
                    &item.source_id,
                    item.line,
                    item.column,
                ));
            }
            output
        }
        DiagnosticFormat::Sarif => {
            let document = SourceDocument::new(source).map_err(CliError::Position)?;
            let mut results = Vec::new();
            for item in analysis.diagnostics() {
                let position = document
                    .offset_to_position(item.range.start(), PositionEncoding::Utf8)
                    .map_err(CliError::Position)?;
                results.push(sarif_result(
                    item.id.as_str(),
                    item.severity,
                    item.code.as_str(),
                    &item.message,
                    source_id,
                    position.line + 1,
                    position.character + 1,
                ));
            }
            results.extend(host.iter().map(|item| {
                let id = format!(
                    "{}@{}:{}:{}",
                    item.code,
                    item.source_id,
                    item.range.start().to_u32(),
                    item.range.end().to_u32()
                );
                sarif_result(
                    &id,
                    diagnostic::Severity::Error,
                    item.code,
                    item.message,
                    &item.source_id,
                    item.line,
                    item.column,
                )
            }));
            sarif_log(results)
        }
    };
    Ok(CheckOutcome {
        output,
        counts,
        fail_on: check.fail_on,
    })
}

fn process_format(
    input: &[u8],
    analysis_options: &AnalysisOptions,
    format_config: &FormatConfig,
) -> Result<String, CliError> {
    let source = decode_input(input)?;
    let analysis = analyze(source, analysis_options)?;
    Ok(format_analysis(&analysis, format_config)
        .map_err(CliError::Position)?
        .formatted)
}

fn process_symbols(input: &[u8], analysis_options: &AnalysisOptions) -> Result<String, CliError> {
    let source = decode_input(input)?;
    let analysis = analyze(source, analysis_options)?;
    Ok(adocweave::semantic::render_symbols_json(
        &adocweave::semantic::document_symbols(analysis.document()),
    ))
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

fn resolved_config_json(
    snapshot: Option<&adocweave_config::ConfigSnapshot>,
    config: &adocweave_config::ResolvedProjectConfig,
) -> serde_json::Value {
    let attributes = config
        .analysis
        .attributes
        .iter()
        .map(|(name, value)| {
            (
                name.clone(),
                serde_json::json!({ "state": if value.is_some() { "set" } else { "unset" } }),
            )
        })
        .collect::<serde_json::Map<_, _>>();
    let rules = diagnostic::LINT_RULES
        .iter()
        .map(|descriptor| {
            let settings = config.analysis.diagnostics.lint.rule(descriptor.id);
            (
                descriptor.id.as_str().to_owned(),
                serde_json::json!({
                    "enabled": settings.enabled,
                    "severity": settings.severity.as_str(),
                }),
            )
        })
        .collect::<serde_json::Map<_, _>>();
    let path = |path: &std::path::Path| path.to_string_lossy().into_owned();
    serde_json::json!({
        "schemaVersion": config.schema_version,
        "source": snapshot.map(|snapshot| path(&snapshot.path)),
        "analysis": {
            "syntaxMode": match config.analysis.syntax.syntax_mode {
                adocweave::SyntaxMode::Permissive => "permissive",
                adocweave::SyntaxMode::Strict => "strict",
            },
            "attributes": attributes,
        },
        "lint": {
            "rules": rules,
            "maxLineLength": config.analysis.diagnostics.lint.max_line_length,
            "maxConsecutiveBlankLines":
                config.analysis.diagnostics.lint.max_consecutive_blank_lines,
            "maxDiagnostics": config.analysis.diagnostics.lint.max_diagnostics,
        },
        "resources": {
            "include": config.resources.include,
            "roots": config.resources.roots.iter().map(|value| path(value)).collect::<Vec<_>>(),
            "maxFiles": config.resources.limits.max_files,
            "maxTotalBytes": config.resources.limits.max_total_bytes,
            "maxResourceBytes": config.resources.limits.max_resource_bytes,
        },
        "localTargets": {
            "enabled": config.local_targets.enabled,
            "projectRoot": config.local_targets.project_root.as_deref().map(path),
        },
        "format": {
            "newline": match config.format.newline {
                adocweave::output::formatter::NewlineStyle::Lf => "lf",
                adocweave::output::formatter::NewlineStyle::CrLf => "cr-lf",
            },
            "finalNewline": config.format.final_newline,
            "maxConsecutiveBlankLines": config.format.max_consecutive_blank_lines,
        },
        "html": {
            "complete": config.html.policy.document_mode == HtmlDocumentMode::Complete,
            "stylesheetFiles":
                config.html.stylesheet_files.iter().map(|value| path(value)).collect::<Vec<_>>(),
            "stylesheetUrls": config.html.stylesheet_urls,
        }
    })
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

fn apply_safe_fixes(input: &[u8], analysis_options: &AnalysisOptions) -> Result<Vec<u8>, CliError> {
    let source = decode_input(input)?;
    let analysis = Engine::new(analysis_options.clone())
        .analyze(source)
        .map_err(CliError::Analysis)?;
    let edits = analysis
        .diagnostics()
        .iter()
        .flat_map(|diagnostic| &diagnostic.fixes)
        .filter(|fix| fix.applicability == diagnostic::Applicability::Always)
        .flat_map(|fix| fix.edits().iter().cloned())
        .collect::<Vec<_>>();
    if edits.is_empty() {
        return Ok(input.to_vec());
    }
    let fix = diagnostic::Fix::new("apply safe fixes", diagnostic::Applicability::Always, edits)
        .map_err(CliError::FixConflict)?;
    let mut fixed = source.to_owned();
    for edit in fix.edits().iter().rev() {
        fixed.replace_range(
            edit.range.start().to_usize()..edit.range.end().to_usize(),
            &edit.replacement,
        );
    }
    Ok(fixed.into_bytes())
}

fn run_multi_path(arguments: &Arguments) -> Result<Option<ExitCode>, CliError> {
    let paths = collect_input_paths(arguments)?;
    let directory_selected = arguments.input.as_ref().is_some_and(|path| path.is_dir());
    let explicit_path_mode = matches!(
        arguments.command,
        CommandOptions::Format { write: true, .. } | CommandOptions::Format { diff: true, .. }
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
        CommandOptions::Format {
            check,
            write,
            diff,
            dry_run,
            summary,
        } => {
            if !check && !write && !diff {
                return Err(CliError::Usage(
                    "multiple format inputs require --check, --write, or --diff".to_owned(),
                ));
            }
            let mut pending = Vec::new();
            let mut differences = 0_usize;
            let mut output = String::new();
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
                let format_config = if *write || *check || *diff {
                    safe_write_format_config(&original, &config)
                } else {
                    config.format
                };
                let formatted =
                    process_format(&original, &config.analysis, &format_config)?.into_bytes();
                if original == formatted {
                    continue;
                }
                differences += 1;
                if *diff {
                    output.push_str(&unified_diff(
                        path,
                        decode_input(&original)?,
                        decode_input(&formatted)?,
                    ));
                }
                if *write && !dry_run {
                    pending.push(PendingWrite {
                        path: path.clone(),
                        original,
                        replacement: formatted,
                    });
                }
            }
            if !pending.is_empty() {
                atomic_write_all(pending)?;
            }
            if !output.is_empty() {
                let output = finish_output(colorize_lines(&output, arguments.color))?;
                print!("{output}");
            }
            if *summary {
                eprintln!(
                    "adocweave format: files={}, changed={differences}",
                    paths.len()
                );
            }
            Ok(Some(if *check && differences > 0 {
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
                    apply_safe_fixes(
                        &original,
                        &check_analysis_options(&config.analysis, &check.enabled_rules),
                    )?
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
                    let mut prepared = if let Some(root) = &project_root {
                        local_include::prepare_local(
                            source,
                            source_id.to_string(),
                            base_dir,
                            &source_base,
                            root,
                            config.resources.limits,
                            &config.preprocess,
                        )
                    } else {
                        local_include::prepare(
                            source,
                            Some(source_id.to_string()),
                            base_dir,
                            allowed_roots,
                            config.resources.limits,
                            &config.preprocess,
                        )
                    }
                    .map_err(CliError::Include)?;
                    check_preprocessed(&mut prepared, check, &config.analysis)
                        .map_err(CliError::Include)?
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

fn completion_script(shell: CompletionShell) -> &'static str {
    match shell {
        CompletionShell::Bash => {
            r#"_adocweave() {
  local commands="convert preview check format symbols config completion help"
  local options="--format --fail-on --summary --fix --check --write --diff --dry-run --config --no-config --include --base-dir --allow-root --local-targets --project-root --complete --css --css-url --bind --port --debounce-ms --allow-external --help --version"
  if [[ ${COMP_CWORD} -eq 1 ]]; then
    COMPREPLY=( $(compgen -W "${commands}" -- "${COMP_WORDS[COMP_CWORD]}") )
  else
    COMPREPLY=( $(compgen -W "${options}" -f -- "${COMP_WORDS[COMP_CWORD]}") )
  fi
}
complete -F _adocweave adocweave
"#
        }
        CompletionShell::Zsh => {
            r#"#compdef adocweave
_adocweave() {
  _arguments '*:argument:->args'
  case $state in
    args) _values 'arguments' convert preview check format symbols config completion help \
      --format --fail-on --summary --fix --check --write --diff --dry-run \
      --config --no-config --include --base-dir --allow-root --local-targets \
      --project-root --complete --css --css-url --bind --port --debounce-ms --allow-external ;;
  esac
}
compdef _adocweave adocweave
"#
        }
        CompletionShell::Fish => {
            r#"complete -c adocweave -f -n '__fish_use_subcommand' -a 'convert preview check format symbols config completion help'
complete -c adocweave -l format -x -a 'human json github sarif'
complete -c adocweave -l fail-on -x -a 'error warning never'
complete -c adocweave -l config -r
complete -c adocweave -l write
complete -c adocweave -l diff
complete -c adocweave -l fix
"#
        }
        CompletionShell::PowerShell => {
            r#"Register-ArgumentCompleter -Native -CommandName adocweave -ScriptBlock {
  param($wordToComplete, $commandAst, $cursorPosition)
  'convert','preview','check','format','symbols','config','completion','help',
  '--format','--fail-on','--summary','--fix','--check','--write','--diff',
  '--dry-run','--config','--no-config','--include','--base-dir','--allow-root',
  '--bind','--port','--debounce-ms','--allow-external' |
    Where-Object { $_ -like "$wordToComplete*" } |
    ForEach-Object { [System.Management.Automation.CompletionResult]::new($_, $_, 'ParameterValue', $_) }
}
"#
        }
    }
}

fn command_help(operation: Operation) -> &'static str {
    match operation {
        Operation::Convert => {
            "Usage:\n  adocweave convert [OPTIONS] [FILE]\n\nExample:\n  adocweave convert --complete manual.adoc\n"
        }
        Operation::Preview => {
            "Usage:\n  adocweave preview [OPTIONS] FILE\n\nExample:\n  adocweave preview --include manual.adoc\n"
        }
        Operation::Check => {
            "Usage:\n  adocweave check [OPTIONS] [FILE...]\n\nExamples:\n  adocweave check --fail-on warning docs\n  adocweave check --format github --summary manual.adoc\n  adocweave check --format sarif docs > adocweave.sarif\n  adocweave check --fix docs\n"
        }
        Operation::Format => {
            "Usage:\n  adocweave format [OPTIONS] [FILE...]\n\nExamples:\n  adocweave format --check docs\n  adocweave format --diff manual.adoc\n  adocweave format --write docs\n"
        }
        Operation::Symbols => {
            "Usage:\n  adocweave symbols [FILE]\n\nExample:\n  adocweave symbols manual.adoc\n"
        }
        Operation::ConfigShow => {
            "Usage:\n  adocweave config show [--config FILE | --no-config]\n\nExample:\n  adocweave config show\n"
        }
    }
}

fn run() -> Result<ExitCode, CliError> {
    match parse_arguments(env::args().skip(1))? {
        Action::Help { operation } => {
            print!("{}", operation.map_or(HELP, command_help));
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
            let project_config = config_snapshot.as_ref().map_or_else(
                adocweave_config::ResolvedProjectConfig::default,
                |snapshot| snapshot.config.clone(),
            );
            if matches!(arguments.command, CommandOptions::ConfigShow) {
                let output = serde_json::to_string_pretty(&resolved_config_json(
                    config_snapshot.as_ref(),
                    &project_config,
                ))
                .expect("resolved configuration is serializable");
                println!("{output}");
                return Ok(ExitCode::SUCCESS);
            }
            let operation = arguments.command.operation();
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
                matches!(operation, Operation::Convert | Operation::Preview),
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
                let metadata =
                    fs::symlink_metadata(input_path).map_err(|source| CliError::Read {
                        source_name: input_path.display().to_string(),
                        source,
                    })?;
                if metadata.file_type().is_symlink() || !metadata.is_file() {
                    return Err(CliError::Path(format!(
                        "preview input must be a regular, non-symlink file: {}",
                        input_path.display()
                    )));
                }
                let canonical_input =
                    input_path.canonicalize().map_err(|source| CliError::Read {
                        source_name: input_path.display().to_string(),
                        source,
                    })?;
                let base_dir = arguments
                    .base_dir
                    .clone()
                    .or_else(|| canonical_input.parent().map(PathBuf::from))
                    .expect("a file has a parent");
                let configured_root = include.then(|| {
                    allowed_roots.iter().find_map(|root| {
                        root.canonicalize()
                            .ok()
                            .filter(|root| canonical_input.starts_with(root))
                    })
                });
                let preview_root = project_root
                    .clone()
                    .or(configured_root.flatten())
                    .unwrap_or_else(|| base_dir.clone())
                    .canonicalize()
                    .map_err(|source| CliError::Read {
                        source_name: "preview project root".to_owned(),
                        source,
                    })?;
                if !canonical_input.starts_with(&preview_root) {
                    return Err(CliError::Path(format!(
                        "preview input is outside the project root: {}",
                        canonical_input.display()
                    )));
                }
                if !bind.is_loopback() {
                    eprintln!(
                        "warning: preview is exposed on non-loopback address {bind}; rendered content may be visible to other hosts"
                    );
                }
                PREVIEW_SHUTDOWN.store(false, std::sync::atomic::Ordering::Release);
                install_preview_signal_handlers();
                preview::run(
                    preview::Options {
                        bind: *bind,
                        port: *port,
                        debounce: Duration::from_millis(*debounce_ms),
                    },
                    |cancellation| {
                        let mut dependencies = std::collections::BTreeMap::new();
                        let result = preview_build(
                            PreviewBuildRequest {
                                input_path: &canonical_input,
                                include,
                                base_dir: &base_dir,
                                project_root: &preview_root,
                                project: &project_config,
                                css,
                            },
                            cancellation,
                            &mut dependencies,
                        );
                        match result {
                            Ok(build) => Ok(build),
                            Err(error) => {
                                let paths = std::iter::once(canonical_input.clone())
                                    .chain(project_config.html.stylesheet_files.iter().cloned())
                                    .chain(css.iter().filter_map(|argument| match argument {
                                        CssArgument::File(path) => Some(path.clone()),
                                        CssArgument::Url(_) => None,
                                    }));
                                dependencies.extend(paths.map(|path| {
                                    let fingerprint = preview::Fingerprint::read(&path);
                                    (path, fingerprint)
                                }));
                                Ok(preview::Build::failure(error.to_string(), dependencies))
                            }
                        }
                    },
                    &PREVIEW_SHUTDOWN,
                )
                .map_err(CliError::Preview)?;
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
                let include_input = if let Some(project_root) = &project_root {
                    let source_base = local_context
                        .as_ref()
                        .map(|(base, _, _)| base.as_path())
                        .unwrap_or(&base_dir);
                    local_include::prepare_local(
                        source,
                        source_id,
                        &base_dir,
                        source_base,
                        project_root,
                        project_config.resources.limits,
                        &project_config.preprocess,
                    )
                } else {
                    local_include::prepare(
                        source,
                        Some(source_id),
                        &base_dir,
                        &allowed_roots,
                        project_config.resources.limits,
                        &project_config.preprocess,
                    )
                }
                .map_err(CliError::Include)?;
                let processed = if operation == Operation::Format {
                    input.clone()
                } else {
                    include_input.document.source.as_bytes().to_vec()
                };
                prepared = Some(include_input);
                processed
            } else {
                input.clone()
            };
            let (output, exit_code) = if let CommandOptions::Check(check) = &arguments.command {
                let outcome = if let Some(prepared) = prepared.as_mut() {
                    check_preprocessed(prepared, check, &project_config.analysis)
                        .map_err(CliError::Include)
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
                CommandOptions::Format { check: true, .. }
            ) {
                let source = decode_input(&input)?;
                let format_config = safe_write_format_config(&input, &project_config);
                let output = process_format(&input, &project_config.analysis, &format_config)?;
                if output != source {
                    return Err(CliError::FormattingRequired);
                }
                Ok((String::new(), ExitCode::SUCCESS))
            } else {
                let output = match &arguments.command {
                    CommandOptions::Convert { complete, css } => process_convert(
                        &processed,
                        &project_config.analysis,
                        &convert_policy(&project_config.html, *complete, css, None)?,
                    )?,
                    CommandOptions::Format { .. } => process_format(
                        &processed,
                        &project_config.analysis,
                        &project_config.format,
                    )?,
                    CommandOptions::Symbols => {
                        process_symbols(&processed, &project_config.analysis)?
                    }
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
) -> Result<CheckOutcome, local_include::LocalIncludeError> {
    let engine = adocweave::Engine::new(check_analysis_options(
        analysis_options,
        &check.enabled_rules,
    ));
    let analysis = engine
        .analyze(&prepared.document.source)
        .map_err(|error| local_include::LocalIncludeError::Analysis(error.to_string()))?;
    let projected = PreprocessedAnalysis {
        document: prepared.document.clone(),
        analysis,
    }
    .project_origins(ProjectionLimits::default())
    .map_err(|error| local_include::LocalIncludeError::Analysis(error.to_string()))?;
    let mut host = Vec::new();
    if let Some(session) = prepared.local_session.as_mut() {
        for target in &projected.local_targets {
            for origin in &target.target_origins {
                let source_id = origin
                    .source_id
                    .as_ref()
                    .map_or("<stdin>", adocweave::SourceId::as_str);
                let directive = (target.value.kind == adocweave::LocalTargetKind::Include)
                    .then(|| {
                        projected.directives.iter().find(|directive| {
                            directive
                                .source_id
                                .as_ref()
                                .map(adocweave::SourceId::as_str)
                                == Some(source_id)
                                && directive.target_range == origin.range.text_range()
                        })
                    })
                    .flatten();
                let base = if directive.is_some() {
                    prepared.include_bases.get(source_id)
                } else {
                    prepared.source_bases.get(source_id)
                }
                .ok_or_else(|| {
                    local_include::LocalIncludeError::MissingSource(source_id.to_owned())
                })?;
                let optional = directive.is_some_and(|directive| directive.optional);
                let source = prepared.sources.get(source_id).ok_or_else(|| {
                    local_include::LocalIncludeError::MissingSource(source_id.to_owned())
                })?;
                if let Some(error) =
                    directive.and_then(|directive| prepared.include_errors.get(&directive.target))
                {
                    if optional && matches!(error, adocweave_host::LocalTargetError::Missing(_)) {
                        continue;
                    }
                    host.push(local_target::diagnostic_from_error(
                        error,
                        source_id,
                        source,
                        origin.range.text_range(),
                        &target.value.target,
                    ));
                    continue;
                }
                if optional && target.value.syntax == adocweave::LocalTargetSyntax::Candidate {
                    match session.inspect(base, &target.value.path) {
                        Ok(_) | Err(adocweave_host::LocalTargetError::Missing(_)) => continue,
                        Err(_) => {}
                    }
                }
                let mut value = target.value.clone();
                value.target_range = origin.range.text_range();
                host.extend(local_target::validate_with_session(
                    std::slice::from_ref(&value),
                    base,
                    source_id,
                    source,
                    session,
                ));
            }
        }
        host.sort_by(|left, right| {
            (
                left.source_id.as_str(),
                left.range.start(),
                left.range.end(),
                left.code,
                left.target.as_str(),
            )
                .cmp(&(
                    right.source_id.as_str(),
                    right.range.start(),
                    right.range.end(),
                    right.code,
                    right.target.as_str(),
                ))
        });
    }
    let mut counts = DiagnosticCounts::default();
    for item in &projected.diagnostics {
        for _ in &item.origins {
            counts.add(item.diagnostic.severity);
        }
    }
    counts.add_host_errors(host.len());
    if check.format == DiagnosticFormat::Json {
        let mut values = projected
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
        values.extend(local_target::json_values(&host));
        return serde_json::to_string(&values)
            .map(|output| CheckOutcome {
                output,
                counts,
                fail_on: check.fail_on,
            })
            .map_err(|error| local_include::LocalIncludeError::Analysis(error.to_string()));
    }
    if check.format == DiagnosticFormat::Sarif {
        let mut results = Vec::new();
        for diagnostic in &projected.diagnostics {
            for origin in &diagnostic.origins {
                let source_id = origin
                    .source_id
                    .as_ref()
                    .map_or("<unknown>", adocweave::SourceId::as_str);
                let source = prepared.sources.get(source_id).ok_or_else(|| {
                    local_include::LocalIncludeError::MissingSource(source_id.to_owned())
                })?;
                let index = SourceDocument::new(source)
                    .map_err(local_include::LocalIncludeError::Position)?;
                let position = index
                    .offset_to_position(origin.range.start(), PositionEncoding::Utf8)
                    .map_err(local_include::LocalIncludeError::Position)?;
                results.push(sarif_result(
                    &format!(
                        "{}@{}:{}:{}",
                        diagnostic.diagnostic.code.as_str(),
                        source_id,
                        origin.range.start().to_u32(),
                        origin.range.end().to_u32()
                    ),
                    diagnostic.diagnostic.severity,
                    diagnostic.diagnostic.code.as_str(),
                    &diagnostic.diagnostic.message,
                    source_id,
                    position.line + 1,
                    position.character + 1,
                ));
            }
        }
        results.extend(host.iter().map(|diagnostic| {
            let id = format!(
                "{}@{}:{}:{}",
                diagnostic.code,
                diagnostic.source_id,
                diagnostic.range.start().to_u32(),
                diagnostic.range.end().to_u32()
            );
            sarif_result(
                &id,
                diagnostic::Severity::Error,
                diagnostic.code,
                diagnostic.message,
                &diagnostic.source_id,
                diagnostic.line,
                diagnostic.column,
            )
        }));
        return Ok(CheckOutcome {
            output: sarif_log(results),
            counts,
            fail_on: check.fail_on,
        });
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
            if check.format == DiagnosticFormat::Github {
                output.push_str(&github_annotation(
                    diagnostic.diagnostic.severity,
                    diagnostic.diagnostic.code.as_str(),
                    &diagnostic.diagnostic.message,
                    source_id,
                    position.line + 1,
                    position.character + 1,
                ));
                continue;
            }
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
    for diagnostic in &host {
        if check.format == DiagnosticFormat::Github {
            output.push_str(&github_annotation(
                diagnostic::Severity::Error,
                diagnostic.code,
                diagnostic.message,
                &diagnostic.source_id,
                diagnostic.line,
                diagnostic.column,
            ));
            continue;
        }
        let source = prepared.sources.get(&diagnostic.source_id).ok_or_else(|| {
            local_include::LocalIncludeError::MissingSource(diagnostic.source_id.clone())
        })?;
        output.push_str(
            &local_target::render_human(std::slice::from_ref(diagnostic), source)
                .map_err(local_include::LocalIncludeError::Position)?,
        );
    }
    Ok(CheckOutcome {
        output,
        counts,
        fail_on: check.fail_on,
    })
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
        Action, CommandOptions, CssArgument, DiagnosticFormat, Operation, PreviewBuildRequest,
        parse_arguments, preview_build,
    };

    fn arguments(values: &[&str]) -> impl Iterator<Item = String> {
        values.iter().map(ToString::to_string)
    }

    #[test]
    fn failed_preview_build_retains_discovered_include_dependencies() {
        let root = tempfile::tempdir().expect("project root");
        let input = root.path().join("manual.adoc");
        let include = root.path().join("part.adoc");
        let stylesheet = root.path().join("invalid.css");
        std::fs::write(&input, "include::part.adoc[]\n").expect("root document");
        std::fs::write(&include, "included text\n").expect("include");
        std::fs::write(&stylesheet, "</style").expect("invalid stylesheet");
        let mut dependencies = std::collections::BTreeMap::new();

        let result = preview_build(
            PreviewBuildRequest {
                input_path: &input,
                include: true,
                base_dir: root.path(),
                project_root: root.path(),
                project: &adocweave_config::ResolvedProjectConfig::default(),
                css: &[CssArgument::File(stylesheet)],
            },
            &adocweave::CancellationToken::new(),
            &mut dependencies,
        );

        assert!(result.is_err());
        assert!(dependencies.contains_key(&include));
    }

    #[test]
    fn preprocess_failure_retains_dependencies_discovered_before_the_error() {
        let root = tempfile::tempdir().expect("project root");
        let input = root.path().join("manual.adoc");
        let include = root.path().join("part.adoc");
        std::fs::write(&input, "include::part.adoc[]\n").expect("root document");
        std::fs::write(&include, "include::part.adoc[]\n").expect("cyclic include");
        let mut dependencies = std::collections::BTreeMap::new();

        let result = preview_build(
            PreviewBuildRequest {
                input_path: &input,
                include: true,
                base_dir: root.path(),
                project_root: root.path(),
                project: &adocweave_config::ResolvedProjectConfig::default(),
                css: &[],
            },
            &adocweave::CancellationToken::new(),
            &mut dependencies,
        );

        assert!(result.is_err());
        assert!(dependencies.contains_key(&include));
    }

    #[test]
    fn parses_file_input() {
        let Action::Run(parsed) =
            parse_arguments(arguments(&["convert", "document.adoc"])).expect("valid arguments")
        else {
            panic!("expected run action");
        };

        assert_eq!(parsed.command.operation(), Operation::Convert);
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

        assert_eq!(parsed.command.operation(), Operation::Check);
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
            CommandOptions::Format { check: true, .. }
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
