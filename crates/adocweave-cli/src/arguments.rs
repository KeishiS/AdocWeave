//! Parsing of the command line into a typed action.
//!
//! The command surface is described once in `commands::model`. This module
//! turns the arguments a caller typed into the request the rest of the program
//! acts on, and reports a usage error for anything it cannot place.

use std::net::{IpAddr, Ipv4Addr};
use std::path::PathBuf;

use adocweave::output::diagnostics as diagnostic;

use crate::check_output::{DiagnosticFormat, FailOn};

use crate::cli_error::CliError;
use crate::commands::format::Options as FormatOptions;
use crate::commands::html_policy::StylesheetArgument;
use crate::commands::model::{CommandId, LookupError, OptionId, OptionSpec};
use crate::commands::{self, check::Options as CheckOptions};
use crate::{DEFAULT_PREVIEW_DEBOUNCE_MS, DEFAULT_PREVIEW_PORT};

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) enum ColorChoice {
    #[default]
    Auto,
    Always,
    Never,
}

#[derive(Debug)]
pub(crate) enum CommandOptions {
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
    pub(crate) const fn command_id(&self) -> CommandId {
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

pub(crate) struct Arguments {
    pub(crate) command: CommandOptions,
    pub(crate) input: Option<PathBuf>,
    pub(crate) additional_inputs: Vec<PathBuf>,
    pub(crate) glob_patterns: Vec<String>,
    pub(crate) include: bool,
    pub(crate) base_dir: Option<PathBuf>,
    pub(crate) allowed_roots: Vec<PathBuf>,
    pub(crate) project_root: Option<PathBuf>,
    pub(crate) config_path: Option<PathBuf>,
    pub(crate) no_config: bool,
    pub(crate) color: ColorChoice,
}

pub(crate) enum Action {
    Run(Box<Arguments>),
    Help { command: Option<CommandId> },
    Version { json: bool },
    Completion { shell: CompletionShell },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum CompletionShell {
    Bash,
    Zsh,
    Fish,
    PowerShell,
}

fn take_option_value(
    option: &OptionSpec,
    arguments: &mut impl Iterator<Item = String>,
) -> Result<String, CliError> {
    let missing = option
        .missing_value()
        .expect("only valued options request a following value");
    arguments
        .next()
        .ok_or_else(|| CliError::Usage(format!("{} requires {missing}", option.canonical_name())))
}

pub(crate) fn parse_arguments(arguments: impl Iterator<Item = String>) -> Result<Action, CliError> {
    let arguments = arguments.collect::<Vec<_>>();
    let Some(command) = arguments.first() else {
        return Err(CliError::Usage("a command is required".to_owned()));
    };

    if commands::model::root_option(command).is_some_and(|option| option.id == OptionId::Help) {
        return Ok(Action::Help { command: None });
    }
    if commands::model::root_option(command).is_some_and(|option| option.id == OptionId::Version) {
        let mut arguments = arguments.into_iter().skip(1);
        let json = match arguments.next().as_deref() {
            None => false,
            Some(argument)
                if commands::model::version_option(argument)
                    .is_some_and(|option| option.id == OptionId::Json)
                    && arguments.next().is_none() =>
            {
                true
            }
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
        let option = commands::model::option_for_command(command_id, &argument);
        match option.map(|option| option.id) {
            Some(OptionId::Help) => {
                return Ok(Action::Help {
                    command: Some(command_id),
                });
            }
            Some(OptionId::Config) => {
                if no_config {
                    return Err(CliError::Usage(
                        "--config cannot be combined with --no-config".to_owned(),
                    ));
                }
                let value = take_option_value(
                    option.expect("matched option id has a specification"),
                    &mut arguments,
                )?;
                if config_path.replace(PathBuf::from(value)).is_some() {
                    return Err(CliError::Usage(
                        "--config cannot be specified more than once".to_owned(),
                    ));
                }
            }
            Some(OptionId::NoConfig) => {
                if config_path.is_some() {
                    return Err(CliError::Usage(
                        "--no-config cannot be combined with --config".to_owned(),
                    ));
                }
                no_config = true;
            }
            Some(OptionId::Color) => {
                let value = take_option_value(
                    option.expect("matched option id has a specification"),
                    &mut arguments,
                )?;
                color = match value.as_str() {
                    "auto" => ColorChoice::Auto,
                    "always" => ColorChoice::Always,
                    "never" => ColorChoice::Never,
                    _ => return Err(CliError::Usage(format!("unknown color choice: {value}"))),
                };
            }
            Some(OptionId::Glob) => {
                let value = take_option_value(
                    option.expect("matched option id has a specification"),
                    &mut arguments,
                )?;
                glob_patterns.push(value);
            }
            Some(OptionId::Json) => {
                if format_selected && diagnostic_format != DiagnosticFormat::Json {
                    return Err(CliError::Usage(
                        "--json conflicts with another --format value".to_owned(),
                    ));
                }
                diagnostic_format = DiagnosticFormat::Json;
                format_selected = true;
            }
            Some(OptionId::DiagnosticFormat) => {
                let value = take_option_value(
                    option.expect("matched option id has a specification"),
                    &mut arguments,
                )?;
                let parsed = DiagnosticFormat::parse(&value)?;
                if format_selected && parsed != diagnostic_format {
                    return Err(CliError::Usage(
                        "--format cannot be specified with conflicting values".to_owned(),
                    ));
                }
                diagnostic_format = parsed;
                format_selected = true;
            }
            Some(OptionId::FailOn) => {
                let value = take_option_value(
                    option.expect("matched option id has a specification"),
                    &mut arguments,
                )?;
                fail_on = FailOn::parse(&value)?;
            }
            Some(OptionId::Summary) => summary = true,
            Some(OptionId::Fix) => fix = true,
            Some(OptionId::DryRun) => dry_run = true,
            Some(OptionId::ListRules) => list_rules = true,
            Some(OptionId::EnableRule) => {
                let code = take_option_value(
                    option.expect("matched option id has a specification"),
                    &mut arguments,
                )?;
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
            Some(OptionId::FormatCheck) => format_check = true,
            Some(OptionId::FormatWrite) => format_write = true,
            Some(OptionId::FormatDiff) => format_diff = true,
            Some(OptionId::Include) => include = true,
            Some(OptionId::LocalTargets) => local_targets = true,
            Some(OptionId::ProjectRoot) => {
                let value = take_option_value(
                    option.expect("matched option id has a specification"),
                    &mut arguments,
                )?;
                project_root = Some(PathBuf::from(value));
            }
            Some(OptionId::Complete) => complete = true,
            Some(OptionId::Css) => {
                let value = take_option_value(
                    option.expect("matched option id has a specification"),
                    &mut arguments,
                )?;
                css.push(StylesheetArgument::File(PathBuf::from(value)));
            }
            Some(OptionId::CssUrl) => {
                let value = take_option_value(
                    option.expect("matched option id has a specification"),
                    &mut arguments,
                )?;
                css.push(StylesheetArgument::Url(value));
            }
            Some(OptionId::Bind) => {
                let value = take_option_value(
                    option.expect("matched option id has a specification"),
                    &mut arguments,
                )?;
                bind = value
                    .parse()
                    .map_err(|_| CliError::Usage(format!("invalid bind address: {value}")))?;
            }
            Some(OptionId::Port) => {
                let value = take_option_value(
                    option.expect("matched option id has a specification"),
                    &mut arguments,
                )?;
                port = value
                    .parse()
                    .map_err(|_| CliError::Usage(format!("invalid port: {value}")))?;
            }
            Some(OptionId::Debounce) => {
                let value = take_option_value(
                    option.expect("matched option id has a specification"),
                    &mut arguments,
                )?;
                debounce_ms = value
                    .parse()
                    .map_err(|_| CliError::Usage(format!("invalid debounce interval: {value}")))?;
                if debounce_ms == 0 {
                    return Err(CliError::Usage(
                        "--debounce-ms must be greater than zero".to_owned(),
                    ));
                }
            }
            Some(OptionId::AllowExternal) => allow_external = true,
            Some(OptionId::BaseDir) => {
                let value = take_option_value(
                    option.expect("matched option id has a specification"),
                    &mut arguments,
                )?;
                base_dir = Some(PathBuf::from(value));
            }
            Some(OptionId::AllowRoot) => {
                let value = take_option_value(
                    option.expect("matched option id has a specification"),
                    &mut arguments,
                )?;
                allowed_roots.push(PathBuf::from(value));
            }
            Some(OptionId::Version) => {
                unreachable!("version is a root-only option")
            }
            None if command_id == CommandId::ConfigShow
                && commands::model::option_by_name(&argument).is_some() =>
            {
                return Err(CliError::Usage(
                    "config show only accepts --config or --no-config".to_owned(),
                ));
            }
            None if argument == "-" && input.is_none() && !stdin_selected => stdin_selected = true,
            None if argument == "-" => {
                return Err(CliError::Usage(
                    "standard input cannot be combined with file paths".to_owned(),
                ));
            }
            None if input.is_none() && !stdin_selected => input = Some(PathBuf::from(argument)),
            None if matches!(command_id, CommandId::Check | CommandId::Format)
                && !stdin_selected =>
            {
                additional_inputs.push(PathBuf::from(argument));
            }
            None => {
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
