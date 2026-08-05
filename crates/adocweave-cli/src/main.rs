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

use adocweave_config::ProjectScopeId;
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

fn retained_write_policy(
    path: &Path,
    filesystem: &adocweave_host::LocalFilesystemSession,
) -> Result<adocweave_host::LocalTargetPolicy, CliError> {
    filesystem.policy_for_path(path).cloned().ok_or_else(|| {
        CliError::Path(format!(
            "write target is outside its retained filesystem authority: {}",
            path.display()
        ))
    })
}

fn filesystem_authority(
    boundary: PathBuf,
) -> Result<adocweave_host::LocalFilesystemPolicy, CliError> {
    adocweave_host::LocalFilesystemPolicy::new(
        [boundary],
        adocweave_host::FilesystemReadLimits::default(),
    )
    .map_err(local_include::LocalIncludeError::Host)
    .map_err(CliError::Include)
}

fn resolve_primary_path(
    path: &Path,
    boundary_policy: &adocweave_host::LocalTargetPolicy,
) -> PathBuf {
    let candidate = if path.is_absolute() {
        path.to_owned()
    } else {
        boundary_policy.root().join(path)
    };
    match boundary_policy.normalize_candidate(&candidate) {
        Ok(candidate) => boundary_policy
            .inspect_candidate(&candidate)
            .unwrap_or(candidate),
        Err(adocweave_host::LocalTargetError::OutsideRoot(_)) => {
            path.canonicalize().unwrap_or_else(|_| path.to_owned())
        }
        Err(_) => candidate,
    }
}

fn filesystem_from_authority(
    authority: &mut adocweave_host::LocalFilesystemPolicy,
    anchor: &Path,
    confined_roots: Vec<PathBuf>,
    independent_roots: Vec<PathBuf>,
    limits: adocweave_host::FilesystemReadLimits,
) -> Result<adocweave_host::LocalFilesystemSession, CliError> {
    filesystem_access_from_authority(authority, anchor, confined_roots, independent_roots, limits)?
        .session()
        .map_err(local_include::LocalIncludeError::Host)
        .map_err(CliError::Include)
}

fn filesystem_access_from_authority(
    authority: &mut adocweave_host::LocalFilesystemPolicy,
    anchor: &Path,
    confined_roots: Vec<PathBuf>,
    independent_roots: Vec<PathBuf>,
    limits: adocweave_host::FilesystemReadLimits,
) -> Result<adocweave_host::LocalFilesystemAccess, CliError> {
    authority
        .access_derived(
            anchor,
            adocweave_host::DerivedFilesystemRoots {
                confined: confined_roots,
                independent: independent_roots,
            },
            limits,
        )
        .map_err(local_include::LocalIncludeError::Host)
        .map_err(CliError::Include)
}

fn partition_roots_below_anchor(
    anchor: &Path,
    roots: impl IntoIterator<Item = PathBuf>,
    confined: &mut Vec<PathBuf>,
    independent: &mut Vec<PathBuf>,
) {
    for root in roots {
        if root.starts_with(anchor) {
            confined.push(root);
        } else {
            independent.push(root);
        }
    }
}

fn processing_filesystem_roots(
    anchor: &Path,
    primary_roots: impl IntoIterator<Item = PathBuf>,
    arguments: &Arguments,
    allowed_roots: &[PathBuf],
    project_root: Option<&Path>,
) -> (Vec<PathBuf>, Vec<PathBuf>) {
    let mut confined = Vec::new();
    let mut independent = Vec::new();
    partition_roots_below_anchor(anchor, primary_roots, &mut confined, &mut independent);
    if arguments.allowed_roots.is_empty() {
        partition_roots_below_anchor(
            anchor,
            allowed_roots.iter().cloned(),
            &mut confined,
            &mut independent,
        );
    } else {
        independent.extend(allowed_roots.iter().cloned());
    }
    if arguments.project_root.is_none() {
        partition_roots_below_anchor(
            anchor,
            project_root.map(Path::to_owned),
            &mut confined,
            &mut independent,
        );
    } else {
        independent.extend(project_root.map(Path::to_owned));
    }
    (confined, independent)
}

fn configuration_stylesheet_session(
    policy: adocweave_host::LocalTargetPolicy,
) -> adocweave_host::LocalTargetSession {
    let stylesheet = adocweave::output::html::StylesheetPolicy::default();
    let max_files = usize::try_from(stylesheet.max_sources).expect("u32 fits usize");
    let max_resource_bytes = u64::from(stylesheet.max_inline_bytes).saturating_add(1);
    adocweave_host::LocalTargetSession::new(
        policy,
        max_files,
        adocweave_host::FilesystemReadLimits {
            max_files,
            max_total_bytes: max_resource_bytes.saturating_mul(u64::from(stylesheet.max_sources)),
            max_resource_bytes,
        },
    )
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
    local: Option<(
        &std::path::Path,
        &std::path::Path,
        &str,
        &adocweave_host::LocalFilesystemSession,
    )>,
) -> Result<CheckOutcome, CliError> {
    let local = local
        .map(|(base, root, source_id, filesystem)| {
            let policy = filesystem
                .policy_for_path(root)
                .ok_or_else(|| {
                    CliError::Path(format!(
                        "local target root is outside its retained filesystem authority: {}",
                        root.display()
                    ))
                })?
                .derive_confined_directory(root)
                .map_err(CliError::LocalTarget)?;
            let limits = filesystem.limits();
            Ok(commands::check::LocalContext {
                base,
                source_id,
                session: adocweave_host::LocalTargetSession::new(policy, limits.max_files, limits),
            })
        })
        .transpose()?;
    commands::check::process(
        input,
        check,
        source_id,
        analysis_options,
        preprocess_options,
        local,
    )
    .map_err(check_error)
}

fn load_project_config_at(
    arguments: &Arguments,
    start: &std::path::Path,
    boundary_policy: &adocweave_host::LocalTargetPolicy,
) -> Result<Option<adocweave_config::ConfigSnapshot>, CliError> {
    if arguments.no_config {
        return Ok(None);
    }
    if let Some(path) = &arguments.config_path {
        return adocweave_config::ConfigSnapshot::load_with_preferred_policy(path, boundary_policy)
            .map(Some)
            .map_err(CliError::Config);
    }
    match adocweave_config::discover_and_load_with_policy(start, boundary_policy) {
        Ok(snapshot) => Ok(snapshot),
        Err(error) if error.code == adocweave_config::ConfigErrorCode::OutsideBoundary => Ok(None),
        Err(error) => Err(CliError::Config(error)),
    }
}

fn validate_project_config_authority(
    config: &adocweave_config::ResolvedProjectConfig,
    boundary_policy: &adocweave_host::LocalTargetPolicy,
    resources: bool,
    local_targets: bool,
    stylesheets: bool,
) -> Result<(), CliError> {
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
        if boundary_policy.normalize_candidate(path).is_err() {
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

struct CollectedInputPaths {
    files: Vec<PathBuf>,
    directory_selected: bool,
}

fn glob_scan_root(pattern: &str) -> Option<PathBuf> {
    let mut root = PathBuf::new();
    for component in Path::new(pattern).components() {
        let authored = component.as_os_str().to_string_lossy();
        if authored
            .chars()
            .any(|character| matches!(character, '*' | '?' | '['))
        {
            return Some(if root.as_os_str().is_empty() {
                PathBuf::from(".")
            } else {
                root
            });
        }
        root.push(component.as_os_str());
    }
    None
}

fn relative_path_from(base: &Path, target: &Path) -> PathBuf {
    let base = base.components().collect::<Vec<_>>();
    let target = target.components().collect::<Vec<_>>();
    let shared = base
        .iter()
        .zip(&target)
        .take_while(|(left, right)| left == right)
        .count();
    let mut relative = PathBuf::new();
    for _ in shared..base.len() {
        relative.push("..");
    }
    for component in &target[shared..] {
        relative.push(component.as_os_str());
    }
    relative
}

fn absolute_lexical_path(anchor: &Path, authored: &Path) -> PathBuf {
    let candidate = if authored.is_absolute() {
        authored.to_owned()
    } else {
        anchor.join(authored)
    };
    let mut normalized = PathBuf::new();
    for component in candidate.components() {
        match component {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                normalized.pop();
            }
            _ => normalized.push(component.as_os_str()),
        }
    }
    normalized
}

fn retained_input_candidate(
    authority: &mut adocweave_host::LocalFilesystemPolicy,
    anchor: &Path,
    authored: &Path,
) -> Result<(adocweave_host::LocalFilesystemAccess, PathBuf), CliError> {
    if authored == Path::new(".") {
        let access = authority
            .access_existing(
                [anchor.to_owned()],
                adocweave_host::FilesystemReadLimits::default(),
            )
            .map_err(local_include::LocalIncludeError::Host)
            .map_err(CliError::Include)?;
        let candidate = access
            .roots()
            .first()
            .cloned()
            .ok_or_else(|| CliError::Path("input authority has no root".to_owned()))?;
        return Ok((access, candidate));
    }
    let absolute = if authored.is_absolute() {
        authored.to_owned()
    } else {
        anchor.join(authored)
    };
    let Some(file_name) = absolute.file_name() else {
        let (mut confined, mut independent) = (Vec::new(), Vec::new());
        partition_roots_below_anchor(anchor, [absolute.clone()], &mut confined, &mut independent);
        let access = filesystem_access_from_authority(
            authority,
            anchor,
            confined,
            independent,
            adocweave_host::FilesystemReadLimits::default(),
        )?;
        let candidate = access.roots()[0].clone();
        return Ok((access, candidate));
    };
    let parent = absolute
        .parent()
        .ok_or_else(|| CliError::Path("input path has no parent directory".to_owned()))?;
    let explicitly_independent = authored.is_absolute() && !authored.starts_with(anchor)
        || authored
            .components()
            .any(|component| component == std::path::Component::ParentDir);
    let (confined, independent) = if explicitly_independent {
        (Vec::new(), vec![parent.to_owned()])
    } else {
        (vec![parent.to_owned()], Vec::new())
    };
    let access = filesystem_access_from_authority(
        authority,
        anchor,
        confined,
        independent,
        adocweave_host::FilesystemReadLimits::default(),
    )?;
    let policy_root = access
        .roots()
        .iter()
        .max_by_key(|root| root.components().count())
        .cloned()
        .ok_or_else(|| CliError::Path("input authority has no root".to_owned()))?;
    Ok((access, policy_root.join(file_name)))
}

fn collect_input_paths(
    arguments: &Arguments,
    authority: &mut adocweave_host::LocalFilesystemPolicy,
    anchor: &Path,
) -> Result<CollectedInputPaths, CliError> {
    let primary = arguments.input.clone();
    let mut pending = arguments
        .input
        .iter()
        .chain(&arguments.additional_inputs)
        .cloned()
        .map(|path| (path, true))
        .collect::<Vec<_>>();
    let mut patterns = Vec::new();
    for authored in &arguments.glob_patterns {
        let pattern = glob::Pattern::new(authored)
            .map_err(|error| CliError::Path(format!("invalid glob pattern {authored}: {error}")))?;
        let root = glob_scan_root(authored).unwrap_or_else(|| {
            Path::new(authored)
                .parent()
                .filter(|parent| !parent.as_os_str().is_empty())
                .map_or_else(|| PathBuf::from("."), Path::to_owned)
        });
        pending.push((root, false));
        patterns.push((pattern, Path::new(authored).is_absolute()));
    }
    let mut scanned_entries = pending.len();
    if scanned_entries > MAX_SCAN_ENTRIES {
        return Err(CliError::Path(
            "directory scan entry limit exceeded".to_owned(),
        ));
    }
    pending.sort();
    let mut files = std::collections::BTreeSet::new();
    let mut directories = std::collections::BTreeSet::new();
    let mut explicit_directories = std::collections::BTreeSet::new();
    let mut directory_selected = false;
    for (path, explicit) in pending {
        let is_primary = primary.as_ref() == Some(&path);
        let (access, candidate) = match retained_input_candidate(authority, anchor, &path) {
            Ok(candidate) => candidate,
            Err(CliError::Include(local_include::LocalIncludeError::Host(
                adocweave_host::ResourceError::Missing(_),
            ))) if !explicit => continue,
            Err(CliError::LocalTarget(adocweave_host::LocalTargetError::Missing(_)))
                if !explicit =>
            {
                continue;
            }
            Err(CliError::Include(local_include::LocalIncludeError::Host(
                adocweave_host::ResourceError::Missing(_),
            )))
            | Err(CliError::LocalTarget(adocweave_host::LocalTargetError::Missing(_))) => {
                return Err(CliError::Read {
                    source_name: path.display().to_string(),
                    source: std::io::Error::from(std::io::ErrorKind::NotFound),
                });
            }
            Err(error) => return Err(error),
        };
        let policy = access.policy_for_path(&candidate).ok_or_else(|| {
            CliError::Path(format!(
                "input is outside its retained filesystem authority: {}",
                path.display()
            ))
        })?;
        match policy.inspect_directory_no_symlinks(&candidate) {
            Ok(directory) => {
                let parent_root = policy.root().to_owned();
                directory_selected |= is_primary;
                let retained = filesystem_access_from_authority(
                    authority,
                    &parent_root,
                    vec![directory],
                    Vec::new(),
                    adocweave_host::FilesystemReadLimits::default(),
                )?;
                directories.extend(retained.roots().iter().cloned());
                if explicit {
                    explicit_directories.extend(retained.roots().iter().cloned());
                }
            }
            Err(adocweave_host::LocalTargetError::NotDirectory(_)) => {
                let file = policy
                    .inspect_candidate_no_symlinks(&candidate)
                    .map_err(CliError::LocalTarget)?;
                files.insert(file);
            }
            Err(adocweave_host::LocalTargetError::Missing(_)) if !explicit => continue,
            Err(adocweave_host::LocalTargetError::Missing(_)) => {
                return Err(CliError::Read {
                    source_name: path.display().to_string(),
                    source: std::io::Error::from(std::io::ErrorKind::NotFound),
                });
            }
            Err(error) => return Err(CliError::LocalTarget(error)),
        }
    }
    if !directories.is_empty() {
        let filesystem = authority
            .access_existing(directories, adocweave_host::FilesystemReadLimits::default())
            .and_then(|access| access.session())
            .map_err(local_include::LocalIncludeError::Host)
            .map_err(CliError::Include)?;
        for path in filesystem
            .discover_adoc_paths()
            .map_err(local_include::LocalIncludeError::Host)
            .map_err(CliError::Include)?
        {
            charge_scan_entry(&mut scanned_entries)?;
            let explicitly_selected = explicit_directories
                .iter()
                .any(|root| path.starts_with(root));
            let glob_selected = patterns.iter().any(|(pattern, absolute)| {
                if *absolute {
                    pattern.matches_path(&path)
                } else {
                    let relative = relative_path_from(anchor, &path);
                    pattern.matches_path(&relative)
                }
            });
            if explicitly_selected || glob_selected {
                files.insert(path);
            }
        }
    }
    Ok(CollectedInputPaths {
        files: files.into_iter().collect(),
        directory_selected,
    })
}

/// Names the scope one input belongs to.
///
/// The Language Server is told its roots by the editor. A command-line run is
/// not, so the root is taken from the project file's directory, or from the
/// input's own directory when no project file applies.
fn cli_project_scope(
    path: &Path,
    snapshot: Option<&adocweave_config::ConfigSnapshot>,
) -> ProjectScopeId {
    let workspace_root = snapshot
        .map(|snapshot| snapshot.path.as_path())
        .and_then(Path::parent)
        .or_else(|| path.parent())
        .unwrap_or_else(|| Path::new(""))
        .to_owned();
    ProjectScopeId::new(workspace_root, snapshot)
}

#[derive(Clone, Debug)]
struct ResolvedCliInput {
    scope: ProjectScopeId,
    config: adocweave_config::ResolvedProjectConfig,
}

fn resolve_input_path_scopes(
    arguments: &Arguments,
    paths: &[PathBuf],
    boundary_policy: &adocweave_host::LocalTargetPolicy,
) -> Result<std::collections::BTreeMap<PathBuf, ResolvedCliInput>, CliError> {
    resolve_input_path_scopes_with_hook(arguments, paths, boundary_policy, |_| {})
}

fn resolve_input_path_scopes_with_hook(
    arguments: &Arguments,
    paths: &[PathBuf],
    boundary_policy: &adocweave_host::LocalTargetPolicy,
    mut after_path: impl FnMut(usize),
) -> Result<std::collections::BTreeMap<PathBuf, ResolvedCliInput>, CliError> {
    let mut scopes = std::collections::BTreeMap::<
        ProjectScopeId,
        (usize, adocweave_config::ResolvedProjectConfig),
    >::new();
    let mut resolved = std::collections::BTreeMap::new();
    for (index, path) in paths.iter().enumerate() {
        let snapshot = load_project_config_at(arguments, path, boundary_policy)?;
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
    let boundary = env::current_dir().map_err(|source| CliError::Read {
        source_name: "current directory".to_owned(),
        source,
    })?;
    let mut filesystem_authority = filesystem_authority(boundary)?;
    let authority_root = filesystem_authority.roots()[0].clone();
    let cli_base_dir = arguments
        .base_dir
        .as_ref()
        .map(|path| absolute_lexical_path(&authority_root, path));
    let cli_allowed_roots = arguments
        .allowed_roots
        .iter()
        .map(|path| absolute_lexical_path(&authority_root, path))
        .collect::<Vec<_>>();
    let cli_project_root = arguments
        .project_root
        .as_ref()
        .map(|path| absolute_lexical_path(&authority_root, path));
    let collected = collect_input_paths(arguments, &mut filesystem_authority, &authority_root)?;
    let paths = collected.files;
    let directory_selected = collected.directory_selected;
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
    let boundary_policy = filesystem_authority
        .root_policy(&authority_root)
        .expect("the initial authority retains its root")
        .clone();
    let resolved_inputs = resolve_input_path_scopes(arguments, &paths, &boundary_policy)?;
    let mut project_filesystems =
        std::collections::BTreeMap::<ProjectScopeId, adocweave_host::LocalFilesystemSession>::new();
    let mut project_retained =
        std::collections::BTreeMap::<ProjectScopeId, ProjectRetainedBudget>::new();
    match &arguments.command {
        CommandOptions::Format(options) => {
            if !options.supports_multiple_inputs() {
                return Err(CliError::Usage(
                    "multiple format inputs require --check, --write, or --diff".to_owned(),
                ));
            }
            let mut workflow = commands::format::BatchWorkflow::new(*options, paths.len());
            let mut write_policies = std::collections::BTreeMap::new();
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
                        &boundary_policy,
                        arguments.allowed_roots.is_empty(),
                        false,
                        false,
                    )?;
                }
                let source_base = path.parent().expect("canonical input path has a parent");
                let base_dir = cli_base_dir.as_deref().unwrap_or(source_base);
                let allowed_roots = if arguments.allowed_roots.is_empty() {
                    &config.resources.roots
                } else {
                    &cli_allowed_roots
                };
                let project_key = resolved.scope.clone();
                let filesystem = match project_filesystems.entry(project_key.clone()) {
                    std::collections::btree_map::Entry::Occupied(entry) => entry.into_mut(),
                    std::collections::btree_map::Entry::Vacant(entry) => {
                        let (confined_roots, independent_roots) = processing_filesystem_roots(
                            &authority_root,
                            std::iter::once(authority_root.clone()).chain(
                                paths
                                    .iter()
                                    .filter(|path| {
                                        resolved_inputs
                                            .get(*path)
                                            .is_some_and(|resolved| resolved.scope == project_key)
                                    })
                                    .filter_map(|path| path.parent().map(Path::to_owned)),
                            ),
                            arguments,
                            allowed_roots,
                            None,
                        );
                        entry.insert(filesystem_from_authority(
                            &mut filesystem_authority,
                            &authority_root,
                            confined_roots,
                            independent_roots,
                            config.resources.limit_plan.filesystem_reads,
                        )?)
                    }
                };
                let original = read_primary_in_session(path, filesystem)?;
                write_policies.insert(path.clone(), retained_write_policy(path, filesystem)?);
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
                        .map(|write| {
                            let policy = write_policies
                                .remove(&write.path)
                                .expect("each pending write retains its input authority");
                            PendingWrite {
                                path: write.path,
                                original: write.original,
                                replacement: write.replacement,
                                policy,
                            }
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
                let project_root = cli_project_root.clone().or_else(|| {
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
                    &boundary_policy,
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
                    &cli_allowed_roots
                };
                let project_key = resolved.scope.clone();
                let filesystem = match project_filesystems.entry(project_key.clone()) {
                    std::collections::btree_map::Entry::Occupied(entry) => entry.into_mut(),
                    std::collections::btree_map::Entry::Vacant(entry) => {
                        let (confined_roots, independent_roots) = processing_filesystem_roots(
                            &authority_root,
                            std::iter::once(authority_root.clone()).chain(
                                paths
                                    .iter()
                                    .filter(|path| {
                                        resolved_inputs
                                            .get(*path)
                                            .is_some_and(|resolved| resolved.scope == project_key)
                                    })
                                    .filter_map(|path| path.parent().map(Path::to_owned)),
                            ),
                            arguments,
                            allowed_roots,
                            project_root.as_deref(),
                        );
                        entry.insert(filesystem_from_authority(
                            &mut filesystem_authority,
                            &authority_root,
                            confined_roots,
                            independent_roots,
                            config.resources.limit_plan.filesystem_reads,
                        )?)
                    }
                };
                let original = read_primary_in_session(path, filesystem)?;
                let write_policy = retained_write_policy(path, filesystem)?;
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
                            policy: write_policy,
                        });
                    }
                }
                let local_context = project_root
                    .as_ref()
                    .map(|root| (source_base.as_path(), root.as_path(), source_id.as_ref()));
                let outcome = if include {
                    let source = decode_input(&checked)?;
                    let base_dir = cli_base_dir.as_deref().unwrap_or(source_base.as_path());
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
                        local_context
                            .map(|(base, root, source_id)| (base, root, source_id, &*filesystem)),
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
            let boundary = env::current_dir().map_err(|source| CliError::Read {
                source_name: "current directory".to_owned(),
                source,
            })?;
            let mut filesystem_authority = filesystem_authority(boundary)?;
            let authority_root = filesystem_authority.roots()[0].clone();
            let cli_base_dir = arguments
                .base_dir
                .as_ref()
                .map(|path| absolute_lexical_path(&authority_root, path));
            let cli_allowed_roots = arguments
                .allowed_roots
                .iter()
                .map(|path| absolute_lexical_path(&authority_root, path))
                .collect::<Vec<_>>();
            let cli_project_root = arguments
                .project_root
                .as_ref()
                .map(|path| absolute_lexical_path(&authority_root, path));
            let boundary_policy = filesystem_authority
                .root_policy(&authority_root)
                .expect("the initial authority retains its root")
                .clone();
            let input_path = arguments.input.clone();
            let canonical_input = input_path
                .as_ref()
                .map(|path| resolve_primary_path(path, &boundary_policy));
            let config_start = canonical_input.as_deref().unwrap_or(&authority_root);
            let config_snapshot =
                load_project_config_at(&arguments, config_start, &boundary_policy)?;
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
                cli_allowed_roots
            };
            let project_root = cli_project_root.or_else(|| {
                project_config
                    .local_targets
                    .enabled
                    .then(|| project_config.local_targets.project_root.clone())
                    .flatten()
            });
            validate_project_config_authority(
                &project_config,
                &boundary_policy,
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
                let (confined_roots, independent_roots) = processing_filesystem_roots(
                    &authority_root,
                    [canonical_input
                        .as_deref()
                        .and_then(Path::parent)
                        .expect("preview input has a parent")
                        .to_owned()],
                    &arguments,
                    &allowed_roots,
                    project_root.as_deref(),
                );
                let preview_filesystem_access = filesystem_access_from_authority(
                    &mut filesystem_authority,
                    &authority_root,
                    confined_roots,
                    independent_roots,
                    project_config.resources.limit_plan.filesystem_reads,
                )?;
                PREVIEW_SHUTDOWN.store(false, std::sync::atomic::Ordering::Release);
                install_preview_signal_handlers();
                commands::preview::run(
                    commands::preview::RunRequest {
                        input_path,
                        include,
                        base_dir: cli_base_dir.as_deref(),
                        allowed_roots: &allowed_roots,
                        project_root: project_root.as_deref(),
                        project: &project_config,
                        css,
                        configuration_policy: boundary_policy.clone(),
                        filesystem_access: preview_filesystem_access,
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
            let explicit_stylesheet_authorities = match &arguments.command {
                CommandOptions::Convert { css, .. } => Some(
                    commands::html_policy::ExplicitStylesheetAuthorities::new(
                        &project_config.html,
                        css,
                    )
                    .map_err(cli_error::html_policy_error)?,
                ),
                _ => None,
            };
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
                let (confined_roots, independent_roots) = processing_filesystem_roots(
                    &authority_root,
                    [primary_base
                        .clone()
                        .expect("canonical input path has a parent")],
                    &arguments,
                    &allowed_roots,
                    project_root.as_deref(),
                );
                let mut filesystem = filesystem_from_authority(
                    &mut filesystem_authority,
                    &authority_root,
                    confined_roots,
                    independent_roots,
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
            if primary_filesystem.is_none()
                && matches!(arguments.command, CommandOptions::Check(_))
                && let Some(project_root) = project_root.as_deref()
            {
                let base = local_context
                    .as_ref()
                    .map(|(base, _, _)| base.clone())
                    .unwrap_or_else(|| project_root.to_owned());
                let (confined_roots, independent_roots) = processing_filesystem_roots(
                    &authority_root,
                    [base],
                    &arguments,
                    &allowed_roots,
                    Some(project_root),
                );
                primary_filesystem = Some(filesystem_from_authority(
                    &mut filesystem_authority,
                    &authority_root,
                    confined_roots,
                    independent_roots,
                    project_config.resources.limit_plan.filesystem_reads,
                )?);
            }
            validate_resource_plan([input.len() as u64], project_config.resources.limit_plan)?;
            let mut retained_resources = ProjectRetainedBudget::default();
            let mut prepared = None;
            let processed = if include {
                let source = decode_input(&input)?;
                let base_dir = match cli_base_dir.clone() {
                    Some(base_dir) => base_dir,
                    None => primary_base.clone().ok_or_else(|| {
                        CliError::Usage(
                            "--include with standard input requires --base-dir".to_owned(),
                        )
                    })?,
                };
                let source_id = input_path.as_ref().map_or_else(
                    || "<stdin>".to_owned(),
                    |path| path.to_string_lossy().into_owned(),
                );
                if primary_filesystem.is_none() {
                    let (confined_roots, independent_roots) = processing_filesystem_roots(
                        &authority_root,
                        [base_dir.clone()],
                        &arguments,
                        &allowed_roots,
                        project_root.as_deref(),
                    );
                    primary_filesystem = Some(filesystem_from_authority(
                        &mut filesystem_authority,
                        &authority_root,
                        confined_roots,
                        independent_roots,
                        include_limits_after_root(
                            project_config.resources.limit_plan,
                            input.len(),
                        )?,
                    )?);
                }
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
                    limits: primary_filesystem
                        .as_ref()
                        .expect("include processing has a filesystem session")
                        .limits(),
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
                        local_context.as_ref().and_then(|(base, root, source_id)| {
                            primary_filesystem.as_ref().map(|filesystem| {
                                (
                                    base.as_path(),
                                    root.as_path(),
                                    source_id.as_str(),
                                    filesystem,
                                )
                            })
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
                let mut configuration_stylesheets =
                    configuration_stylesheet_session(boundary_policy.clone());
                let output = match &arguments.command {
                    CommandOptions::Convert { complete, css } => commands::convert::run(
                        &processed,
                        &project_config.analysis,
                        &project_config.html,
                        *complete,
                        css,
                        |origin, path| match origin {
                            commands::html_policy::StylesheetFileOrigin::ProjectConfiguration => {
                                configuration_stylesheets
                                    .read_candidate_bytes(path)
                                    .map(|loaded| loaded.into_parts().1)
                                    .map_err(io::Error::other)
                            }
                            commands::html_policy::StylesheetFileOrigin::CommandLine => {
                                explicit_stylesheet_authorities
                                    .as_ref()
                                    .expect("convert retains command-line stylesheet authorities")
                                    .read_authored(path)
                                    .map(|(_, bytes)| bytes)
                            }
                        },
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
        Action, CliError, CommandOptions, CompletionShell, DEFAULT_PREVIEW_DEBOUNCE_MS,
        DEFAULT_PREVIEW_PORT, DiagnosticFormat, FormatOptions, MAX_SCAN_ENTRIES,
        ProjectRetainedBudget, charge_scan_entry, cli_project_scope,
        configuration_stylesheet_session, filesystem_authority, filesystem_from_authority,
        load_project_config_at, parse_arguments, read_primary_in_session, render_completion_script,
        resolve_input_path_scopes_with_hook, validate_project_config_authority,
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
        let policy = adocweave_host::LocalTargetPolicy::new(directory.path())
            .expect("configuration boundary");
        let error = resolve_input_path_scopes_with_hook(
            &arguments,
            &[first.clone(), second.clone()],
            &policy,
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

    #[cfg(target_os = "linux")]
    #[test]
    fn configuration_and_primary_input_share_one_filesystem_authority() {
        let directory = tempfile::tempdir().expect("temporary project parent");
        let root = directory.path().join("workspace");
        std::fs::create_dir(&root).expect("trusted workspace");
        let document = root.join("document.adoc");
        std::fs::write(&document, "trusted\n").expect("trusted document");
        std::fs::write(root.join("style.css"), "trusted-style").expect("trusted stylesheet");
        std::fs::write(
            root.join(adocweave_config::FILE_NAME),
            "schema-version = 1\n[resources]\nroots = [\".\"]\n[html]\ncomplete = true\nstylesheet-files = [\"style.css\"]\n",
        )
        .expect("trusted configuration");
        let Action::Run(arguments) = parse_arguments(
            [
                "format".to_owned(),
                "--check".to_owned(),
                document.to_string_lossy().into_owned(),
            ]
            .into_iter(),
        )
        .expect("arguments") else {
            panic!("expected run action");
        };
        let mut authority = filesystem_authority(root.clone()).expect("filesystem authority");
        let anchor = authority.roots()[0].clone();
        let boundary_policy = authority
            .root_policy(&anchor)
            .expect("boundary policy")
            .clone();

        let moved = directory.path().join("trusted-workspace");
        std::fs::rename(&root, &moved).expect("move trusted workspace");
        std::fs::create_dir(&root).expect("replacement workspace");
        std::fs::write(root.join(adocweave_config::FILE_NAME), "invalid")
            .expect("replacement configuration");
        std::fs::write(&document, "replacement\n").expect("replacement document");
        std::fs::write(root.join("style.css"), "replacement-style")
            .expect("replacement stylesheet");

        let snapshot = load_project_config_at(&arguments, &document, &boundary_policy)
            .expect("configuration lookup")
            .expect("trusted configuration snapshot");
        assert_eq!(snapshot.path, root.join(adocweave_config::FILE_NAME));
        let mut confined_roots = vec![root.clone()];
        confined_roots.extend(snapshot.config.resources.roots.iter().cloned());
        let mut filesystem = filesystem_from_authority(
            &mut authority,
            &anchor,
            confined_roots,
            Vec::new(),
            snapshot.config.resources.limit_plan.filesystem_reads,
        )
        .expect("filesystem session");

        let input = read_primary_in_session(&document, &mut filesystem).expect("primary input");
        assert_eq!(input, b"trusted\n");
        let mut stylesheets = configuration_stylesheet_session(boundary_policy);
        let stylesheet = stylesheets
            .read_candidate_bytes(&root.join("style.css"))
            .expect("configured stylesheet");
        assert_eq!(stylesheet.source(), b"trusted-style");
    }

    #[cfg(unix)]
    #[test]
    fn external_explicit_config_cannot_authorize_its_stylesheet() {
        use std::os::unix::fs::symlink;

        let workspace = tempfile::tempdir().expect("workspace");
        let external = tempfile::tempdir().expect("external configuration");
        let target = external.path().join("project.toml");
        std::fs::write(
            &target,
            "schema-version = 1\n[html]\ncomplete = true\nstylesheet-files = [\"style.css\"]\n",
        )
        .expect("external configuration");
        std::fs::write(external.path().join("style.css"), "external-style")
            .expect("external stylesheet");
        let selected = workspace.path().join("selected.toml");
        symlink(&target, &selected).expect("explicit configuration symlink");
        let Action::Run(arguments) = parse_arguments(
            [
                "config".to_owned(),
                "show".to_owned(),
                "--config".to_owned(),
                selected.to_string_lossy().into_owned(),
            ]
            .into_iter(),
        )
        .expect("arguments") else {
            panic!("expected run action");
        };
        let authority = filesystem_authority(workspace.path().to_owned()).expect("authority");
        let boundary = authority.roots()[0].clone();
        let boundary_policy = authority.root_policy(&boundary).expect("boundary policy");

        let snapshot = load_project_config_at(&arguments, workspace.path(), boundary_policy)
            .expect("explicit configuration")
            .expect("configuration snapshot");

        assert_eq!(snapshot.path, target);
        assert!(matches!(
            validate_project_config_authority(
                &snapshot.config,
                boundary_policy,
                false,
                false,
                true,
            ),
            Err(CliError::ConfigAuthority(path))
                if path == external.path().join("style.css")
        ));
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
