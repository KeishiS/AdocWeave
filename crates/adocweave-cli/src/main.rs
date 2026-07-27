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
  help     Print this message

Arguments:
  [FILE]   Input file; omit it or use '-' to read standard input

Options:
  --json      Emit check diagnostics as JSON
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
            Self::Usage(_)
            | Self::InvalidUtf8 { .. }
            | Self::OutputLimit { .. }
            | Self::FormattingRequired
            | Self::Stylesheet(_) => None,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Operation {
    Convert,
    Check,
    Format,
    Symbols,
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
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct CheckOptions {
    format: DiagnosticFormat,
    list_rules: bool,
    enabled_rules: Vec<diagnostic::LintRuleId>,
}

struct CheckOutcome {
    output: String,
    /// Host-side validation errors make `check` fail. Core diagnostics remain
    /// report output and do not change the process status by themselves.
    has_host_errors: bool,
}

impl CheckOutcome {
    fn success(output: String) -> Self {
        Self {
            output,
            has_host_errors: false,
        }
    }

    const fn exit_code(&self) -> ExitCode {
        if self.has_host_errors {
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
}

impl CommandOptions {
    const fn operation(&self) -> Operation {
        match self {
            Self::Convert { .. } => Operation::Convert,
            Self::Check(_) => Operation::Check,
            Self::Format { .. } => Operation::Format,
            Self::Symbols => Operation::Symbols,
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
    while let Some(argument) = arguments.next() {
        match argument.as_str() {
            "-h" | "--help" => return Ok(Action::Help),
            "--json" if operation == Operation::Check => json = true,
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
    if !complete && !css.is_empty() {
        return Err(CliError::Usage(
            "--css and --css-url require --complete".to_owned(),
        ));
    }
    if list_rules {
        if !json {
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
            format: if json {
                DiagnosticFormat::Json
            } else {
                DiagnosticFormat::Human
            },
            list_rules,
            enabled_rules,
        }),
        Operation::Format => CommandOptions::Format {
            check: format_check,
        },
        Operation::Symbols => CommandOptions::Symbols,
    };
    Ok(Action::Run(Arguments {
        command,
        input,
        include,
        base_dir,
        allowed_roots,
        project_root,
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

fn analyze(source: &str) -> Result<adocweave::Analysis, CliError> {
    Engine::new(AnalysisOptions::default())
        .analyze(source)
        .map_err(CliError::Analysis)
}

fn check_analysis_options(enabled_rules: &[diagnostic::LintRuleId]) -> AnalysisOptions {
    let mut options = AnalysisOptions::default();
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
fn convert_policy(complete: bool, css: &[CssArgument]) -> Result<RenderPolicy, CliError> {
    let limits = StylesheetPolicy::default();
    let mut sources = Vec::new();
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
    Ok(RenderPolicy {
        document_mode: if complete {
            HtmlDocumentMode::Complete
        } else {
            HtmlDocumentMode::Fragment
        },
        stylesheets: StylesheetPolicy { sources, ..limits },
        ..RenderPolicy::default()
    })
}

fn process_convert(input: &[u8], render_policy: &RenderPolicy) -> Result<String, CliError> {
    let source = decode_input(input)?;
    let analysis = analyze(source)?;
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
    format: DiagnosticFormat,
    enabled_rules: &[diagnostic::LintRuleId],
    local: Option<(&std::path::Path, &std::path::Path, &str)>,
) -> Result<CheckOutcome, CliError> {
    let source = decode_input(input)?;
    let analysis = Engine::new(check_analysis_options(enabled_rules))
        .analyze(source)
        .map_err(CliError::Analysis)?;
    let mut host = if let Some((base, root, source_id)) = local {
        let mut targets = analysis.local_targets();
        let snapshot =
            std::iter::empty::<(String, adocweave::preprocess::ResourceDocument)>().collect();
        let include_document = adocweave::preprocess::preprocess(
            source,
            &snapshot,
            &adocweave::preprocess::PreprocessOptions {
                source_id: Some(adocweave::SourceId::new(source_id)),
                enable_includes: false,
                ..adocweave::preprocess::PreprocessOptions::default()
            },
        )
        .map_err(|error| CliError::Include(local_include::LocalIncludeError::Preprocess(error)))?;
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
        let mut diagnostics = local_target::validate(&targets, base, root, source_id, source)
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
    let output = match format {
        DiagnosticFormat::Json => {
            if host.is_empty() {
                return Ok(CheckOutcome::success(diagnostic::render_json(
                    analysis.diagnostics(),
                )));
            }
            let mut values = serde_json::from_str::<Vec<serde_json::Value>>(
                &diagnostic::render_json(analysis.diagnostics()),
            )
            .expect("core diagnostic renderer returns a JSON array");
            values.extend(local_target::json_values(&host));
            serde_json::to_string(&values).expect("diagnostics are serializable")
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
    };
    Ok(CheckOutcome {
        output,
        has_host_errors: !host.is_empty(),
    })
}

fn process_format(input: &[u8]) -> Result<String, CliError> {
    let source = decode_input(input)?;
    let analysis = analyze(source)?;
    Ok(format_analysis(&analysis, &FormatConfig::default())
        .map_err(CliError::Position)?
        .formatted)
}

fn process_symbols(input: &[u8]) -> Result<String, CliError> {
    let source = decode_input(input)?;
    let analysis = analyze(source)?;
    Ok(adocweave::semantic::render_symbols_json(
        &adocweave::semantic::document_symbols(analysis.document()),
    ))
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
            let operation = arguments.command.operation();
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
            let local_context = arguments.project_root.as_ref().map(|project_root| {
                let canonical_input = input_path
                    .as_ref()
                    .map(|path| path.canonicalize().unwrap_or_else(|_| path.clone()));
                let source_id = canonical_input.as_ref().map_or_else(
                    || "<stdin>".to_owned(),
                    |path| path.to_string_lossy().into_owned(),
                );
                let base = canonical_input
                    .as_deref()
                    .and_then(std::path::Path::parent)
                    .map_or_else(|| project_root.clone(), PathBuf::from);
                (base, project_root.clone(), source_id)
            });
            let input = read_input(arguments.input)?;
            let mut prepared = None;
            let processed = if arguments.include {
                let source = decode_input(&input)?;
                let base_dir = match arguments.base_dir {
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
                let include_input = if let Some(project_root) = &arguments.project_root {
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
                    )
                } else {
                    local_include::prepare(
                        source,
                        Some(source_id),
                        &base_dir,
                        &arguments.allowed_roots,
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
                    check_preprocessed(prepared, check).map_err(CliError::Include)
                } else {
                    process_check(
                        &processed,
                        check.format,
                        &check.enabled_rules,
                        local_context.as_ref().map(|(base, root, source_id)| {
                            (base.as_path(), root.as_path(), source_id.as_str())
                        }),
                    )
                }?;
                let exit_code = outcome.exit_code();
                Ok((outcome.output, exit_code))
            } else if matches!(&arguments.command, CommandOptions::Format { check: true }) {
                let source = decode_input(&input)?;
                let output = process_format(&input)?;
                if output != source {
                    return Err(CliError::FormattingRequired);
                }
                Ok((String::new(), ExitCode::SUCCESS))
            } else {
                let output = match &arguments.command {
                    CommandOptions::Convert { complete, css } => {
                        process_convert(&processed, &convert_policy(*complete, css)?)?
                    }
                    CommandOptions::Format { .. } => process_format(&processed)?,
                    CommandOptions::Symbols => process_symbols(&processed)?,
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
) -> Result<CheckOutcome, local_include::LocalIncludeError> {
    let engine = adocweave::Engine::new(check_analysis_options(&check.enabled_rules));
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
                let Some(base) = prepared.source_bases.get(source_id) else {
                    continue;
                };
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
                let optional = directive.is_some_and(|directive| directive.optional);
                let Some(source) = prepared.sources.get(source_id) else {
                    continue;
                };
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
                has_host_errors: !host.is_empty(),
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
        has_host_errors: !host.is_empty(),
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
