use std::collections::BTreeMap;
use std::io;
use std::net::IpAddr;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::time::Duration;

use adocweave::output::diagnostics as diagnostic;
use adocweave::{CancellationCheck, CancellationToken, Engine, ParseError};

use super::html_policy::{self, StylesheetArgument};
use crate::{local_include, preview};

#[derive(Debug)]
pub(crate) enum Error {
    Read {
        source_name: String,
        source: io::Error,
    },
    Analysis(ParseError),
    Include(local_include::LocalIncludeError),
    Html(html_policy::Error),
    Path(String),
    Server(preview::Error),
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct ServerOptions {
    pub(crate) bind: IpAddr,
    pub(crate) port: u16,
    pub(crate) debounce_ms: u64,
}

pub(crate) struct RunRequest<'request> {
    pub(crate) input_path: &'request Path,
    pub(crate) include: bool,
    pub(crate) base_dir: Option<&'request Path>,
    pub(crate) allowed_roots: &'request [PathBuf],
    pub(crate) project_root: Option<&'request Path>,
    pub(crate) project: &'request adocweave_config::ResolvedProjectConfig,
    pub(crate) css: &'request [StylesheetArgument],
    pub(crate) server: ServerOptions,
}

struct BuildRequest<'request> {
    input_path: &'request Path,
    include: bool,
    base_dir: &'request Path,
    project_root: &'request Path,
    project: &'request adocweave_config::ResolvedProjectConfig,
    css: &'request [StylesheetArgument],
}

struct DependencyObserver<'dependencies> {
    dependencies: &'dependencies mut BTreeMap<PathBuf, preview::Fingerprint>,
}

impl local_include::DependencyObserver for DependencyObserver<'_> {
    fn observe_path(&mut self, path: &Path) {
        self.dependencies
            .entry(path.to_owned())
            .or_insert_with(|| preview::Fingerprint::read(path));
    }

    fn observe_loaded(&mut self, path: &Path, source: &str) {
        self.dependencies.insert(
            path.to_owned(),
            preview::Fingerprint::from_loaded_bytes(path, source.as_bytes()),
        );
    }
}

pub(crate) fn run(request: RunRequest<'_>, shutdown: &AtomicBool) -> Result<(), Error> {
    let metadata = std::fs::symlink_metadata(request.input_path).map_err(|source| Error::Read {
        source_name: request.input_path.display().to_string(),
        source,
    })?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(Error::Path(format!(
            "preview input must be a regular, non-symlink file: {}",
            request.input_path.display()
        )));
    }
    let canonical_input = request
        .input_path
        .canonicalize()
        .map_err(|source| Error::Read {
            source_name: request.input_path.display().to_string(),
            source,
        })?;
    let base_dir = request
        .base_dir
        .map(PathBuf::from)
        .or_else(|| canonical_input.parent().map(PathBuf::from))
        .expect("a file has a parent");
    let configured_root = request.include.then(|| {
        request.allowed_roots.iter().find_map(|root| {
            root.canonicalize()
                .ok()
                .filter(|root| canonical_input.starts_with(root))
        })
    });
    let preview_root = request
        .project_root
        .map(PathBuf::from)
        .or(configured_root.flatten())
        .unwrap_or_else(|| base_dir.clone())
        .canonicalize()
        .map_err(|source| Error::Read {
            source_name: "preview project root".to_owned(),
            source,
        })?;
    if !canonical_input.starts_with(&preview_root) {
        return Err(Error::Path(format!(
            "preview input is outside the project root: {}",
            canonical_input.display()
        )));
    }
    if !request.server.bind.is_loopback() {
        eprintln!(
            "warning: preview is exposed on non-loopback address {}; rendered content may be visible to other hosts",
            request.server.bind
        );
    }
    preview::run(
        preview::Options {
            bind: request.server.bind,
            port: request.server.port,
            debounce: Duration::from_millis(request.server.debounce_ms),
        },
        |cancellation| {
            let mut dependencies = BTreeMap::new();
            let result = build(
                BuildRequest {
                    input_path: &canonical_input,
                    include: request.include,
                    base_dir: &base_dir,
                    project_root: &preview_root,
                    project: request.project,
                    css: request.css,
                },
                cancellation,
                &mut dependencies,
            );
            match result {
                Ok(build) => Ok(build),
                Err(error) => {
                    let paths = std::iter::once(canonical_input.clone())
                        .chain(request.project.html.stylesheet_files.iter().cloned())
                        .chain(request.css.iter().filter_map(|argument| match argument {
                            StylesheetArgument::File(path) => Some(path.clone()),
                            StylesheetArgument::Url(_) => None,
                        }));
                    dependencies.extend(paths.map(|path| {
                        let fingerprint = preview::Fingerprint::read(&path);
                        (path, fingerprint)
                    }));
                    Ok(preview::Build::failure(error.to_string(), dependencies))
                }
            }
        },
        shutdown,
    )
    .map_err(Error::Server)
}

fn build(
    request: BuildRequest<'_>,
    cancellation: &CancellationToken,
    dependencies: &mut BTreeMap<PathBuf, preview::Fingerprint>,
) -> Result<preview::Build, Error> {
    build_with_stage_hook(request, cancellation, dependencies, |_| {})
}

#[derive(Clone, Copy)]
enum BuildStage {
    IncludesPrepared,
}

fn build_with_stage_hook(
    request: BuildRequest<'_>,
    cancellation: &CancellationToken,
    dependencies: &mut BTreeMap<PathBuf, preview::Fingerprint>,
    mut stage_hook: impl FnMut(BuildStage),
) -> Result<preview::Build, Error> {
    ensure_active(cancellation)?;
    let plan = request.project.resources.limit_plan;
    let policy = adocweave_host::LocalFilesystemPolicy::new(
        [request.project_root.to_owned()],
        plan.filesystem_reads,
    )
    .map_err(local_include::LocalIncludeError::Host)
    .map_err(Error::Include)?;
    let mut filesystem = policy
        .session()
        .map_err(local_include::LocalIncludeError::Host)
        .map_err(Error::Include)?;
    let loaded = filesystem
        .read_utf8(
            adocweave_host::LogicalSourceId::new(request.input_path.to_string_lossy())
                .map_err(local_include::LocalIncludeError::Host)
                .map_err(Error::Include)?,
            request.input_path,
        )
        .map_err(local_include::LocalIncludeError::Host)
        .map_err(Error::Include)?;
    let (_, input) = loaded.into_parts();
    let input_fingerprint =
        preview::Fingerprint::from_loaded_bytes(request.input_path, input.as_bytes());
    ensure_active(cancellation)?;
    let source = input.as_ref();
    ensure_active(cancellation)?;
    let source_id = request.input_path.to_string_lossy().into_owned();
    dependencies.insert(request.input_path.to_owned(), input_fingerprint);

    let (processed, include_diagnostics) = if request.include {
        ensure_active(cancellation)?;
        let prepared = {
            let mut observer = DependencyObserver { dependencies };
            local_include::prepare_local_tracking_with_existing_session(
                source,
                source_id,
                request.base_dir,
                request.base_dir,
                request.project_root,
                &request.project.preprocess,
                &mut observer,
                &mut filesystem,
            )
        }
        .map_err(Error::Include)?;
        crate::validate_resource_plan(prepared.resource_sizes(), plan)
            .map_err(|error| Error::Path(error.to_string()))?;
        stage_hook(BuildStage::IncludesPrepared);
        ensure_active(cancellation)?;
        let include_diagnostics = prepared
            .validation()
            .expect("local preparation has validation context")
            .include_errors()
            .iter()
            .map(|(target, error)| {
                serde_json::json!({
                    "code": error.diagnostic_code(),
                    "message": error.to_string(),
                    "target": target,
                })
            })
            .collect::<Vec<_>>();
        (
            prepared.projection().document().source.to_string(),
            include_diagnostics,
        )
    } else {
        crate::validate_resource_plan([input.len() as u64], plan)
            .map_err(|error| Error::Path(error.to_string()))?;
        (source.to_owned(), Vec::new())
    };
    ensure_active(cancellation)?;
    let analysis = Engine::new(request.project.analysis.clone())
        .analyze_with(
            &processed,
            adocweave::AnalysisInputs {
                cancellation: Some(cancellation),
                ..adocweave::AnalysisInputs::default()
            },
        )
        .map_err(Error::Analysis)?;
    ensure_active(cancellation)?;
    let render_policy = html_policy::build(
        &request.project.html,
        true,
        request.css,
        |path| {
            let (bytes, fingerprint) = preview::read_dependency(path)?;
            dependencies.insert(path.to_owned(), fingerprint);
            Ok(bytes)
        },
        || cancellation.is_cancelled(),
    )
    .map_err(Error::Html)?;
    ensure_active(cancellation)?;
    let output =
        html_policy::render_checked(analysis.document(), &render_policy).map_err(Error::Html)?;
    ensure_active(cancellation)?;
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
    let style_origins = html_policy::external_origins(&render_policy);
    Ok(preview::Build::new(
        output.html,
        serde_json::to_string(&diagnostics).expect("diagnostics are serializable"),
        dependencies.clone(),
    )
    .with_style_origins(style_origins))
}

fn ensure_active(cancellation: &CancellationToken) -> Result<(), Error> {
    if cancellation.is_cancelled() {
        Err(Error::Analysis(ParseError::Cancelled))
    } else {
        Ok(())
    }
}

impl std::fmt::Display for Error {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Read {
                source_name,
                source,
            } => write!(formatter, "could not read {source_name}: {source}"),
            Self::Analysis(source) => source.fmt(formatter),
            Self::Include(source) => source.fmt(formatter),
            Self::Html(source) => match source {
                html_policy::Error::Cancelled => ParseError::Cancelled.fmt(formatter),
                html_policy::Error::InvalidUtf8 { valid_up_to } => write!(
                    formatter,
                    "input is not valid UTF-8 (invalid byte starts at offset {valid_up_to})"
                ),
                html_policy::Error::Read {
                    source_name,
                    source,
                } => write!(formatter, "could not read {source_name}: {source}"),
                html_policy::Error::Stylesheet(message) | html_policy::Error::Usage(message) => {
                    formatter.write_str(message)
                }
            },
            Self::Path(message) => formatter.write_str(message),
            Self::Server(source) => source.fmt(formatter),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::local_include::DependencyObserver as _;

    #[test]
    fn failed_build_retains_discovered_include_dependencies() {
        let root = tempfile::tempdir().expect("temporary directory");
        let input = root.path().join("manual.adoc");
        let stylesheet = root.path().join("missing.css");
        let include = root.path().join("chapter.adoc");
        std::fs::write(&input, "include::chapter.adoc[]\n").expect("root document");
        std::fs::write(&include, "chapter\n").expect("included document");
        std::fs::write(&stylesheet, "</style").expect("invalid stylesheet");
        let mut dependencies = BTreeMap::new();

        let result = build(
            BuildRequest {
                input_path: &input,
                include: true,
                base_dir: root.path(),
                project_root: root.path(),
                project: &adocweave_config::ResolvedProjectConfig::default(),
                css: &[StylesheetArgument::File(stylesheet.clone())],
            },
            &CancellationToken::new(),
            &mut dependencies,
        );

        assert!(result.is_err());
        assert!(dependencies.contains_key(&input));
        assert!(dependencies.contains_key(&include));
        assert!(dependencies.contains_key(&stylesheet));
    }

    #[test]
    fn preprocess_failure_retains_dependencies_discovered_before_the_error() {
        let root = tempfile::tempdir().expect("temporary directory");
        let input = root.path().join("manual.adoc");
        let include = root.path().join("chapter.adoc");
        std::fs::write(&input, "include::chapter.adoc[]\n").expect("root document");
        std::fs::write(&include, "include::chapter.adoc[]\n").expect("cyclic include");
        let mut dependencies = BTreeMap::new();

        let result = build(
            BuildRequest {
                input_path: &input,
                include: true,
                base_dir: root.path(),
                project_root: root.path(),
                project: &adocweave_config::ResolvedProjectConfig::default(),
                css: &[],
            },
            &CancellationToken::new(),
            &mut dependencies,
        );

        assert!(result.is_err());
        assert!(dependencies.contains_key(&include));
    }

    #[test]
    fn observer_records_the_loaded_snapshot() {
        let root = tempfile::tempdir().expect("temporary directory");
        let include = root.path().join("chapter.adoc");
        std::fs::write(&include, "later snapshot\n").expect("included document");
        let mut dependencies = BTreeMap::new();

        DependencyObserver {
            dependencies: &mut dependencies,
        }
        .observe_loaded(&include, "first snapshot\n");

        let observed = dependencies.get(&include).expect("observed dependency");
        assert_eq!(
            observed,
            &preview::Fingerprint::from_loaded_bytes(&include, b"first snapshot\n")
        );
        assert_ne!(observed, &preview::Fingerprint::read(&include));
    }

    #[test]
    fn cancellation_is_checked_at_build_stage_boundaries() {
        let cancellation = CancellationToken::new();
        assert!(ensure_active(&cancellation).is_ok());
        cancellation.cancel();
        assert!(matches!(
            ensure_active(&cancellation),
            Err(Error::Analysis(ParseError::Cancelled))
        ));
    }

    #[test]
    fn cancelled_build_retains_loaded_include_snapshot() {
        let root = tempfile::tempdir().expect("temporary directory");
        let input = root.path().join("manual.adoc");
        let include = root.path().join("chapter.adoc");
        std::fs::write(&input, "include::chapter.adoc[]\n").expect("root document");
        std::fs::write(&include, "first snapshot\n").expect("included document");
        let cancellation = CancellationToken::new();
        let mut dependencies = BTreeMap::new();

        let result = build_with_stage_hook(
            BuildRequest {
                input_path: &input,
                include: true,
                base_dir: root.path(),
                project_root: root.path(),
                project: &adocweave_config::ResolvedProjectConfig::default(),
                css: &[],
            },
            &cancellation,
            &mut dependencies,
            |stage| {
                if matches!(stage, BuildStage::IncludesPrepared) {
                    cancellation.cancel();
                }
            },
        );

        assert!(matches!(
            result,
            Err(Error::Analysis(ParseError::Cancelled))
        ));
        assert_eq!(
            dependencies.get(&include),
            Some(&preview::Fingerprint::from_loaded_bytes(
                &include,
                b"first snapshot\n"
            ))
        );
    }

    #[test]
    fn cancelled_build_retains_missing_include_candidate() {
        let root = tempfile::tempdir().expect("temporary directory");
        let input = root.path().join("manual.adoc");
        let missing = root.path().join("missing.adoc");
        std::fs::write(&input, "include::missing.adoc[]\n").expect("root document");
        let cancellation = CancellationToken::new();
        let mut dependencies = BTreeMap::new();

        let result = build_with_stage_hook(
            BuildRequest {
                input_path: &input,
                include: true,
                base_dir: root.path(),
                project_root: root.path(),
                project: &adocweave_config::ResolvedProjectConfig::default(),
                css: &[],
            },
            &cancellation,
            &mut dependencies,
            |stage| {
                if matches!(stage, BuildStage::IncludesPrepared) {
                    cancellation.cancel();
                }
            },
        );

        assert!(matches!(
            result,
            Err(Error::Analysis(ParseError::Cancelled))
        ));
        assert_eq!(
            dependencies.get(&missing),
            Some(&preview::Fingerprint::read(&missing))
        );
    }
}
