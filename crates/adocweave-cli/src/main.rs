use std::env;
use std::error::Error;
use std::fmt;
use std::fs;
use std::io::{self, Read, Write};
use std::path::PathBuf;
use std::process::ExitCode;

use adocweave::output::diagnostics as diagnostic;
use adocweave::output::formatter::{FormatConfig, format_analysis};
use adocweave::output::html::{
    HtmlDocumentMode, RenderPolicy, StylesheetPolicy, StylesheetSource, render,
};
use adocweave::preprocess::{PreprocessedAnalysis, ProjectionLimits};
use adocweave::text::{PositionEncoding, SourceDocument};
use adocweave::{AnalysisOptions, Engine, OutputLimits, ParseError};

mod local_include;
mod local_target;

const HELP: &str = "\
AdocWeave command-line interface

Usage:
  adocweave <COMMAND> [FILE]

Commands:
  convert  Convert an AsciiDoc document
  check    Check an AsciiDoc document
  format   Format an AsciiDoc document
  symbols  Print document symbols as JSON
  config show  Print the resolved project configuration as JSON
  help     Print this message

Arguments:
  [FILE]   Input file; omit it or use '-' to read standard input

Options:
  --format FORMAT  Emit check diagnostics as human, json, or github
  --json      Emit check diagnostics as JSON (deprecated alias)
  --fail-on LEVEL  Fail check on error, warning, or never (default: error)
  --summary   Emit check diagnostic counts to standard error
  --config FILE  Use an explicit project configuration
  --no-config    Disable project configuration discovery
  --list-rules  List available check rules; requires --json
  --enable-rule CODE  Enable an opt-in check rule; repeatable
  --check     Check formatting without writing formatted text
  --include   Enable bounded local include processing
  --base-dir DIR    Resolve root document includes from DIR
  --allow-root DIR  Permit include resources below DIR; repeatable
  --local-targets     Check local file targets; check only
  --project-root DIR  Restrict local targets below DIR; requires --local-targets
  --complete  Convert to a complete HTML document instead of a fragment
  --css FILE      Embed CSS from FILE into the complete document; repeatable
  --css-url URL   Link an allowed stylesheet URL; repeatable
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
            Self::Usage(_)
            | Self::InvalidUtf8 { .. }
            | Self::OutputLimit { .. }
            | Self::FormattingRequired
            | Self::Stylesheet(_)
            | Self::ConfigAuthority(_) => None,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Operation {
    Convert,
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum DiagnosticFormat {
    Human,
    Json,
    Github,
}

impl DiagnosticFormat {
    fn parse(value: &str) -> Result<Self, CliError> {
        match value {
            "human" => Ok(Self::Human),
            "json" => Ok(Self::Json),
            "github" => Ok(Self::Github),
            _ => Err(CliError::Usage(format!(
                "unknown diagnostic format: {value}"
            ))),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum FailOn {
    Error,
    Warning,
    Never,
}

impl FailOn {
    fn parse(value: &str) -> Result<Self, CliError> {
        match value {
            "error" => Ok(Self::Error),
            "warning" => Ok(Self::Warning),
            "never" => Ok(Self::Never),
            _ => Err(CliError::Usage(format!(
                "unknown failure threshold: {value}"
            ))),
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct DiagnosticCounts {
    errors: usize,
    warnings: usize,
    information: usize,
    hints: usize,
}

impl DiagnosticCounts {
    fn add(&mut self, severity: diagnostic::Severity) {
        match severity {
            diagnostic::Severity::Error => self.errors += 1,
            diagnostic::Severity::Warning => self.warnings += 1,
            diagnostic::Severity::Information => self.information += 1,
            diagnostic::Severity::Hint => self.hints += 1,
        }
    }

    fn add_host_errors(&mut self, count: usize) {
        self.errors += count;
    }

    const fn fails(self, threshold: FailOn) -> bool {
        match threshold {
            FailOn::Error => self.errors > 0,
            FailOn::Warning => self.errors > 0 || self.warnings > 0,
            FailOn::Never => false,
        }
    }

    fn summary(self) -> String {
        format!(
            "errors={}, warnings={}, information={}, hints={}",
            self.errors, self.warnings, self.information, self.hints
        )
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct CheckOptions {
    format: DiagnosticFormat,
    fail_on: FailOn,
    summary: bool,
    list_rules: bool,
    enabled_rules: Vec<diagnostic::LintRuleId>,
}

struct CheckOutcome {
    output: String,
    counts: DiagnosticCounts,
    fail_on: FailOn,
}

impl CheckOutcome {
    const fn exit_code(&self) -> ExitCode {
        if self.counts.fails(self.fail_on) {
            ExitCode::FAILURE
        } else {
            ExitCode::SUCCESS
        }
    }
}

#[derive(Debug)]
enum CommandOptions {
    Convert {
        complete: bool,
        css: Vec<CssArgument>,
    },
    Check(CheckOptions),
    Format {
        check: bool,
    },
    Symbols,
    ConfigShow,
}

impl CommandOptions {
    const fn operation(&self) -> Operation {
        match self {
            Self::Convert { .. } => Operation::Convert,
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
    include: bool,
    base_dir: Option<PathBuf>,
    allowed_roots: Vec<PathBuf>,
    project_root: Option<PathBuf>,
    config_path: Option<PathBuf>,
    no_config: bool,
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
        "config" => match arguments.next().as_deref() {
            Some("show") => Operation::ConfigShow,
            Some(value) => return Err(CliError::Usage(format!("unknown config command: {value}"))),
            None => return Err(CliError::Usage("config requires a command".to_owned())),
        },
        _ => return Err(CliError::Usage(format!("unknown command: {command}"))),
    };

    let mut input = None;
    let mut stdin_selected = false;
    let mut diagnostic_format = DiagnosticFormat::Human;
    let mut format_selected = false;
    let mut fail_on = FailOn::Error;
    let mut summary = false;
    let mut list_rules = false;
    let mut enabled_rules = Vec::new();
    let mut format_check = false;
    let mut include = false;
    let mut base_dir = None;
    let mut allowed_roots = Vec::new();
    let mut local_targets = false;
    let mut project_root = None;
    let mut complete = false;
    let mut css = Vec::new();
    let mut config_path = None;
    let mut no_config = false;
    while let Some(argument) = arguments.next() {
        match argument.as_str() {
            "-h" | "--help" => return Ok(Action::Help),
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
            "--summary" if operation == Operation::Check => summary = true,
            "--list-rules" if operation == Operation::Check => list_rules = true,
            "--enable-rule" if operation == Operation::Check => {
                let code = arguments
                    .next()
                    .ok_or_else(|| CliError::Usage("--enable-rule requires a code".to_owned()))?;
                let descriptor = diagnostic::lint_rule(&code).ok_or_else(|| {
                    CliError::Usage(format!("unknown or non-enableable rule: {code}"))
                })?;
                if !descriptor.user_configurable {
                    return Err(CliError::Usage(format!(
                        "rule is already enabled by default: {code}"
                    )));
                }
                if !enabled_rules.contains(&descriptor.id) {
                    enabled_rules.push(descriptor.id);
                }
            }
            "--check" if operation == Operation::Format => format_check = true,
            "--include" => include = true,
            "--local-targets" if operation == Operation::Check => local_targets = true,
            "--project-root" if operation == Operation::Check => {
                let value = arguments.next().ok_or_else(|| {
                    CliError::Usage("--project-root requires a directory".to_owned())
                })?;
                project_root = Some(PathBuf::from(value));
            }
            "--complete" if operation == Operation::Convert => complete = true,
            "--css" if operation == Operation::Convert => {
                let value = arguments
                    .next()
                    .ok_or_else(|| CliError::Usage("--css requires a file".to_owned()))?;
                css.push(CssArgument::File(PathBuf::from(value)));
            }
            "--css-url" if operation == Operation::Convert => {
                let value = arguments
                    .next()
                    .ok_or_else(|| CliError::Usage("--css-url requires a URL".to_owned()))?;
                css.push(CssArgument::Url(value));
            }
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
    if local_targets != project_root.is_some() {
        return Err(CliError::Usage(
            "--local-targets and --project-root must be used together".to_owned(),
        ));
    }
    if local_targets && !allowed_roots.is_empty() {
        return Err(CliError::Usage(
            "--allow-root cannot be combined with --local-targets; --project-root is the boundary"
                .to_owned(),
        ));
    }
    if operation == Operation::ConfigShow
        && (input.is_some()
            || stdin_selected
            || include
            || base_dir.is_some()
            || !allowed_roots.is_empty()
            || project_root.is_some()
            || complete
            || !css.is_empty())
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
            || stdin_selected
            || include
            || base_dir.is_some()
            || !allowed_roots.is_empty()
            || local_targets
            || project_root.is_some()
            || !enabled_rules.is_empty()
        {
            return Err(CliError::Usage(
                "--list-rules cannot be combined with document input or include options".to_owned(),
            ));
        }
    }

    let command = match operation {
        Operation::Convert => CommandOptions::Convert { complete, css },
        Operation::Check => CommandOptions::Check(CheckOptions {
            format: diagnostic_format,
            fail_on,
            summary,
            list_rules,
            enabled_rules,
        }),
        Operation::Format => CommandOptions::Format {
            check: format_check,
        },
        Operation::Symbols => CommandOptions::Symbols,
        Operation::ConfigShow => CommandOptions::ConfigShow,
    };
    Ok(Action::Run(Arguments {
        command,
        input,
        include,
        base_dir,
        allowed_roots,
        project_root,
        config_path,
        no_config,
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
) -> Result<RenderPolicy, CliError> {
    let limits = StylesheetPolicy::default();
    let mut sources = Vec::new();
    for path in &project.stylesheet_files {
        let bytes = fs::read(path).map_err(|source| CliError::Read {
            source_name: path.display().to_string(),
            source,
        })?;
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
                let bytes = fs::read(path).map_err(|source| CliError::Read {
                    source_name: path.display().to_string(),
                    source,
                })?;
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
            diagnostic::render_human(
                analysis.diagnostics(),
                analysis.source_document(),
                PositionEncoding::Utf8,
            )
            .map_err(CliError::Position)?
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
    };
    Ok(CheckOutcome {
        output,
        counts,
        fail_on: check.fail_on,
    })
}

fn github_annotation(
    severity: diagnostic::Severity,
    code: &str,
    message: &str,
    source_id: &str,
    line: u32,
    column: u32,
) -> String {
    let command = match severity {
        diagnostic::Severity::Error => "error",
        diagnostic::Severity::Warning => "warning",
        diagnostic::Severity::Information | diagnostic::Severity::Hint => "notice",
    };
    format!(
        "::{command} file={},line={line},col={column},title={}::{}\n",
        github_property(source_id),
        github_property(code),
        github_message(message)
    )
}

fn github_message(value: &str) -> String {
    value
        .replace('%', "%25")
        .replace('\r', "%0D")
        .replace('\n', "%0A")
}

fn github_property(value: &str) -> String {
    github_message(value)
        .replace(':', "%3A")
        .replace(',', "%2C")
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
    if arguments.no_config {
        return Ok(None);
    }
    if let Some(path) = &arguments.config_path {
        return adocweave_config::ConfigSnapshot::load(path)
            .map(Some)
            .map_err(CliError::Config);
    }
    let boundary = env::current_dir().map_err(|source| CliError::Read {
        source_name: "current directory".to_owned(),
        source,
    })?;
    let start = arguments.input.as_deref().unwrap_or(&boundary);
    if !start.exists() {
        return Ok(None);
    }
    match adocweave_config::discover_and_load(start, &boundary) {
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
        .chain(config.local_targets.project_root.iter())
        .chain(config.html.stylesheet_files.iter());
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

fn run() -> Result<ExitCode, CliError> {
    match parse_arguments(env::args().skip(1))? {
        Action::Help => {
            print!("{HELP}");
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
        Action::Run(arguments) => {
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
            validate_project_config_authority(&project_config)?;
            let operation = arguments.command.operation();
            let include = arguments.include || project_config.resources.include;
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
            let input_path = arguments.input.clone();
            let canonical_input = input_path
                .as_ref()
                .map(|path| path.canonicalize().unwrap_or_else(|_| path.clone()));
            let source_id = canonical_input.as_ref().map_or_else(
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
                    |path| {
                        path.canonicalize()
                            .unwrap_or_else(|_| path.clone())
                            .to_string_lossy()
                            .into_owned()
                    },
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
            } else if matches!(&arguments.command, CommandOptions::Format { check: true }) {
                let source = decode_input(&input)?;
                let output =
                    process_format(&input, &project_config.analysis, &project_config.format)?;
                if output != source {
                    return Err(CliError::FormattingRequired);
                }
                Ok((String::new(), ExitCode::SUCCESS))
            } else {
                let output = match &arguments.command {
                    CommandOptions::Convert { complete, css } => process_convert(
                        &processed,
                        &project_config.analysis,
                        &convert_policy(&project_config.html, *complete, css)?,
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
                    CommandOptions::Check(_) => unreachable!("check handled above"),
                };
                Ok((output, ExitCode::SUCCESS))
            }?;
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
    use super::{Action, CommandOptions, DiagnosticFormat, Operation, parse_arguments};

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
            CommandOptions::Format { check: true }
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
