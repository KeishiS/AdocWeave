//! LSP URI and filesystem adapter for the runtime-independent workspace.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use adocweave::preprocess::{PreprocessOptions, ProjectionLimits, SafeMode};
use adocweave_host::{LocalFilesystemPolicy, LocalFilesystemSession, LogicalSourceId};
use adocweave_workspace::{
    Generation, ResourceId, RetainedResourceBudget, RetainedResourceLimits, Revision, Workspace,
    WorkspaceAnalysis, WorkspaceLimits, WorkspaceSnapshot,
};
use async_lsp::lsp_types::Url;

#[derive(Clone, Debug)]
pub struct WorkspaceInput {
    pub generation: Generation,
    pub root: ResourceId,
    pub snapshot: WorkspaceSnapshot,
    pub options: PreprocessOptions,
    pub project_config: adocweave_config::ResolvedProjectConfig,
    pub config_sha256: Option<[u8; 32]>,
}

impl WorkspaceInput {
    #[cfg(test)]
    pub fn root_text(&self) -> Option<&Arc<str>> {
        self.snapshot
            .get(&self.root)
            .map(adocweave_workspace::Resource::text)
    }

    pub fn analyze(
        &self,
        analysis_options: &adocweave::AnalysisOptions,
        cancellation: &adocweave::CancellationToken,
    ) -> Result<WorkspaceAnalysis, adocweave_workspace::WorkspaceError> {
        self.snapshot.analyze(
            &self.root,
            analysis_options,
            &self.options,
            ProjectionLimits::default(),
            cancellation,
        )
    }
}

#[derive(Clone, Debug, Default)]
pub struct WorkspaceResources {
    inner: Workspace,
    roots: Vec<PathBuf>,
    filesystems: BTreeMap<Option<PathBuf>, Arc<Mutex<LocalFilesystemSession>>>,
    project_plans: BTreeMap<Option<PathBuf>, adocweave_config::ResolvedResourceLimitPlan>,
    resource_projects: BTreeMap<ResourceId, Option<PathBuf>>,
    retained_layers: BTreeMap<Option<PathBuf>, RetainedResourceBudget>,
    next_disk_version: i64,
}

impl WorkspaceResources {
    pub fn load_roots(&mut self, roots: &[Url]) -> Result<(), String> {
        self.load_roots_with_limits(roots, adapter_managed_workspace_limits())
    }

    fn load_roots_with_limits(
        &mut self,
        roots: &[Url],
        limits: WorkspaceLimits,
    ) -> Result<(), String> {
        let mut paths = roots
            .iter()
            .map(|root| {
                root.to_file_path()
                    .map_err(|()| format!("workspace root is not a file URI: {root}"))?
                    .canonicalize()
                    .map_err(|error| format!("cannot canonicalize workspace root: {error}"))
            })
            .collect::<Result<Vec<_>, _>>()?;
        paths.sort();
        paths.dedup();
        let discovery = (!paths.is_empty())
            .then(|| {
                LocalFilesystemPolicy::new(
                    paths.clone(),
                    adocweave_host::FilesystemReadLimits::default(),
                )
            })
            .transpose()
            .map_err(|error| error.to_string())?
            .map(|policy| policy.session())
            .transpose()
            .map_err(|error| error.to_string())?;
        let candidates = match discovery.as_ref() {
            Some(session) => session
                .discover_adoc_paths()
                .map_err(|error| error.to_string())?,
            None => Vec::new(),
        };
        // Retain only the trusted canonical boundaries when a project file is
        // invalid. Open overlays must still resolve that invalid nearest file
        // and fail closed instead of falling back to the built-in plan.
        self.roots = paths.clone();
        let seed = Generation::new(self.inner.generation().get().saturating_add(1));
        self.inner = Workspace::new_at_generation(limits, seed);
        self.filesystems.clear();
        self.project_plans.clear();
        self.resource_projects.clear();
        self.retained_layers.clear();
        let mut inner = Workspace::new_at_generation(limits, seed);
        let mut filesystems = BTreeMap::new();
        let mut resource_projects = BTreeMap::new();
        let mut project_plans = BTreeMap::new();
        let mut retained_layers: BTreeMap<Option<PathBuf>, RetainedResourceBudget> =
            BTreeMap::new();
        let mut next_disk_version = self.next_disk_version;
        for path in candidates {
            let config = config_for_path(&paths, &path)?;
            let project = config.as_ref().map(|snapshot| snapshot.path.clone());
            let plan = config.as_ref().map_or_else(
                adocweave_config::ResolvedResourceLimitPlan::default,
                |snapshot| snapshot.config.resources.limit_plan,
            );
            if let Some(previous) = project_plans.insert(project.clone(), plan)
                && previous != plan
            {
                return Err("project resource limit plan changed during workspace scan".to_owned());
            }
            let filesystem = match filesystems.entry(project.clone()) {
                std::collections::btree_map::Entry::Occupied(entry) => entry.into_mut(),
                std::collections::btree_map::Entry::Vacant(entry) => {
                    let session = LocalFilesystemPolicy::new(paths.clone(), plan.filesystem_reads)
                        .map_err(|error| error.to_string())?
                        .session()
                        .map_err(|error| error.to_string())?;
                    entry.insert(Arc::new(Mutex::new(session)))
                }
            };
            let uri = Url::from_file_path(&path).map_err(|()| {
                format!("cannot convert workspace path to URI: {}", path.display())
            })?;
            let file = filesystem
                .lock()
                .map_err(|_| "workspace resource session lock is poisoned".to_owned())?
                .read_utf8(
                    LogicalSourceId::new(uri.to_string()).map_err(|error| error.to_string())?,
                    &path,
                )
                .map_err(|error| error.to_string())?;
            next_disk_version = next_disk_version.saturating_add(1);
            let (source_id, text) = file.into_parts();
            let id = ResourceId::new(source_id.as_str()).map_err(|error| error.to_string())?;
            let budget = retained_layers
                .get(&project)
                .cloned()
                .unwrap_or_default()
                .with_disk(id.clone(), Some(text.len() as u64), plan.retained_layers)
                .map_err(|error| error.to_string())?;
            inner
                .upsert_disk(id.clone(), Revision::new(next_disk_version), text)
                .map_err(|error| error.to_string())?;
            inner
                .register_root(id.clone())
                .map_err(|error| error.to_string())?;
            retained_layers.insert(project.clone(), budget);
            resource_projects.insert(id, project);
        }
        self.inner = inner;
        self.roots = paths;
        self.filesystems = filesystems;
        self.project_plans = project_plans;
        self.resource_projects = resource_projects;
        self.retained_layers = retained_layers;
        self.next_disk_version = next_disk_version;
        Ok(())
    }

    pub fn reload_file(&mut self, uri: Url) -> Result<BTreeSet<String>, String> {
        let path = uri
            .to_file_path()
            .map_err(|()| format!("workspace resource is not a file URI: {uri}"))?;
        let (project, plan) = self.plan_for_path(&path)?;
        let text = self.read_workspace_file(&path, project.clone(), plan.filesystem_reads)?;
        self.next_disk_version = self.next_disk_version.saturating_add(1);
        let id = uri_id(&uri)?;
        let budget = self
            .retained_layers
            .get(&project)
            .cloned()
            .unwrap_or_default()
            .with_disk(id.clone(), Some(text.len() as u64), plan.retained_layers)
            .map_err(|error| error.to_string())?;
        let affected = self
            .inner
            .upsert_disk(id.clone(), Revision::new(self.next_disk_version), text)
            .map_err(|error| error.to_string())?;
        if !self.inner.roots().contains(&id) {
            self.inner
                .register_root(id.clone())
                .map_err(|error| error.to_string())?;
        }
        self.retained_layers.insert(project.clone(), budget);
        self.project_plans.insert(project.clone(), plan);
        self.resource_projects.insert(id, project);
        Ok(strings(affected))
    }

    fn read_workspace_file(
        &mut self,
        path: &Path,
        project: Option<PathBuf>,
        limits: adocweave_host::FilesystemReadLimits,
    ) -> Result<Arc<str>, String> {
        if path.extension().and_then(|value| value.to_str()) != Some("adoc") {
            return Err(format!(
                "workspace resource is not an .adoc file: {}",
                path.display()
            ));
        }
        if !self.filesystems.contains_key(&project) {
            let session = LocalFilesystemPolicy::new(self.roots.clone(), limits)
                .map_err(|error| error.to_string())?
                .session()
                .map_err(|error| error.to_string())?;
            self.filesystems
                .insert(project.clone(), Arc::new(Mutex::new(session)));
        }
        self.filesystems
            .get(&project)
            .expect("project filesystem session was inserted")
            .lock()
            .map_err(|_| "workspace resource session lock is poisoned".to_owned())?
            .reread_utf8(
                LogicalSourceId::new(path.to_string_lossy().into_owned())
                    .map_err(|error| error.to_string())?,
                path,
            )
            .map(|loaded| loaded.into_parts().1)
            .map_err(|error| error.to_string())
    }

    pub fn get(&self, uri: &Url) -> Option<&adocweave_workspace::Resource> {
        let id = uri_id(uri).ok()?;
        self.inner.get(&id)
    }

    pub fn upsert_open(
        &mut self,
        uri: Url,
        version: i64,
        text: impl Into<Arc<str>>,
    ) -> Result<BTreeSet<String>, String> {
        let id = uri_id(&uri)?;
        let text = text.into();
        let path = uri
            .to_file_path()
            .map_err(|()| format!("workspace resource is not a file URI: {uri}"))?;
        let (project, plan) = self.plan_for_path(&path)?;
        let budget = self
            .retained_layers
            .get(&project)
            .cloned()
            .unwrap_or_default()
            .with_overlay(id.clone(), Some(text.len() as u64), plan.retained_layers)
            .map_err(|error| error.to_string())?;
        let affected = self
            .inner
            .upsert_overlay(id.clone(), Revision::new(version), text)
            .map_err(|error| error.to_string())?;
        if !self.inner.roots().contains(&id) {
            self.inner
                .register_root(id.clone())
                .map_err(|error| error.to_string())?;
        }
        let mut affected = strings(affected);
        affected.insert(id.to_string());
        self.retained_layers.insert(project.clone(), budget);
        self.project_plans.insert(project.clone(), plan);
        self.resource_projects.insert(id, project);
        Ok(affected)
    }

    pub fn remove_disk(&mut self, uri: &Url) -> BTreeSet<String> {
        let id = uri_id(uri).ok();
        let project = id
            .as_ref()
            .and_then(|id| self.resource_projects.get(id))
            .cloned()
            .flatten();
        if let Ok(path) = uri.to_file_path()
            && let Some(filesystem) = self.filesystems.get(&project)
            && let Ok(mut session) = filesystem.lock()
        {
            session.release(&path);
        }
        let Some(id) = id else {
            return BTreeSet::new();
        };
        let affected = strings(self.inner.remove_disk(&id));
        if let Some(project) = self.resource_projects.get(&id).cloned()
            && let Some(plan) = self.project_plans.get(&project).copied()
        {
            let budget = self
                .retained_layers
                .get(&project)
                .cloned()
                .unwrap_or_default()
                .with_disk(id.clone(), None, plan.retained_layers);
            if let Ok(budget) = budget {
                self.retained_layers.insert(project, budget);
            }
        }
        if self.inner.get(&id).is_none() {
            self.resource_projects.remove(&id);
        }
        affected
    }

    pub fn close_open(&mut self, uri: &Url) -> Result<BTreeSet<String>, String> {
        let id = uri_id(uri)?;
        let affected = self
            .inner
            .close_overlay(&id)
            .map_err(|error| error.to_string())?;
        if let Some(project) = self.resource_projects.get(&id).cloned() {
            let plan = self
                .project_plans
                .get(&project)
                .copied()
                .ok_or_else(|| "workspace resource limit plan is missing".to_owned())?;
            let budget = self
                .retained_layers
                .get(&project)
                .cloned()
                .unwrap_or_default()
                .with_overlay(id.clone(), None, plan.retained_layers)
                .map_err(|error| error.to_string())?;
            self.retained_layers.insert(project, budget);
        }
        if self.inner.get(&id).is_none() {
            self.resource_projects.remove(&id);
        }
        Ok(strings(affected))
    }

    pub fn input(&self, root: &Url) -> Result<WorkspaceInput, String> {
        let root_id = uri_id(root)?;
        if self.inner.get(&root_id).is_none() {
            return Err(format!("workspace resource is missing: {root}"));
        }
        let mut allowed_schemes = BTreeSet::new();
        allowed_schemes.insert("file".to_owned());
        let snapshot = self.inner.snapshot();
        let config_snapshot = self.config_for_uri(root)?;
        let project_config = config_snapshot.as_ref().map_or_else(
            adocweave_config::ResolvedProjectConfig::default,
            |snapshot| snapshot.config.clone(),
        );
        let mut options = project_config.preprocess.clone();
        if config_snapshot.is_none() {
            options.enable_includes = true;
        }
        options.base_uri = parent_uri(root);
        options.safe_mode = SafeMode::Server;
        options.allowed_schemes = allowed_schemes;
        let allowed_roots = if options.enable_includes {
            project_config
                .resources
                .roots
                .iter()
                .map(|root| {
                    let canonical = root
                        .canonicalize()
                        .map_err(|error| format!("cannot canonicalize configured root: {error}"))?;
                    if !self
                        .roots
                        .iter()
                        .any(|workspace_root| canonical.starts_with(workspace_root))
                    {
                        return Err(format!(
                            "configured root is outside the workspace: {}",
                            root.display()
                        ));
                    }
                    Ok(canonical)
                })
                .collect::<Result<Vec<_>, _>>()?
        } else {
            Vec::new()
        };
        let snapshot = snapshot.filter_resources(|id, _| {
            if id == &root_id {
                return true;
            }
            if !options.enable_includes {
                return false;
            }
            if allowed_roots.is_empty() {
                return true;
            }
            Url::parse(id.as_str())
                .ok()
                .and_then(|uri| uri.to_file_path().ok())
                .is_some_and(|path| allowed_roots.iter().any(|root| path.starts_with(root)))
        });
        let retained_files = snapshot.resources().count();
        let retained_bytes = snapshot
            .resources()
            .try_fold(0_u64, |total, (_, resource)| {
                total.checked_add(resource.text().len() as u64)
            });
        let limits = project_config.resources.limit_plan.analysis_snapshot;
        let oversized_resource = snapshot
            .resources()
            .any(|(_, resource)| resource.text().len() as u64 > limits.max_resource_bytes);
        if retained_files > limits.max_resources
            || oversized_resource
            || retained_bytes.is_none_or(|bytes| bytes > limits.max_total_bytes)
        {
            return Err("configured analysis snapshot resource limit exceeded".to_owned());
        }
        Ok(WorkspaceInput {
            generation: snapshot.generation(),
            root: root_id,
            snapshot,
            options,
            config_sha256: config_snapshot.map(|snapshot| snapshot.content_sha256),
            project_config,
        })
    }

    pub fn input_is_current(&self, input: &WorkspaceInput) -> bool {
        input.generation == self.generation()
            && self.config_for_id(&input.root).is_ok_and(|snapshot| {
                snapshot.map(|value| value.content_sha256) == input.config_sha256
            })
    }

    pub fn accept(&mut self, analysis: &WorkspaceAnalysis) -> Result<(), String> {
        self.inner
            .accept(analysis)
            .map_err(|error| error.to_string())
    }

    pub const fn generation(&self) -> Generation {
        self.inner.generation()
    }

    fn config_for_id(
        &self,
        id: &ResourceId,
    ) -> Result<Option<adocweave_config::ConfigSnapshot>, String> {
        let uri = Url::parse(id.as_str()).map_err(|error| error.to_string())?;
        self.config_for_uri(&uri)
    }

    fn config_for_uri(
        &self,
        uri: &Url,
    ) -> Result<Option<adocweave_config::ConfigSnapshot>, String> {
        let path = uri
            .to_file_path()
            .map_err(|()| format!("workspace resource is not a file URI: {uri}"))?;
        config_for_path(&self.roots, &path)
    }

    fn plan_for_path(
        &self,
        path: &Path,
    ) -> Result<(Option<PathBuf>, adocweave_config::ResolvedResourceLimitPlan), String> {
        let config = config_for_path(&self.roots, path)?;
        Ok(config.map_or_else(
            || (None, adocweave_config::ResolvedResourceLimitPlan::default()),
            |snapshot| (Some(snapshot.path), snapshot.config.resources.limit_plan),
        ))
    }
}

const fn adapter_managed_workspace_limits() -> WorkspaceLimits {
    WorkspaceLimits {
        resources: RetainedResourceLimits {
            max_files: usize::MAX,
            max_total_bytes: u64::MAX,
            max_resource_bytes: u64::MAX,
        },
        max_roots: usize::MAX,
    }
}

fn uri_id(uri: &Url) -> Result<ResourceId, String> {
    ResourceId::new(uri.to_string()).map_err(|error| error.to_string())
}

fn config_for_path(
    roots: &[PathBuf],
    path: &Path,
) -> Result<Option<adocweave_config::ConfigSnapshot>, String> {
    let boundary = roots
        .iter()
        .filter(|root| path.starts_with(root))
        .max_by_key(|root| root.components().count());
    let Some(boundary) = boundary else {
        return Ok(None);
    };
    let mut start = path;
    while !start.exists() {
        let Some(parent) = start.parent() else {
            return Ok(None);
        };
        start = parent;
    }
    adocweave_config::discover_and_load(start, boundary).map_err(|error| error.to_string())
}

fn strings(values: BTreeSet<ResourceId>) -> BTreeSet<String> {
    values.into_iter().map(|value| value.to_string()).collect()
}

fn parent_uri(uri: &Url) -> Option<String> {
    uri.join(".").ok().map(|uri| uri.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_DIRECTORY: AtomicU64 = AtomicU64::new(0);

    struct TestDirectory(PathBuf);

    impl TestDirectory {
        fn new() -> Self {
            let id = NEXT_DIRECTORY.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "adocweave-lsp-filesystem-session-{}-{id}",
                std::process::id()
            ));
            std::fs::create_dir(&path).expect("workspace root");
            Self(path)
        }
    }

    impl Drop for TestDirectory {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn write_resource_config(
        directory: &Path,
        max_files: usize,
        max_total_bytes: u64,
        max_resource_bytes: u64,
        include: bool,
    ) {
        std::fs::write(
            directory.join(adocweave_config::FILE_NAME),
            format!(
                "schema-version = 1\n[resources]\ninclude = {include}\nroots = [\".\"]\nmax-files = {max_files}\nmax-total-bytes = {max_total_bytes}\nmax-resource-bytes = {max_resource_bytes}\n"
            ),
        )
        .expect("project configuration");
    }

    #[test]
    fn filesystem_scan_ingests_logical_resources_before_snapshot_analysis() {
        let root = TestDirectory::new();
        let first = root.0.join("a.adoc");
        let second = root.0.join("b.adoc");
        std::fs::write(&first, "first\n").expect("first source");
        std::fs::write(&second, "second\n").expect("second source");
        std::fs::write(root.0.join("ignored.txt"), "ignored\n").expect("ignored source");
        let root_uri = Url::from_directory_path(&root.0).expect("root URI");
        let first_uri = Url::from_file_path(&first).expect("first URI");
        let second_uri = Url::from_file_path(&second).expect("second URI");
        let mut resources = WorkspaceResources::default();

        resources.load_roots(&[root_uri]).expect("load workspace");

        assert_eq!(
            resources.inner.roots(),
            &BTreeSet::from([
                uri_id(&first_uri).expect("first resource ID"),
                uri_id(&second_uri).expect("second resource ID"),
            ])
        );
        assert_eq!(
            resources
                .get(&first_uri)
                .expect("first resource")
                .text()
                .as_ref(),
            "first\n"
        );
        assert_eq!(
            resources
                .get(&second_uri)
                .expect("second resource")
                .text()
                .as_ref(),
            "second\n"
        );

        let input = resources.input(&first_uri).expect("workspace input");
        std::fs::remove_file(first).expect("remove first source after snapshot");
        std::fs::remove_file(second).expect("remove second source after snapshot");
        assert_eq!(
            input
                .snapshot
                .get(&input.root)
                .expect("snapshot resource")
                .text()
                .as_ref(),
            "first\n"
        );
    }

    #[test]
    fn resolved_default_plan_names_each_budget_domain() {
        let plan = adocweave_config::ResolvedResourceLimitPlan::default();
        assert_eq!(
            plan.filesystem_reads,
            adocweave_host::FilesystemReadLimits::default()
        );
        assert_eq!(
            plan.retained_layers,
            adocweave_workspace::RetainedResourceLimits::default()
        );
        assert_eq!(
            plan.analysis_snapshot.max_resources,
            plan.filesystem_reads.max_files
        );
    }

    #[test]
    fn watched_file_reload_reads_the_new_filesystem_snapshot() {
        let root = TestDirectory::new();
        let path = root.0.join("document.adoc");
        std::fs::write(&path, "first\n").expect("initial source");
        let root_uri = Url::from_directory_path(&root.0).expect("root URI");
        let document_uri = Url::from_file_path(&path).expect("document URI");
        let mut resources = WorkspaceResources::default();
        resources.load_roots(&[root_uri]).expect("load workspace");
        assert_eq!(
            resources
                .get(&document_uri)
                .expect("initial resource")
                .text()
                .as_ref(),
            "first\n"
        );

        std::fs::write(&path, "second\n").expect("updated source");
        resources
            .reload_file(document_uri.clone())
            .expect("reload resource");

        assert_eq!(
            resources
                .get(&document_uri)
                .expect("updated resource")
                .text()
                .as_ref(),
            "second\n"
        );
    }

    #[test]
    fn watched_file_reloads_share_the_workspace_filesystem_budget() {
        let root = TestDirectory::new();
        let first = root.0.join("first.adoc");
        let second = root.0.join("second.adoc");
        std::fs::write(&first, "first\n").expect("initial source");
        let root_uri = Url::from_directory_path(&root.0).expect("root URI");
        let mut resources = WorkspaceResources::default();
        let mut limits = WorkspaceLimits::default();
        limits.resources.max_files = 1;
        resources
            .load_roots_with_limits(&[root_uri], limits)
            .expect("load workspace");

        std::fs::write(&second, "second\n").expect("new source");
        let second_uri = Url::from_file_path(&second).expect("document URI");
        let error = resources
            .reload_file(second_uri)
            .expect_err("shared file limit");

        assert!(error.contains("file limit"), "{error}");
    }

    #[test]
    fn removing_a_disk_resource_releases_its_filesystem_charge() {
        let root = TestDirectory::new();
        let first = root.0.join("first.adoc");
        let second = root.0.join("second.adoc");
        std::fs::write(&first, "first\n").expect("initial source");
        let root_uri = Url::from_directory_path(&root.0).expect("root URI");
        let first_uri = Url::from_file_path(&first).expect("first URI");
        let mut resources = WorkspaceResources::default();
        let mut limits = WorkspaceLimits::default();
        limits.resources.max_files = 1;
        resources
            .load_roots_with_limits(&[root_uri], limits)
            .expect("load workspace");

        std::fs::remove_file(&first).expect("remove first");
        resources.remove_disk(&first_uri);
        std::fs::write(&second, "second\n").expect("new source");
        let second_uri = Url::from_file_path(&second).expect("second URI");
        resources
            .reload_file(second_uri.clone())
            .expect("released file charge");

        assert_eq!(
            resources
                .get(&second_uri)
                .expect("second resource")
                .text()
                .as_ref(),
            "second\n"
        );
    }

    #[test]
    fn nearest_project_plan_rejects_an_oversized_disk_resource_before_ingest() {
        let root = TestDirectory::new();
        write_resource_config(&root.0, 2, 8, 4, false);
        let path = root.0.join("document.adoc");
        std::fs::write(&path, "12345").expect("source");
        let root_uri = Url::from_directory_path(&root.0).expect("root URI");
        let document_uri = Url::from_file_path(&path).expect("document URI");
        let mut resources = WorkspaceResources::default();

        let error = resources
            .load_roots(&[root_uri])
            .expect_err("strict disk limit");

        assert!(error.contains("too large"), "{error}");
        assert!(resources.get(&document_uri).is_none());
    }

    #[test]
    fn retained_layer_plan_rejects_overlay_bytes_before_workspace_ingest() {
        let root = TestDirectory::new();
        write_resource_config(&root.0, 1, 3, 3, false);
        let path = root.0.join("document.adoc");
        std::fs::write(&path, "a\n").expect("source");
        let root_uri = Url::from_directory_path(&root.0).expect("root URI");
        let document_uri = Url::from_file_path(&path).expect("document URI");
        let mut resources = WorkspaceResources::default();
        resources.load_roots(&[root_uri]).expect("load workspace");

        let error = resources
            .upsert_open(document_uri.clone(), 1, "b\n")
            .expect_err("disk and overlay byte limit");

        assert!(error.contains("retained resource byte"), "{error}");
        assert_eq!(
            resources
                .get(&document_uri)
                .expect("disk layer remains effective")
                .text()
                .as_ref(),
            "a\n"
        );
    }

    #[test]
    fn configured_read_charge_is_released_before_a_new_reload() {
        let root = TestDirectory::new();
        write_resource_config(&root.0, 1, 8, 8, false);
        let first = root.0.join("first.adoc");
        let second = root.0.join("second.adoc");
        std::fs::write(&first, "first\n").expect("first source");
        let root_uri = Url::from_directory_path(&root.0).expect("root URI");
        let first_uri = Url::from_file_path(&first).expect("first URI");
        let second_uri = Url::from_file_path(&second).expect("second URI");
        let mut resources = WorkspaceResources::default();
        resources.load_roots(&[root_uri]).expect("load workspace");

        std::fs::remove_file(first).expect("remove first source");
        resources.remove_disk(&first_uri);
        std::fs::write(&second, "new\n").expect("second source");
        resources
            .reload_file(second_uri.clone())
            .expect("released read and retention charge");

        assert_eq!(
            resources
                .get(&second_uri)
                .expect("reloaded resource")
                .text()
                .as_ref(),
            "new\n"
        );
    }

    #[test]
    fn analysis_snapshot_uses_the_root_documents_nearest_plan() {
        let root = TestDirectory::new();
        write_resource_config(&root.0, 1, 16, 16, true);
        let root_path = root.0.join("root.adoc");
        std::fs::write(&root_path, "root\n").expect("root source");
        let nested = root.0.join("nested");
        std::fs::create_dir(&nested).expect("nested directory");
        write_resource_config(&nested, 1, 16, 16, false);
        std::fs::write(nested.join("child.adoc"), "child\n").expect("child source");
        let root_uri = Url::from_directory_path(&root.0).expect("root URI");
        let document_uri = Url::from_file_path(&root_path).expect("document URI");
        let mut resources = WorkspaceResources::default();
        resources.load_roots(&[root_uri]).expect("load workspace");

        let error = resources
            .input(&document_uri)
            .expect_err("root snapshot count limit");

        assert!(error.contains("analysis snapshot"), "{error}");
    }
}
