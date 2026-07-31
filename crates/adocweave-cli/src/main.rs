use std::env;
use std::fs;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::atomic::AtomicBool;

use adocweave::output::diagnostics as diagnostic;
use adocweave::{AnalysisOptions, OutputLimits};

mod arguments;
mod check_output;
mod cli_error;
mod commands;
mod diagnostic_json;
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

use check_output::{CheckOutcome, DiagnosticCounts, DiagnosticFormat, sarif_log, sarif_results};
use commands::check::Options as CheckOptions;
use commands::format::Options as FormatOptions;
use commands::model::CommandId;
use file_workflow::{PendingWrite, atomic_write_all, colorize_lines};
const DEFAULT_PREVIEW_PORT: u16 = 4000;
const DEFAULT_PREVIEW_DEBOUNCE_MS: u64 = 100;

use adocweave_host::ExitStatus;

use arguments::{Action, Arguments, ColorChoice, CommandOptions, CompletionShell, parse_arguments};
use cli_error::{CliError, check_error, convert_error, format_error, preview_error};

fn read_input(
    path: Option<PathBuf>,
    limits: adocweave_config::AnalysisSnapshotLimits,
) -> Result<Vec<u8>, CliError> {
    let limit = limits.max_resource_bytes.min(limits.max_total_bytes);
    let (mut reader, source_name): (Box<dyn io::Read>, String) = match path {
        Some(path) => (
            Box::new(fs::File::open(&path).map_err(|source| CliError::Read {
                source_name: path.display().to_string(),
                source,
            })?),
            path.display().to_string(),
        ),
        None => (Box::new(io::stdin()), "standard input".to_owned()),
    };
    let mut input = Vec::new();
    reader
        .by_ref()
        .take(limit.saturating_add(1))
        .read_to_end(&mut input)
        .map_err(|source| CliError::Read {
            source_name,
            source,
        })?;
    let bytes = u64::try_from(input.len()).unwrap_or(u64::MAX);
    let mut budget = adocweave_config::AnalysisSnapshotBudget::new(limits);
    budget
        .charge(bytes)
        .map_err(|error| CliError::ResourceLimit(error.to_string()))?;
    Ok(input)
}

fn read_primary_in_session(
    path: &Path,
    filesystem: &mut adocweave_host::LocalFilesystemSession,
) -> Result<Vec<u8>, CliError> {
    let budget_before_read = filesystem.budget();
    let limits = filesystem.limits();
    let loaded = filesystem
        .read_utf8(
            adocweave_host::LogicalSourceId::new(path.to_string_lossy())
                .map_err(local_include::LocalIncludeError::Host)
                .map_err(CliError::Include)?,
            path,
        )
        .map_err(|error| match error {
            adocweave_host::ResourceError::ResourceTooLarge(_) => CliError::ResourceLimit(
                "analysis snapshot single-resource byte limit exceeded".to_owned(),
            ),
            adocweave_host::ResourceError::FileLimit { limit } => CliError::ResourceLimit(format!(
                "filesystem resource count limit exceeded: {limit}"
            )),
            adocweave_host::ResourceError::ByteLimit
                if budget_before_read.files() == 0
                    && limits.max_resource_bytes <= limits.max_total_bytes =>
            {
                CliError::ResourceLimit(
                    "analysis snapshot single-resource byte limit exceeded".to_owned(),
                )
            }
            adocweave_host::ResourceError::ByteLimit => {
                CliError::ResourceLimit("analysis snapshot total byte limit exceeded".to_owned())
            }
            error => CliError::Include(local_include::LocalIncludeError::Host(error)),
        })?;
    let (_, source) = loaded.into_parts();
    Ok(source.as_bytes().to_vec())
}

fn filesystem_for_roots(
    roots: Vec<PathBuf>,
    limits: adocweave_host::FilesystemReadLimits,
) -> Result<adocweave_host::LocalFilesystemSession, CliError> {
    adocweave_host::LocalFilesystemPolicy::new(roots, limits)
        .and_then(|policy| policy.session())
        .map_err(local_include::LocalIncludeError::Host)
        .map_err(CliError::Include)
}

fn include_limits_after_root(
    plan: adocweave_config::ResolvedResourceLimitPlan,
    root_bytes: usize,
) -> Result<adocweave_host::FilesystemReadLimits, CliError> {
    let root_bytes = u64::try_from(root_bytes)
        .map_err(|_| CliError::ResourceLimit("input byte count exceeds u64".to_owned()))?;
    Ok(adocweave_host::FilesystemReadLimits {
        max_files: plan
            .filesystem_reads
            .max_files
            .checked_sub(1)
            .ok_or_else(|| {
                CliError::ResourceLimit(
                    "analysis snapshot resource count limit exceeded".to_owned(),
                )
            })?,
        max_total_bytes: plan
            .filesystem_reads
            .max_total_bytes
            .checked_sub(root_bytes)
            .ok_or_else(|| {
                CliError::ResourceLimit("analysis snapshot total byte limit exceeded".to_owned())
            })?,
        max_resource_bytes: plan.filesystem_reads.max_resource_bytes,
    })
}

fn validate_resource_plan(
    sizes: impl IntoIterator<Item = u64>,
    plan: adocweave_config::ResolvedResourceLimitPlan,
) -> Result<(), CliError> {
    let mut budget = adocweave_config::AnalysisSnapshotBudget::new(plan.analysis_snapshot);
    for size in sizes {
        budget
            .charge(size)
            .map_err(|error| CliError::ResourceLimit(error.to_string()))?;
    }
    Ok(())
}

#[derive(Debug, Default)]
struct ProjectRetainedBudget {
    resources: std::collections::BTreeMap<String, u64>,
    bytes: u64,
}

impl ProjectRetainedBudget {
    fn replace_all(
        &mut self,
        resources: impl IntoIterator<Item = (String, u64)>,
        max_files: usize,
        max_total_bytes: u64,
        max_resource_bytes: u64,
    ) -> Result<(), CliError> {
        let replacements = resources
            .into_iter()
            .collect::<std::collections::BTreeMap<_, _>>();
        if replacements
            .values()
            .any(|bytes| *bytes > max_resource_bytes)
        {
            return Err(Self::limit_error());
        }
        let new_files = replacements
            .keys()
            .filter(|id| !self.resources.contains_key(*id))
            .count();
        let files = self
            .resources
            .len()
            .checked_add(new_files)
            .ok_or_else(Self::limit_error)?;
        let replaced_bytes = replacements.keys().try_fold(0_u64, |total, id| {
            total.checked_add(self.resources.get(id).copied().unwrap_or(0))
        });
        let replacement_bytes = replacements
            .values()
            .try_fold(0_u64, |total, bytes| total.checked_add(*bytes));
        let bytes = replaced_bytes
            .and_then(|replaced| self.bytes.checked_sub(replaced))
            .zip(replacement_bytes)
            .and_then(|(retained, replacement)| retained.checked_add(replacement))
            .ok_or_else(Self::limit_error)?;
        if files > max_files || bytes > max_total_bytes {
            return Err(Self::limit_error());
        }
        self.resources.extend(replacements);
        self.bytes = bytes;
        Ok(())
    }

    fn limit_error() -> CliError {
        CliError::ResourceLimit("configured retained resource limit exceeded".to_owned())
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
    limits: adocweave_host::FilesystemReadLimits,
    preprocess: &'request adocweave::preprocess::PreprocessOptions,
    filesystem: Option<&'request mut adocweave_host::LocalFilesystemSession>,
}

fn prepare_includes(
    mut request: IncludePreparation<'_>,
) -> Result<local_include::PreparedInput, local_include::LocalIncludeError> {
    if let (Some(project_root), Some(filesystem)) =
        (request.project_root, request.filesystem.as_deref_mut())
    {
        local_include::prepare_local_with_session(
            request.source,
            request.source_id,
            request.base_dir,
            request.source_base,
            project_root,
            request.preprocess,
            filesystem,
        )
    } else if let Some(project_root) = request.project_root {
        local_include::prepare_local(
            request.source,
            request.source_id,
            request.base_dir,
            request.source_base,
            project_root,
            request.limits,
            request.preprocess,
        )
    } else if let Some(filesystem) = request.filesystem.as_deref_mut() {
        local_include::prepare_with_session(
            request.source,
            Some(request.source_id),
            request.base_dir,
            request.allowed_roots,
            request.preprocess,
            filesystem,
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
    resource_limits: adocweave_host::FilesystemReadLimits,
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

const MAX_SCAN_ENTRIES: usize = 100_000;

fn charge_scan_entry(scanned_entries: &mut usize) -> Result<(), CliError> {
    *scanned_entries = scanned_entries.saturating_add(1);
    if *scanned_entries > MAX_SCAN_ENTRIES {
        return Err(CliError::Path(
            "directory scan entry limit exceeded".to_owned(),
        ));
    }
    Ok(())
}

fn collect_input_paths(arguments: &Arguments) -> Result<Vec<PathBuf>, CliError> {
    let mut pending = arguments
        .input
        .iter()
        .chain(&arguments.additional_inputs)
        .cloned()
        .collect::<Vec<_>>();
    let mut scanned_entries = pending.len();
    if scanned_entries > MAX_SCAN_ENTRIES {
        return Err(CliError::Path(
            "directory scan entry limit exceeded".to_owned(),
        ));
    }
    for pattern in &arguments.glob_patterns {
        let matches = glob::glob(pattern)
            .map_err(|error| CliError::Path(format!("invalid glob pattern {pattern}: {error}")))?;
        for path in matches {
            charge_scan_entry(&mut scanned_entries)?;
            pending.push(
                path.map_err(|error| CliError::Path(format!("cannot read glob match: {error}")))?,
            );
        }
    }
    pending.sort();
    let mut files = std::collections::BTreeSet::new();
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
                charge_scan_entry(&mut scanned_entries)?;
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
    }
    Ok(files.into_iter().collect())
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct CliProjectScopeId {
    workspace_root: PathBuf,
    config_path: Option<PathBuf>,
}

fn cli_project_scope(
    path: &Path,
    snapshot: Option<&adocweave_config::ConfigSnapshot>,
) -> CliProjectScopeId {
    let config_path = snapshot.map(|snapshot| snapshot.path.clone());
    let workspace_root = config_path
        .as_deref()
        .and_then(Path::parent)
        .or_else(|| path.parent())
        .unwrap_or_else(|| Path::new(""))
        .to_owned();
    CliProjectScopeId {
        workspace_root,
        config_path,
    }
}

#[derive(Clone, Debug)]
struct ResolvedCliInput {
    scope: CliProjectScopeId,
    config: adocweave_config::ResolvedProjectConfig,
}

fn resolve_input_path_scopes(
    arguments: &Arguments,
    paths: &[PathBuf],
    boundary: &Path,
) -> Result<std::collections::BTreeMap<PathBuf, ResolvedCliInput>, CliError> {
    resolve_input_path_scopes_with_hook(arguments, paths, boundary, |_| {})
}

fn resolve_input_path_scopes_with_hook(
    arguments: &Arguments,
    paths: &[PathBuf],
    boundary: &Path,
    mut after_path: impl FnMut(usize),
) -> Result<std::collections::BTreeMap<PathBuf, ResolvedCliInput>, CliError> {
    let mut scopes = std::collections::BTreeMap::<
        CliProjectScopeId,
        (usize, adocweave_config::ResolvedProjectConfig),
    >::new();
    let mut resolved = std::collections::BTreeMap::new();
    for (index, path) in paths.iter().enumerate() {
        let snapshot = load_project_config_at(arguments, path, boundary)?;
        let scope = cli_project_scope(path, snapshot.as_ref());
        let config = snapshot.as_ref().map_or_else(
            adocweave_config::ResolvedProjectConfig::default,
            |snapshot| snapshot.config.clone(),
        );
        let entry = scopes.entry(scope.clone()).or_insert((0, config.clone()));
        if entry.1 != config {
            return Err(CliError::ResourceLimit(
                "project configuration changed while collecting inputs".to_owned(),
            ));
        }
        entry.0 = entry.0.saturating_add(1);
        let limit = entry.1.resources.limit_plan.filesystem_reads.max_files;
        if entry.0 > limit {
            return Err(CliError::ResourceLimit(format!(
                "filesystem resource count limit exceeded: {}",
                limit
            )));
        }
        resolved.insert(
            path.clone(),
            ResolvedCliInput {
                scope,
                config: entry.1.clone(),
            },
        );
        after_path(index);
    }
    Ok(resolved)
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
    let resolved_inputs = resolve_input_path_scopes(arguments, &paths, &boundary)?;
    let mut project_filesystems = std::collections::BTreeMap::<
        CliProjectScopeId,
        adocweave_host::LocalFilesystemSession,
    >::new();
    let mut project_retained =
        std::collections::BTreeMap::<CliProjectScopeId, ProjectRetainedBudget>::new();
    match &arguments.command {
        CommandOptions::Format(options) => {
            if !options.supports_multiple_inputs() {
                return Err(CliError::Usage(
                    "multiple format inputs require --check, --write, or --diff".to_owned(),
                ));
            }
            let mut workflow = commands::format::BatchWorkflow::new(*options, paths.len());
            for path in &paths {
                let resolved = resolved_inputs
                    .get(path)
                    .expect("every collected input has a resolved project");
                let config = &resolved.config;
                let include = arguments.include || config.resources.include;
                if !include && (arguments.base_dir.is_some() || !arguments.allowed_roots.is_empty())
                {
                    return Err(CliError::Usage(
                        "--base-dir and --allow-root require include processing".to_owned(),
                    ));
                }
                if include {
                    validate_project_config_authority(
                        config,
                        arguments.allowed_roots.is_empty(),
                        false,
                        false,
                    )?;
                }
                let source_base = path.parent().expect("canonical input path has a parent");
                let base_dir = arguments.base_dir.as_deref().unwrap_or(source_base);
                let allowed_roots = if arguments.allowed_roots.is_empty() {
                    &config.resources.roots
                } else {
                    &arguments.allowed_roots
                };
                let project_key = resolved.scope.clone();
                let filesystem = match project_filesystems.entry(project_key.clone()) {
                    std::collections::btree_map::Entry::Occupied(entry) => entry.into_mut(),
                    std::collections::btree_map::Entry::Vacant(entry) => {
                        let mut roots = vec![boundary.clone()];
                        roots.extend(
                            paths
                                .iter()
                                .filter_map(|path| path.parent().map(Path::to_owned)),
                        );
                        roots.extend(allowed_roots.iter().cloned());
                        entry.insert(filesystem_for_roots(
                            roots,
                            config.resources.limit_plan.filesystem_reads,
                        )?)
                    }
                };
                let original = read_primary_in_session(path, filesystem)?;
                if include {
                    let source = decode_input(&original)?;
                    let prepared = local_include::prepare_with_session(
                        source,
                        Some(path.to_string_lossy().into_owned()),
                        base_dir,
                        allowed_roots,
                        &config.preprocess,
                        filesystem,
                    )
                    .map_err(CliError::Include)?;
                    validate_resource_plan(prepared.resource_sizes(), config.resources.limit_plan)?;
                    let retained_limits = config.resources.limit_plan.retained_layers;
                    project_retained
                        .entry(project_key)
                        .or_default()
                        .replace_all(
                            prepared
                                .resource_entries()
                                .map(|(id, bytes)| (id.to_owned(), bytes)),
                            retained_limits.max_files,
                            retained_limits.max_total_bytes,
                            retained_limits.max_resource_bytes,
                        )?;
                } else {
                    validate_resource_plan([original.len() as u64], config.resources.limit_plan)?;
                    let retained_limits = config.resources.limit_plan.retained_layers;
                    project_retained
                        .entry(project_key)
                        .or_default()
                        .replace_all(
                            [(path.to_string_lossy().into_owned(), original.len() as u64)],
                            retained_limits.max_files,
                            retained_limits.max_total_bytes,
                            retained_limits.max_resource_bytes,
                        )?;
                }
                let format_config = commands::format::format_config(*options, &original, config);
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
                ExitStatus::Diagnostics.into()
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
                let resolved = resolved_inputs
                    .get(path)
                    .expect("every collected input has a resolved project");
                let config = &resolved.config;
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
                    config,
                    include && arguments.allowed_roots.is_empty(),
                    project_root.is_some() && arguments.project_root.is_none(),
                    false,
                )?;
                let source_base = path
                    .parent()
                    .expect("canonical input path has a parent")
                    .to_path_buf();
                let allowed_roots = if arguments.allowed_roots.is_empty() {
                    &config.resources.roots
                } else {
                    &arguments.allowed_roots
                };
                let project_key = resolved.scope.clone();
                let filesystem = match project_filesystems.entry(project_key.clone()) {
                    std::collections::btree_map::Entry::Occupied(entry) => entry.into_mut(),
                    std::collections::btree_map::Entry::Vacant(entry) => {
                        let mut roots = vec![boundary.clone()];
                        roots.extend(
                            paths
                                .iter()
                                .filter_map(|path| path.parent().map(Path::to_owned)),
                        );
                        roots.extend(allowed_roots.iter().cloned());
                        roots.extend(project_root.iter().cloned());
                        entry.insert(filesystem_for_roots(
                            roots,
                            config.resources.limit_plan.filesystem_reads,
                        )?)
                    }
                };
                let original = read_primary_in_session(path, filesystem)?;
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
                let local_context = project_root
                    .as_ref()
                    .map(|root| (source_base.as_path(), root.as_path(), source_id.as_ref()));
                let outcome = if include {
                    let source = decode_input(&checked)?;
                    let base_dir = arguments
                        .base_dir
                        .as_deref()
                        .unwrap_or(source_base.as_path());
                    let mut prepared = prepare_includes(IncludePreparation {
                        source,
                        source_id: source_id.to_string(),
                        base_dir,
                        source_base: &source_base,
                        project_root: project_root.as_deref(),
                        allowed_roots,
                        limits: config.resources.limit_plan.filesystem_reads,
                        preprocess: &config.preprocess,
                        filesystem: Some(filesystem),
                    })
                    .map_err(CliError::Include)?;
                    validate_resource_plan(prepared.resource_sizes(), config.resources.limit_plan)?;
                    let retained_limits = config.resources.limit_plan.retained_layers;
                    project_retained
                        .entry(project_key)
                        .or_default()
                        .replace_all(
                            prepared
                                .resource_entries()
                                .map(|(id, bytes)| (id.to_owned(), bytes)),
                            retained_limits.max_files,
                            retained_limits.max_total_bytes,
                            retained_limits.max_resource_bytes,
                        )?;
                    check_preprocessed(&mut prepared, check, &config.analysis)?
                } else {
                    validate_resource_plan([checked.len() as u64], config.resources.limit_plan)?;
                    let retained_limits = config.resources.limit_plan.retained_layers;
                    project_retained
                        .entry(project_key)
                        .or_default()
                        .replace_all(
                            [(path.to_string_lossy().into_owned(), checked.len() as u64)],
                            retained_limits.max_files,
                            retained_limits.max_total_bytes,
                            retained_limits.max_resource_bytes,
                        )?;
                    process_check(
                        &checked,
                        check,
                        &source_id,
                        &config.analysis,
                        &config.preprocess,
                        config.resources.limit_plan.filesystem_reads,
                        local_context,
                    )?
                };
                counts.merge(outcome.counts);
                if check.format == DiagnosticFormat::Json {
                    // Every record already carries its own source, so the batch
                    // only concatenates what each document produced.
                    machine_results.extend(
                        serde_json::from_str::<Vec<serde_json::Value>>(&outcome.output)
                            .map_err(|error| CliError::Usage(error.to_string()))?,
                    );
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
                ExitStatus::Diagnostics.into()
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
    let completion_words = |command| {
        let mut words = Vec::new();
        for option in commands::model::options_for_command(command) {
            for value in option.names.iter().chain(option.candidates()) {
                if !words.contains(value) {
                    words.push(*value);
                }
            }
        }
        words
    };
    let mut contract = format!("# adocweave-command-tree root={}\n", tree.roots.join(","));
    for group in &tree.nested {
        contract.push_str(&format!(
            "# adocweave-command-tree parent={} children={}\n",
            group.parent.join("/"),
            group.children.join(",")
        ));
    }
    for (command, path) in &tree.commands {
        for option in commands::model::options_for_command(*command) {
            contract.push_str(&format!(
                "# adocweave-option command={} names={} metavar={} values={}\n",
                path.join("/"),
                option.names.join(","),
                option.metavar().unwrap_or("-"),
                option.candidates().join(","),
            ));
        }
    }
    let rendered = match shell {
        CompletionShell::Bash => {
            let nested_declarations = tree
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
            let option_declarations = tree
                .commands
                .iter()
                .enumerate()
                .filter_map(|(index, (command, _))| {
                    let options = completion_words(*command);
                    (!options.is_empty()).then(|| {
                        format!(
                            "  local command_options_{index}=\"{}\"",
                            shell_words(&options)
                        )
                    })
                })
                .collect::<Vec<_>>()
                .join("\n");
            let nested_branches = tree
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
            let option_branches = tree
                .commands
                .iter()
                .enumerate()
                .filter_map(|(index, (command, path))| {
                    let options = completion_words(*command);
                    if options.is_empty() {
                        return None;
                    }
                    let conditions = path
                        .iter()
                        .enumerate()
                        .map(|(position, token)| {
                            format!(
                                "${{COMP_WORDS[{position_plus_one}]}} == {token}",
                                position_plus_one = position + 1
                            )
                        })
                        .collect::<Vec<_>>()
                        .join(" && ");
                    Some(format!(
                        "  elif [[ ${{COMP_CWORD}} -gt {path_len} && {conditions} ]]; then\n    COMPREPLY=( $(compgen -W \"${{command_options_{index}}}\" -f -- \"${{COMP_WORDS[COMP_CWORD]}}\") )",
                        path_len = path.len(),
                    ))
                })
                .collect::<Vec<_>>()
                .join("\n");
            r#"_adocweave() {
  local commands="@ROOTS@"
@NESTED_DECLARATIONS@
@OPTION_DECLARATIONS@
  if [[ ${COMP_CWORD} -eq 1 ]]; then
    COMPREPLY=( $(compgen -W "${commands}" -- "${COMP_WORDS[COMP_CWORD]}") )
@NESTED_BRANCHES@
@OPTION_BRANCHES@
  else
    COMPREPLY=( $(compgen -f -- "${COMP_WORDS[COMP_CWORD]}") )
  fi
}
complete -F _adocweave adocweave
"#
            .replace("@ROOTS@", &shell_words(&tree.roots))
            .replace("@NESTED_DECLARATIONS@", &nested_declarations)
            .replace("@OPTION_DECLARATIONS@", &option_declarations)
            .replace("@NESTED_BRANCHES@", &nested_branches)
            .replace("@OPTION_BRANCHES@", &option_branches)
        }
        CompletionShell::Zsh => {
            let nested_branches = tree
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
            let option_branches = tree
                .commands
                .iter()
                .filter_map(|(command, path)| {
                    let options = completion_words(*command);
                    if options.is_empty() {
                        return None;
                    }
                    let conditions = path
                        .iter()
                        .enumerate()
                        .map(|(position, token)| {
                            format!("$words[{}] == {token}", position + 2)
                        })
                        .collect::<Vec<_>>()
                        .join(" && ");
                    Some(format!(
                        "  elif [[ $CURRENT -gt {current} && {conditions} ]]; then\n    _values 'arguments for {parent}' {options}",
                        current = path.len() + 1,
                        parent = path.join(" "),
                        options = shell_words(&options),
                    ))
                })
                .collect::<Vec<_>>()
                .join("\n");
            r#"#compdef adocweave
_adocweave() {
  if (( CURRENT == 2 )); then
    _values 'commands' @ROOTS@
@NESTED_BRANCHES@
@OPTION_BRANCHES@
  else
    _files
  fi
}
compdef _adocweave adocweave
"#
            .replace("@ROOTS@", &shell_words(&tree.roots))
            .replace("@NESTED_BRANCHES@", &nested_branches)
            .replace("@OPTION_BRANCHES@", &option_branches)
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
            let options = tree
                .commands
                .iter()
                .flat_map(|(command, path)| {
                    commands::model::options_for_command(*command).map(move |option| {
                        let names = option
                            .names
                            .iter()
                            .map(|name| {
                                if let Some(long) = name.strip_prefix("--") {
                                    format!("-l {long}")
                                } else {
                                    format!("-s {}", &name[1..])
                                }
                            })
                            .collect::<Vec<_>>()
                            .join(" ");
                        let value = option.metavar().map_or_else(String::new, |_| {
                            let candidates = option.candidates();
                            if candidates.is_empty() {
                                " -r".to_owned()
                            } else {
                                format!(" -r -a '{}'", shell_words(candidates))
                            }
                        });
                        format!(
                            "complete -c adocweave -f -n '__adocweave_uses_command {}' {names}{value}",
                            path.join(" "),
                        )
                    })
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
function __adocweave_uses_command
  set -l expected $argv
  set -l words (commandline -opc)
  test (count $words) -ge (math (count $expected) + 1); or return 1
  for index in (seq (count $expected))
    test $words[(math $index + 1)] = $expected[$index]; or return 1
  end
end
complete -c adocweave -f -n '__fish_use_subcommand' -a '@ROOTS@'
@NESTED@
@OPTIONS@
"#
            .replace("@ROOTS@", &shell_words(&tree.roots))
            .replace("@NESTED@", &nested)
            .replace("@OPTIONS@", &options)
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
            let option_branches = tree
                .commands
                .iter()
                .filter_map(|(command, path)| {
                    let options = completion_words(*command);
                    if options.is_empty() {
                        return None;
                    }
                    let conditions = path
                        .iter()
                        .enumerate()
                        .map(|(position, token)| format!("$words[{}] -eq '{token}'", position + 1))
                        .collect::<Vec<_>>()
                        .join(" -and ");
                    Some(format!(
                        "  }} elseif ({conditions}) {{\n    @({options})",
                        options = powershell_words(&options),
                    ))
                })
                .collect::<Vec<_>>()
                .join("\n");
            r#"Register-ArgumentCompleter -Native -CommandName adocweave -ScriptBlock {
  param($wordToComplete, $commandAst, $cursorPosition)
  $words = @($commandAst.CommandElements | ForEach-Object { $_.Value })
  $candidates = if ($false) {
    @()
@NESTED@
@OPTION_BRANCHES@
  } elseif ($words.Count -le 2) {
    @(@ROOTS@)
  } else {
    @()
  }
  $candidates |
    Where-Object { $_ -like "$wordToComplete*" } |
    ForEach-Object { [System.Management.Automation.CompletionResult]::new($_, $_, 'ParameterValue', $_) }
}
"#
            .replace("@ROOTS@", &powershell_words(&tree.roots))
            .replace("@NESTED@", &nested)
            .replace("@OPTION_BRANCHES@", &option_branches)
        }
    };
    format!("{contract}{rendered}")
}

fn run() -> Result<ExitCode, CliError> {
    match parse_arguments(env::args().skip(1))? {
        Action::Help { command } => {
            let help = command.map_or_else(commands::model::root_help, |id| {
                commands::model::command_help(id).expect("document commands have command help")
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
            let primary_base = canonical_input
                .as_deref()
                .and_then(Path::parent)
                .map(Path::to_owned);
            let (input, mut primary_filesystem) = if let Some(path) = canonical_input.as_deref() {
                let mut roots = vec![
                    primary_base
                        .clone()
                        .expect("canonical input path has a parent"),
                ];
                roots.extend(allowed_roots.iter().cloned());
                roots.extend(project_root.iter().cloned());
                let mut filesystem = filesystem_for_roots(
                    roots,
                    project_config.resources.limit_plan.filesystem_reads,
                )?;
                let input = read_primary_in_session(path, &mut filesystem)?;
                (input, Some(filesystem))
            } else {
                (
                    read_input(None, project_config.resources.limit_plan.analysis_snapshot)?,
                    None,
                )
            };
            validate_resource_plan([input.len() as u64], project_config.resources.limit_plan)?;
            let mut retained_resources = ProjectRetainedBudget::default();
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
                    limits: if primary_filesystem.is_some() {
                        project_config.resources.limit_plan.filesystem_reads
                    } else {
                        include_limits_after_root(project_config.resources.limit_plan, input.len())?
                    },
                    preprocess: &project_config.preprocess,
                    filesystem: primary_filesystem.as_mut(),
                })
                .map_err(CliError::Include)?;
                validate_resource_plan(
                    include_input.resource_sizes(),
                    project_config.resources.limit_plan,
                )?;
                let retained_limits = project_config.resources.limit_plan.retained_layers;
                retained_resources.replace_all(
                    include_input
                        .resource_entries()
                        .map(|(id, bytes)| (id.to_owned(), bytes)),
                    retained_limits.max_files,
                    retained_limits.max_total_bytes,
                    retained_limits.max_resource_bytes,
                )?;
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
                let retained_limits = project_config.resources.limit_plan.retained_layers;
                retained_resources.replace_all(
                    [(source_id.clone(), input.len() as u64)],
                    retained_limits.max_files,
                    retained_limits.max_total_bytes,
                    retained_limits.max_resource_bytes,
                )?;
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
                        project_config.resources.limit_plan.filesystem_reads,
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
            let status = error.exit_status();
            eprintln!("adocweave: {error}");
            // Only a caller who wrote the command wrong is helped by being sent
            // to the help text.
            if status == ExitStatus::Usage {
                eprintln!("Try 'adocweave --help' for more information.");
            }
            status.into()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        Action, CommandOptions, CompletionShell, DEFAULT_PREVIEW_DEBOUNCE_MS, DEFAULT_PREVIEW_PORT,
        DiagnosticFormat, FormatOptions, MAX_SCAN_ENTRIES, ProjectRetainedBudget,
        charge_scan_entry, cli_project_scope, parse_arguments, render_completion_script,
        resolve_input_path_scopes_with_hook,
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
                help_options: &[],
            },
            model::CommandSpec {
                id: CommandId::Help,
                path: &["project", "status"],
                root_usage: "",
                summary: "show project status",
                help: None,
                help_options: &[],
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
    fn completion_renderers_use_every_model_option_and_value_candidate() {
        let tree = model::completion_tree();
        for shell in [
            CompletionShell::Bash,
            CompletionShell::Zsh,
            CompletionShell::Fish,
            CompletionShell::PowerShell,
        ] {
            let output = render_completion_script(shell, &tree);
            let body_marker = match shell {
                CompletionShell::Bash => "_adocweave() {",
                CompletionShell::Zsh => "#compdef adocweave",
                CompletionShell::Fish => "function __adocweave_at_path",
                CompletionShell::PowerShell => {
                    "Register-ArgumentCompleter -Native -CommandName adocweave"
                }
            };
            let body = output
                .split_once(body_marker)
                .map(|(_, body)| body)
                .expect("completion output contains its shell-specific body");
            for (command, path) in &tree.commands {
                for option in model::options_for_command(*command) {
                    let contract = format!(
                        "# adocweave-option command={} names={} metavar={} values={}",
                        path.join("/"),
                        option.names.join(","),
                        option.metavar().unwrap_or("-"),
                        option.candidates().join(","),
                    );
                    assert!(output.contains(&contract), "{shell:?}: {contract}");
                    for token in option.names.iter().chain(option.candidates()) {
                        let rendered = match shell {
                            CompletionShell::Fish if token.starts_with("--") => {
                                format!("-l {}", &token[2..])
                            }
                            CompletionShell::Fish if token.starts_with('-') => {
                                format!("-s {}", &token[1..])
                            }
                            _ => (*token).to_owned(),
                        };
                        assert!(
                            body.contains(&rendered),
                            "{shell:?} did not render {token} from {command:?} as {rendered}"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn parser_accepts_every_typed_value_candidate() {
        for candidate in model::option(model::OptionId::DiagnosticFormat).candidates() {
            assert!(
                parse_arguments(arguments(&["check", "--format", candidate])).is_ok(),
                "diagnostic format {candidate}"
            );
        }
        for candidate in model::option(model::OptionId::FailOn).candidates() {
            assert!(
                parse_arguments(arguments(&["check", "--fail-on", candidate])).is_ok(),
                "failure level {candidate}"
            );
        }
        for candidate in model::option(model::OptionId::Color).candidates() {
            assert!(
                parse_arguments(arguments(&["symbols", "--color", candidate])).is_ok(),
                "color choice {candidate}"
            );
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
        let help = model::command_help(CommandId::Preview).expect("preview has command help");
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

    #[test]
    fn scan_candidate_counter_rejects_the_first_entry_past_the_cap() {
        let mut scanned = MAX_SCAN_ENTRIES - 1;
        charge_scan_entry(&mut scanned).expect("exact scan boundary");
        assert_eq!(scanned, MAX_SCAN_ENTRIES);
        let error = charge_scan_entry(&mut scanned).expect_err("entry past scan boundary");
        assert!(error.to_string().contains("scan entry limit"));
    }

    #[test]
    fn configless_input_folders_have_distinct_project_scopes() {
        let first = cli_project_scope(std::path::Path::new("/workspace/one/a.adoc"), None);
        let same = cli_project_scope(std::path::Path::new("/workspace/one/b.adoc"), None);
        let second = cli_project_scope(std::path::Path::new("/workspace/two/a.adoc"), None);

        assert_eq!(first, same);
        assert_ne!(first, second);

        let mut budgets = std::collections::BTreeMap::new();
        budgets
            .entry(first)
            .or_insert_with(ProjectRetainedBudget::default)
            .replace_all([("a".to_owned(), 1)], 1, 1, 1)
            .expect("first project boundary");
        budgets
            .entry(second)
            .or_insert_with(ProjectRetainedBudget::default)
            .replace_all([("a".to_owned(), 1)], 1, 1, 1)
            .expect("second project has an independent budget");
    }

    #[test]
    fn multi_path_resolution_pins_one_project_plan_before_processing() {
        let directory = tempfile::tempdir().expect("temporary project");
        let first = directory.path().join("first.adoc");
        let second = directory.path().join("second.adoc");
        std::fs::write(&first, "first").expect("first source");
        std::fs::write(&second, "second").expect("second source");
        let config = directory.path().join(adocweave_config::FILE_NAME);
        std::fs::write(
            &config,
            "schema-version = 1\n[resources]\nroots = [\".\"]\nmax-files = 2\nmax-total-bytes = 16\nmax-resource-bytes = 8\n",
        )
        .expect("initial config");
        let Action::Run(arguments) = parse_arguments(arguments(&[
            "format",
            "--check",
            "first.adoc",
            "second.adoc",
        ]))
        .expect("multi-path arguments") else {
            panic!("expected run action");
        };
        let error = resolve_input_path_scopes_with_hook(
            &arguments,
            &[first.clone(), second.clone()],
            directory.path(),
            |index| {
                if index == 0 {
                    std::fs::write(
                        &config,
                        "schema-version = 1\n[resources]\nroots = [\".\"]\nmax-files = 2\nmax-total-bytes = 1\nmax-resource-bytes = 1\n",
                    )
                    .expect("stricter config");
                }
            },
        )
        .expect_err("configuration changed between paths");
        assert!(
            error
                .to_string()
                .contains("project configuration changed while collecting inputs"),
            "{error}"
        );
    }

    #[test]
    fn project_retained_budget_applies_replacements_without_partial_failure() {
        let mut budget = ProjectRetainedBudget::default();
        budget
            .replace_all([("a".to_owned(), 2)], 2, 4, 3)
            .expect("first resource");
        budget
            .replace_all([("b".to_owned(), 2)], 2, 4, 3)
            .expect("total boundary");
        budget
            .replace_all([("a".to_owned(), 1)], 2, 4, 3)
            .expect("replacement");
        assert_eq!(budget.bytes, 3);

        let committed = budget.resources.clone();
        assert!(budget.replace_all([("c".to_owned(), 1)], 2, 4, 3).is_err());
        assert_eq!(budget.resources, committed);
        assert_eq!(budget.bytes, 3);
        assert!(budget.replace_all([("a".to_owned(), 4)], 2, 4, 3).is_err());
        assert_eq!(budget.resources, committed);
        assert_eq!(budget.bytes, 3);
    }

    #[test]
    fn project_retained_budget_handles_a_large_batch_without_recloning_committed_entries() {
        let mut budget = ProjectRetainedBudget::default();
        budget
            .replace_all(
                (0..10_000).map(|index| (format!("resource-{index:05}"), 1)),
                10_000,
                10_000,
                1,
            )
            .expect("large boundary");
        budget
            .replace_all([("resource-09999".to_owned(), 1)], 10_000, 10_000, 1)
            .expect("single replacement");
        assert_eq!(budget.resources.len(), 10_000);
        assert_eq!(budget.bytes, 10_000);
    }
}
