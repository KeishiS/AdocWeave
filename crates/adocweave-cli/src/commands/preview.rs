use std::collections::BTreeMap;
use std::io;
use std::net::IpAddr;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::time::Duration;

use adocweave::output::diagnostics as diagnostic;
use adocweave::{CancellationCheck, CancellationToken, Engine, ParseError};

use super::html_policy::{self, StylesheetArgument, StylesheetFileOrigin};
use crate::{local_include, preview};

#[derive(Debug)]
pub(crate) enum Error {
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
    pub(crate) configuration_policy: adocweave_host::LocalTargetPolicy,
    pub(crate) filesystem_access: adocweave_host::LocalFilesystemPolicy,
    pub(crate) server: ServerOptions,
}

struct BuildRequest<'request> {
    input_path: &'request Path,
    include: bool,
    base_dir: &'request Path,
    project_root: &'request Path,
    project: &'request adocweave_config::ResolvedProjectConfig,
    css: &'request [StylesheetArgument],
    authorities: &'request PreviewAuthorities,
}

#[derive(Clone)]
struct PreviewAuthorities {
    configuration_policy: adocweave_host::LocalTargetPolicy,
    filesystem_access: adocweave_host::LocalFilesystemPolicy,
    explicit_stylesheets: html_policy::ExplicitStylesheetAuthorities,
}

impl PreviewAuthorities {
    fn new(request: &RunRequest<'_>) -> Result<Self, Error> {
        let explicit_stylesheets =
            html_policy::ExplicitStylesheetAuthorities::new(&request.project.html, request.css)
                .map_err(Error::Html)?;
        Ok(Self {
            configuration_policy: request.configuration_policy.clone(),
            filesystem_access: request.filesystem_access.clone(),
            explicit_stylesheets,
        })
    }

    fn workspace_policy_for(
        &self,
        candidate: &Path,
    ) -> Result<&adocweave_host::LocalTargetPolicy, Error> {
        self.filesystem_access
            .policy_for_path(candidate)
            .ok_or_else(|| {
                Error::Path(format!(
                    "preview path is outside its filesystem authority: {}",
                    candidate.display()
                ))
            })
    }

    fn absolute_workspace_candidate(&self, path: &Path) -> PathBuf {
        if path.is_absolute() {
            path.to_owned()
        } else {
            self.configuration_policy.root().join(path)
        }
    }

    fn input(&self, path: &Path) -> Result<PathBuf, Error> {
        let candidate = self.absolute_workspace_candidate(path);
        self.workspace_policy_for(&candidate)?
            .inspect_candidate_no_symlinks(&candidate)
            .map_err(|error| Error::Path(error.to_string()))
    }

    fn directory(&self, path: &Path) -> Result<PathBuf, Error> {
        let candidate = self.absolute_workspace_candidate(path);
        self.workspace_policy_for(&candidate)?
            .inspect_directory_no_symlinks(&candidate)
            .map_err(|error| Error::Path(error.to_string()))
    }

    fn snapshot(
        &self,
        dependencies: &[preview::Dependency],
    ) -> BTreeMap<preview::Dependency, preview::Fingerprint> {
        let mut workspace = self.filesystem_access.session();
        let mut configuration =
            crate::configuration_stylesheet_session(self.configuration_policy.clone());
        dependencies
            .iter()
            .cloned()
            .map(|dependency| {
                let fingerprint = match dependency.authority() {
                    preview::DependencyAuthority::Workspace => workspace
                        .as_mut()
                        .map_err(|error| error.to_string())
                        .and_then(|session| {
                            session
                                .read_utf8(
                                    adocweave_host::LogicalSourceId::new(
                                        dependency.path().to_string_lossy(),
                                    )
                                    .map_err(|error| error.to_string())?,
                                    dependency.path(),
                                )
                                .map(|loaded| {
                                    preview::Fingerprint::from_loaded_bytes(
                                        loaded.source().as_bytes(),
                                    )
                                })
                                .map_err(|error| error.to_string())
                        }),
                    preview::DependencyAuthority::Configuration => configuration
                        .read_candidate_bytes(dependency.path())
                        .map(|loaded| preview::Fingerprint::from_loaded_bytes(loaded.source()))
                        .map_err(|error| error.to_string()),
                    preview::DependencyAuthority::ExplicitStylesheet => self
                        .explicit_stylesheets
                        .read_candidate(dependency.path())
                        .map(|(_, bytes)| preview::Fingerprint::from_loaded_bytes(&bytes))
                        .map_err(|error| error.to_string()),
                }
                .unwrap_or_else(|error| preview::Fingerprint::unavailable(&error));
                (dependency, fingerprint)
            })
            .collect()
    }

    fn read_explicit_stylesheet(
        &self,
        authored: &Path,
    ) -> io::Result<(preview::Dependency, Vec<u8>, preview::Fingerprint)> {
        let (path, bytes) = self.explicit_stylesheets.read_authored(authored)?;
        let dependency = preview::Dependency::explicit_stylesheet(path);
        let fingerprint = preview::Fingerprint::from_loaded_bytes(&bytes);
        Ok((dependency, bytes, fingerprint))
    }
}

struct DependencyObserver<'dependencies, 'authorities> {
    dependencies: &'dependencies mut BTreeMap<preview::Dependency, preview::Fingerprint>,
    authorities: &'authorities PreviewAuthorities,
}

impl local_include::DependencyObserver for DependencyObserver<'_, '_> {
    fn observe_path(&mut self, path: &Path) {
        let dependency = preview::Dependency::workspace(path);
        if !self.dependencies.contains_key(&dependency) {
            let fingerprint = self
                .authorities
                .snapshot(std::slice::from_ref(&dependency))
                .remove(&dependency)
                .unwrap_or_else(|| preview::Fingerprint::unavailable("snapshot-missing"));
            self.dependencies.insert(dependency, fingerprint);
        }
    }

    fn observe_loaded(&mut self, path: &Path, source: &str) {
        self.dependencies.insert(
            preview::Dependency::workspace(path),
            preview::Fingerprint::from_loaded_bytes(source.as_bytes()),
        );
    }
}

pub(crate) fn run(request: RunRequest<'_>, shutdown: &AtomicBool) -> Result<(), Error> {
    let authorities = PreviewAuthorities::new(&request)?;
    let canonical_input = authorities.input(request.input_path)?;
    let base_dir = request
        .base_dir
        .map(|base| authorities.directory(base))
        .transpose()?
        .or_else(|| canonical_input.parent().map(PathBuf::from))
        .expect("a file has a parent");
    let configured_root = request.include.then(|| {
        request.allowed_roots.iter().find_map(|root| {
            authorities
                .directory(root)
                .ok()
                .filter(|root| canonical_input.starts_with(root))
        })
    });
    let preview_root = request
        .project_root
        .map(|root| authorities.directory(root))
        .transpose()?
        .or(configured_root.flatten())
        .unwrap_or_else(|| base_dir.clone());
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
    let build_authorities = authorities.clone();
    let snapshot_authorities = authorities;
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
                    authorities: &build_authorities,
                },
                cancellation,
                &mut dependencies,
            );
            match result {
                Ok(build) => Ok(build),
                Err(error) => {
                    let fallback =
                        std::iter::once(preview::Dependency::workspace(canonical_input.clone()))
                            .chain(
                                request
                                    .project
                                    .html
                                    .stylesheet_files
                                    .iter()
                                    .cloned()
                                    .map(preview::Dependency::configuration),
                            )
                            .chain(
                                build_authorities
                                    .explicit_stylesheets
                                    .candidates()
                                    .map(preview::Dependency::explicit_stylesheet),
                            )
                            .collect::<Vec<_>>();
                    dependencies.extend(build_authorities.snapshot(&fallback));
                    Ok(preview::Build::failure(error.to_string(), dependencies))
                }
            }
        },
        move |dependencies| snapshot_authorities.snapshot(dependencies),
        shutdown,
    )
    .map_err(Error::Server)
}

fn build(
    request: BuildRequest<'_>,
    cancellation: &CancellationToken,
    dependencies: &mut BTreeMap<preview::Dependency, preview::Fingerprint>,
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
    dependencies: &mut BTreeMap<preview::Dependency, preview::Fingerprint>,
    mut stage_hook: impl FnMut(BuildStage),
) -> Result<preview::Build, Error> {
    ensure_active(cancellation)?;
    let plan = request.project.resources.limit_plan;
    let mut filesystem = request
        .authorities
        .filesystem_access
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
    let input_fingerprint = preview::Fingerprint::from_loaded_bytes(input.as_bytes());
    ensure_active(cancellation)?;
    let source = input.as_ref();
    ensure_active(cancellation)?;
    let source_id = request.input_path.to_string_lossy().into_owned();
    dependencies.insert(
        preview::Dependency::workspace(request.input_path),
        input_fingerprint,
    );

    let (processed, include_diagnostics) = if request.include {
        ensure_active(cancellation)?;
        let prepared = {
            let mut observer = DependencyObserver {
                dependencies,
                authorities: request.authorities,
            };
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
    let mut configuration_stylesheets =
        crate::configuration_stylesheet_session(request.authorities.configuration_policy.clone());
    let render_policy = html_policy::build(
        &request.project.html,
        true,
        request.css,
        |origin, path| {
            let (dependency, bytes, fingerprint) = match origin {
                StylesheetFileOrigin::ProjectConfiguration => {
                    let bytes = configuration_stylesheets
                        .read_candidate_bytes(path)
                        .map(|loaded| loaded.into_parts().1)
                        .map_err(io::Error::other)?;
                    let fingerprint = preview::Fingerprint::from_loaded_bytes(&bytes);
                    (preview::Dependency::configuration(path), bytes, fingerprint)
                }
                StylesheetFileOrigin::CommandLine => {
                    request.authorities.read_explicit_stylesheet(path)?
                }
            };
            dependencies.insert(dependency, fingerprint);
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

    fn test_authorities(
        root: &Path,
        project: &adocweave_config::ResolvedProjectConfig,
        css: &[StylesheetArgument],
    ) -> PreviewAuthorities {
        let filesystem_policy = adocweave_host::LocalFilesystemPolicy::new(
            [root.to_owned()],
            adocweave_host::FilesystemReadLimits::default(),
        )
        .expect("filesystem policy");
        let filesystem_roots = filesystem_policy.roots().to_vec();
        let configuration_policy = filesystem_policy
            .root_policy(&filesystem_roots[0])
            .expect("configuration policy")
            .clone();
        let filesystem_access = filesystem_policy
            .access_existing(
                filesystem_roots,
                project.resources.limit_plan.filesystem_reads,
            )
            .expect("filesystem access");
        PreviewAuthorities::new(&RunRequest {
            input_path: &root.join("manual.adoc"),
            include: true,
            base_dir: Some(root),
            allowed_roots: &[],
            project_root: Some(root),
            project,
            css,
            configuration_policy,
            filesystem_access,
            server: ServerOptions {
                bind: "127.0.0.1".parse().expect("loopback"),
                port: 0,
                debounce_ms: 0,
            },
        })
        .expect("preview authorities")
    }

    #[test]
    fn preview_rejects_excess_stylesheets_before_opening_authorities() {
        let root = tempfile::tempdir().expect("temporary directory");
        let filesystem_policy = adocweave_host::LocalFilesystemPolicy::new(
            [root.path().to_owned()],
            adocweave_host::FilesystemReadLimits::default(),
        )
        .expect("filesystem policy");
        let filesystem_roots = filesystem_policy.roots().to_vec();
        let configuration_policy = filesystem_policy
            .root_policy(&filesystem_roots[0])
            .expect("configuration policy")
            .clone();
        let project = adocweave_config::ResolvedProjectConfig {
            html: adocweave_config::HtmlSettings {
                stylesheet_urls: vec!["https://example.com/project.css".to_owned()],
                ..adocweave_config::HtmlSettings::default()
            },
            ..adocweave_config::ResolvedProjectConfig::default()
        };
        let css = (0..16)
            .map(|index| {
                StylesheetArgument::File(
                    root.path()
                        .join(format!("missing-{index}"))
                        .join("style.css"),
                )
            })
            .collect::<Vec<_>>();
        let filesystem_access = filesystem_policy
            .access_existing(
                filesystem_roots,
                project.resources.limit_plan.filesystem_reads,
            )
            .expect("filesystem access");

        let result = PreviewAuthorities::new(&RunRequest {
            input_path: &root.path().join("manual.adoc"),
            include: false,
            base_dir: Some(root.path()),
            allowed_roots: &[],
            project_root: Some(root.path()),
            project: &project,
            css: &css,
            configuration_policy,
            filesystem_access,
            server: ServerOptions {
                bind: "127.0.0.1".parse().expect("loopback"),
                port: 0,
                debounce_ms: 0,
            },
        });

        assert!(matches!(
            result,
            Err(Error::Html(html_policy::Error::Stylesheet(message)))
                if message == "stylesheet count exceeds the limit of 16"
        ));
    }

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
        let project = adocweave_config::ResolvedProjectConfig::default();
        let css = [StylesheetArgument::File(stylesheet.clone())];
        let authorities = test_authorities(root.path(), &project, &css);

        let result = build(
            BuildRequest {
                input_path: &input,
                include: true,
                base_dir: root.path(),
                project_root: root.path(),
                project: &project,
                css: &css,
                authorities: &authorities,
            },
            &CancellationToken::new(),
            &mut dependencies,
        );

        assert!(result.is_err());
        assert!(dependencies.contains_key(&preview::Dependency::workspace(input)));
        assert!(dependencies.contains_key(&preview::Dependency::workspace(include)));
        assert!(dependencies.contains_key(&preview::Dependency::explicit_stylesheet(stylesheet)));
    }

    #[test]
    fn preprocess_failure_retains_dependencies_discovered_before_the_error() {
        let root = tempfile::tempdir().expect("temporary directory");
        let input = root.path().join("manual.adoc");
        let include = root.path().join("chapter.adoc");
        std::fs::write(&input, "include::chapter.adoc[]\n").expect("root document");
        std::fs::write(&include, "include::chapter.adoc[]\n").expect("cyclic include");
        let mut dependencies = BTreeMap::new();
        let project = adocweave_config::ResolvedProjectConfig::default();
        let authorities = test_authorities(root.path(), &project, &[]);

        let result = build(
            BuildRequest {
                input_path: &input,
                include: true,
                base_dir: root.path(),
                project_root: root.path(),
                project: &project,
                css: &[],
                authorities: &authorities,
            },
            &CancellationToken::new(),
            &mut dependencies,
        );

        assert!(result.is_err());
        assert!(dependencies.contains_key(&preview::Dependency::workspace(include)));
    }

    #[test]
    fn observer_records_the_loaded_snapshot() {
        let root = tempfile::tempdir().expect("temporary directory");
        let include = root.path().join("chapter.adoc");
        std::fs::write(&include, "later snapshot\n").expect("included document");
        let mut dependencies = BTreeMap::new();
        let project = adocweave_config::ResolvedProjectConfig::default();
        let authorities = test_authorities(root.path(), &project, &[]);

        DependencyObserver {
            dependencies: &mut dependencies,
            authorities: &authorities,
        }
        .observe_loaded(&include, "first snapshot\n");

        let observed = dependencies
            .get(&preview::Dependency::workspace(&include))
            .expect("observed dependency");
        assert_eq!(
            observed,
            &preview::Fingerprint::from_loaded_bytes(b"first snapshot\n")
        );
        assert_ne!(
            observed,
            &preview::Fingerprint::from_loaded_bytes(b"later snapshot\n")
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn build_keeps_all_retained_authorities_after_root_replacement() {
        let parent = tempfile::tempdir().expect("temporary parent");
        let root = parent.path().join("workspace");
        std::fs::create_dir(&root).expect("workspace");
        let input = root.join("manual.adoc");
        let include = root.join("chapter.adoc");
        let configured = root.join("configured.css");
        let explicit = root.join("explicit.css");
        std::fs::write(&input, "trusted input\ninclude::chapter.adoc[]\n").expect("trusted input");
        std::fs::write(&include, "trusted include\n").expect("trusted include");
        std::fs::write(&configured, "/* trusted configured */").expect("configured stylesheet");
        std::fs::write(&explicit, "/* trusted explicit */").expect("explicit stylesheet");
        let project = adocweave_config::ResolvedProjectConfig {
            html: adocweave_config::HtmlSettings {
                stylesheet_files: vec![configured.clone()],
                ..adocweave_config::HtmlSettings::default()
            },
            ..adocweave_config::ResolvedProjectConfig::default()
        };
        let css = [StylesheetArgument::File(explicit.clone())];
        let authorities = test_authorities(&root, &project, &css);
        let displaced = parent.path().join("retained-workspace");

        std::fs::rename(&root, &displaced).expect("displace workspace");
        std::fs::create_dir(&root).expect("replacement workspace");
        std::fs::write(&input, "outside input\n").expect("replacement input");
        std::fs::write(&include, "outside include\n").expect("replacement include");
        std::fs::write(&configured, "/* outside configured */")
            .expect("replacement configured stylesheet");
        std::fs::write(&explicit, "/* outside explicit */")
            .expect("replacement explicit stylesheet");

        let mut dependencies = BTreeMap::new();
        let build = build(
            BuildRequest {
                input_path: &input,
                include: true,
                base_dir: &root,
                project_root: &root,
                project: &project,
                css: &css,
                authorities: &authorities,
            },
            &CancellationToken::new(),
            &mut dependencies,
        )
        .expect("preview build");

        assert!(build.html.contains("trusted input"));
        assert!(build.html.contains("trusted include"));
        assert!(!build.html.contains("outside"));
        assert_eq!(
            dependencies.get(&preview::Dependency::workspace(&input)),
            Some(&preview::Fingerprint::from_loaded_bytes(
                b"trusted input\ninclude::chapter.adoc[]\n"
            ))
        );
        assert_eq!(
            dependencies.get(&preview::Dependency::workspace(&include)),
            Some(&preview::Fingerprint::from_loaded_bytes(
                b"trusted include\n"
            ))
        );
        assert_eq!(
            dependencies.get(&preview::Dependency::configuration(&configured)),
            Some(&preview::Fingerprint::from_loaded_bytes(
                b"/* trusted configured */"
            ))
        );
        assert_eq!(
            dependencies.get(&preview::Dependency::explicit_stylesheet(&explicit)),
            Some(&preview::Fingerprint::from_loaded_bytes(
                b"/* trusted explicit */"
            ))
        );
        std::fs::remove_dir_all(&root).expect("remove replacement workspace");
        std::fs::rename(displaced, &root).expect("restore workspace");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn dependency_snapshots_keep_each_retained_authority_after_root_replacement() {
        let parent = tempfile::tempdir().expect("temporary parent");
        let root = parent.path().join("workspace");
        std::fs::create_dir(&root).expect("workspace");
        let input = root.join("manual.adoc");
        let configured = root.join("configured.css");
        let explicit = root.join("explicit.css");
        std::fs::write(&input, "trusted input\n").expect("trusted input");
        std::fs::write(&configured, "trusted configured").expect("configured stylesheet");
        std::fs::write(&explicit, "trusted explicit").expect("explicit stylesheet");
        let project = adocweave_config::ResolvedProjectConfig::default();
        let css = [StylesheetArgument::File(explicit.clone())];
        let authorities = test_authorities(&root, &project, &css);
        let displaced = parent.path().join("retained-workspace");

        std::fs::rename(&root, &displaced).expect("displace workspace");
        std::fs::create_dir(&root).expect("replacement workspace");
        std::fs::write(&input, "outside input\n").expect("replacement input");
        std::fs::write(&configured, "outside configured").expect("replacement configured");
        std::fs::write(&explicit, "outside explicit").expect("replacement explicit");

        let workspace_dependency = preview::Dependency::workspace(&input);
        let configured_dependency = preview::Dependency::configuration(&configured);
        let explicit_dependency = preview::Dependency::explicit_stylesheet(&explicit);
        let snapshots = authorities.snapshot(&[
            workspace_dependency.clone(),
            configured_dependency.clone(),
            explicit_dependency.clone(),
        ]);

        assert_eq!(
            snapshots.get(&workspace_dependency),
            Some(&preview::Fingerprint::from_loaded_bytes(b"trusted input\n"))
        );
        assert_eq!(
            snapshots.get(&configured_dependency),
            Some(&preview::Fingerprint::from_loaded_bytes(
                b"trusted configured"
            ))
        );
        assert_eq!(
            snapshots.get(&explicit_dependency),
            Some(&preview::Fingerprint::from_loaded_bytes(
                b"trusted explicit"
            ))
        );
        std::fs::remove_dir_all(&root).expect("remove replacement workspace");
        std::fs::rename(displaced, &root).expect("restore workspace");
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
        let project = adocweave_config::ResolvedProjectConfig::default();
        let authorities = test_authorities(root.path(), &project, &[]);

        let result = build_with_stage_hook(
            BuildRequest {
                input_path: &input,
                include: true,
                base_dir: root.path(),
                project_root: root.path(),
                project: &project,
                css: &[],
                authorities: &authorities,
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
            dependencies.get(&preview::Dependency::workspace(&include)),
            Some(&preview::Fingerprint::from_loaded_bytes(
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
        let project = adocweave_config::ResolvedProjectConfig::default();
        let authorities = test_authorities(root.path(), &project, &[]);

        let result = build_with_stage_hook(
            BuildRequest {
                input_path: &input,
                include: true,
                base_dir: root.path(),
                project_root: root.path(),
                project: &project,
                css: &[],
                authorities: &authorities,
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
            dependencies.get(&preview::Dependency::workspace(&missing)),
            authorities
                .snapshot(&[preview::Dependency::workspace(&missing)])
                .get(&preview::Dependency::workspace(&missing))
        );
    }
}
