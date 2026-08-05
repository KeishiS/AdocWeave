//! LSP URI and filesystem adapter for the runtime-independent workspace.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

#[cfg(test)]
use adocweave::NeverCancel;
use adocweave::preprocess::{
    PreprocessErrorKind, PreprocessOptions, ProjectionLimits, ResourceDocument, ResourceSnapshot,
    SafeMode, preprocess,
};
use adocweave::{CancellationCheck, SourceId};
use adocweave_host::{
    FilesystemReadRollback, LocalFilesystemPolicy, LocalFilesystemSession, LogicalSourceId,
};
use adocweave_workspace::{
    Generation, ResourceId, RetainedLayerCharge, RetainedResourceBudget, RetainedResourceLimits,
    Revision, Workspace, WorkspaceAnalysis, WorkspaceLimits, WorkspaceSnapshot,
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

use adocweave_config::ProjectScopeId;

#[derive(Debug)]
enum ScopeConfigError {
    Config(adocweave_config::ConfigError),
    Other(String),
}

impl ScopeConfigError {
    fn preserves_previous(&self) -> bool {
        matches!(
            self,
            Self::Config(error)
                if error.code == adocweave_config::ConfigErrorCode::ReadFailed
        )
    }
}

impl std::fmt::Display for ScopeConfigError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Config(error) => error.fmt(formatter),
            Self::Other(error) => formatter.write_str(error),
        }
    }
}

/// A completed read of the workspace roots, not yet installed.
///
/// Produced by [`WorkspaceResources::load_roots_detached`] and consumed by
/// [`WorkspaceResources::apply_loaded_roots`].
#[derive(Clone, Debug)]
pub struct LoadedRoots {
    replacement: WorkspaceResources,
    error: Option<String>,
}

#[derive(Clone, Debug, Default)]
pub struct WorkspaceResources {
    inner: Workspace,
    roots: Vec<PathBuf>,
    directory_roots: Vec<PathBuf>,
    single_file_roots: BTreeSet<PathBuf>,
    filesystems: BTreeMap<ProjectScopeId, Arc<Mutex<LocalFilesystemSession>>>,
    project_plans: BTreeMap<ProjectScopeId, adocweave_config::ResolvedResourceLimitPlan>,
    resource_projects: BTreeMap<ResourceId, ProjectScopeId>,
    retained_layers: BTreeMap<ProjectScopeId, RetainedResourceBudget>,
    /// Project files already discovered and parsed, keyed by the directory the
    /// search started from.
    ///
    /// Resolving a document's configuration walks up to the workspace root,
    /// then canonicalizes, reads, hashes and parses the project file. Without
    /// this the work repeats on every keystroke, on the thread that answers
    /// every other request. Discovery depends only on the directory and the
    /// roots, so the directory is a complete key while the roots hold still.
    config_cache: BTreeMap<PathBuf, Option<adocweave_config::ConfigSnapshot>>,
    next_disk_version: i64,
    last_load_failed_closed: bool,
}

struct PreparedWorkspaceRead {
    text: Arc<str>,
    filesystem: Arc<Mutex<LocalFilesystemSession>>,
    rollback: FilesystemReadRollback,
}

impl PreparedWorkspaceRead {
    fn rollback(self) -> Result<(), String> {
        self.filesystem
            .lock()
            .map_err(|_| "workspace resource session lock is poisoned".to_owned())?
            .rollback_reread(self.rollback)
            .map_err(|error| error.to_string())?;
        Ok(())
    }
}

impl WorkspaceResources {
    #[cfg(test)]
    pub fn load_roots(&mut self, roots: &[Url]) -> Result<(), String> {
        self.load_roots_with_limits(roots, adapter_managed_workspace_limits(), &NeverCancel)
    }

    #[cfg(test)]
    pub fn reload_roots_with_open_sources(
        &mut self,
        roots: &[Url],
        open_sources: &[(Url, i64, Arc<str>)],
    ) -> Result<(), String> {
        self.reload_roots_with_open_sources_after_load(roots, open_sources, || {})
    }

    /// Reads the roots into a detached copy of this state.
    ///
    /// Walking the roots and reading every `.adoc` below them takes time
    /// proportional to the size of the workspace. Separating it lets a caller
    /// run it away from the thread that answers requests. The result holds no
    /// borrow of this state, and applying it is a separate, cheap step.
    #[cfg(test)]
    pub fn load_roots_detached(&self, roots: &[Url]) -> LoadedRoots {
        self.load_roots_detached_with_cancellation(roots, &NeverCancel)
    }

    /// Reads roots into a detached copy and stops promptly when superseded.
    pub fn load_roots_detached_with_cancellation(
        &self,
        roots: &[Url],
        cancellation: &dyn CancellationCheck,
    ) -> LoadedRoots {
        let mut replacement = self.clone();
        let error = replacement
            .load_roots_with_limits(roots, adapter_managed_workspace_limits(), cancellation)
            .err();
        LoadedRoots { replacement, error }
    }

    /// Installs a completed read and overlays the documents open right now.
    ///
    /// The open documents are read here rather than when the walk started, so
    /// a document opened while the walk was running is not lost.
    pub fn apply_loaded_roots(
        &mut self,
        loaded: LoadedRoots,
        open_sources: &[(Url, i64, Arc<str>)],
    ) -> Result<(), String> {
        let LoadedRoots { replacement, error } = loaded;
        if let Some(error) = error {
            if replacement.last_load_failed_closed {
                *self = replacement;
            } else {
                self.last_load_failed_closed = false;
            }
            return Err(error);
        }
        self.overlay_open_sources(replacement, open_sources)
    }

    #[cfg(test)]
    fn reload_roots_with_open_sources_after_load(
        &mut self,
        roots: &[Url],
        open_sources: &[(Url, i64, Arc<str>)],
        after_load: impl FnOnce(),
    ) -> Result<(), String> {
        let loaded = self.load_roots_detached(roots);
        if loaded.error.is_some() {
            return self.apply_loaded_roots(loaded, open_sources);
        }
        after_load();
        self.apply_loaded_roots(loaded, open_sources)
    }

    fn overlay_open_sources(
        &mut self,
        mut replacement: Self,
        open_sources: &[(Url, i64, Arc<str>)],
    ) -> Result<(), String> {
        for (uri, version, source) in open_sources {
            let scope_and_plan = match replacement.open_scope_and_plan(uri) {
                Ok(scope_and_plan) => scope_and_plan,
                Err(error) => {
                    let preserve_previous = error.preserves_previous();
                    let error = error.to_string();
                    if preserve_previous {
                        self.last_load_failed_closed = false;
                    } else {
                        replacement.fail_closed(
                            replacement.roots.clone(),
                            adapter_managed_workspace_limits(),
                        );
                        *self = replacement;
                    }
                    return Err(error);
                }
            };
            if let Some((scope, plan)) = scope_and_plan
                && let Err(error) = replacement.upsert_open_with_plan(
                    uri.clone(),
                    *version,
                    Arc::clone(source),
                    scope,
                    plan,
                )
            {
                replacement.fail_closed(
                    replacement.roots.clone(),
                    adapter_managed_workspace_limits(),
                );
                *self = replacement;
                return Err(error);
            }
        }
        *self = replacement;
        Ok(())
    }

    fn load_roots_with_limits(
        &mut self,
        roots: &[Url],
        limits: WorkspaceLimits,
        cancellation: &dyn CancellationCheck,
    ) -> Result<(), String> {
        self.last_load_failed_closed = false;
        // A reload is the only way the roots or a project file can change, so it
        // is also the only point at which a remembered configuration can go
        // stale.
        self.forget_configs();
        let seed = Generation::new(self.inner.generation().get().saturating_add(1));
        let root_paths = match roots
            .iter()
            .map(|root| {
                root.to_file_path()
                    .map_err(|()| format!("workspace root is not a file URI: {root}"))?
                    .canonicalize()
                    .map_err(|error| format!("cannot canonicalize workspace root: {error}"))
            })
            .collect::<Result<Vec<_>, _>>()
        {
            Ok(paths) => paths,
            Err(error) => {
                self.fail_closed(Vec::new(), limits);
                return Err(error);
            }
        };
        let mut directory_roots = Vec::new();
        let mut single_file_roots = BTreeSet::new();
        for path in root_paths {
            if path.is_dir() {
                directory_roots.push(path);
            } else if path.is_file() {
                single_file_roots.insert(path);
            } else {
                self.fail_closed(Vec::new(), limits);
                return Err("workspace root is neither a directory nor a regular file".to_owned());
            }
        }
        directory_roots.sort();
        directory_roots.dedup();
        single_file_roots.retain(|path| {
            !directory_roots
                .iter()
                .any(|directory| path.starts_with(directory))
        });
        let mut paths = directory_roots.clone();
        paths.extend(
            single_file_roots
                .iter()
                .filter_map(|path| path.parent().map(Path::to_owned)),
        );
        paths.sort();
        paths.dedup();
        let preserve_previous = std::cell::Cell::new(false);
        let load_result = (|| {
            let scan_settings = directory_roots
                .iter()
                .map(|root| {
                    let snapshot =
                        adocweave_config::discover_and_load(root, root).map_err(|error| {
                            preserve_previous
                                .set(error.code == adocweave_config::ConfigErrorCode::ReadFailed);
                            error.to_string()
                        })?;
                    Ok((
                        root.clone(),
                        snapshot.map_or_else(
                            adocweave_config::WorkspaceScanSettings::default,
                            |snapshot| snapshot.config.workspace.scan,
                        ),
                    ))
                })
                .collect::<Result<BTreeMap<_, _>, String>>()?;
            let discovery = (!directory_roots.is_empty())
                .then(|| {
                    LocalFilesystemPolicy::new(
                        directory_roots.clone(),
                        adocweave_host::FilesystemReadLimits::default(),
                    )
                })
                .transpose()
                .map_err(|error| error.to_string())?
                .map(|policy| policy.session())
                .transpose()
                .map_err(|error| error.to_string())?;
            let mut candidates = match discovery.as_ref() {
                Some(session) => session
                    .discover_adoc_paths_with_control(
                        |root, relative| {
                            let directory = root.join(relative);
                            let is_nested_workspace_root = directory != root
                                && directory_roots.binary_search(&directory).is_ok();
                            is_nested_workspace_root
                                || scan_settings
                                    .get(root)
                                    .is_some_and(|settings| settings.excludes(relative))
                        },
                        || cancellation.is_cancelled(),
                    )
                    .map_err(|error| error.to_string())?,
                None => Vec::new(),
            };
            candidates.extend(single_file_roots.iter().cloned());
            candidates.sort();
            candidates.dedup();
            let mut inner = Workspace::new_at_generation(limits, seed);
            let mut filesystems = BTreeMap::new();
            let mut resource_projects = BTreeMap::new();
            let mut project_plans = BTreeMap::new();
            let mut retained_layers: BTreeMap<ProjectScopeId, RetainedResourceBudget> =
                BTreeMap::new();
            let mut analysis_root_paths = Vec::new();
            let mut next_disk_version = self.next_disk_version;
            for path in candidates {
                if cancellation.is_cancelled() {
                    return Err("workspace scan was cancelled".to_owned());
                }
                let config = match config_for_path_typed(&paths, &path) {
                    Ok(config) => config,
                    Err(error) => {
                        preserve_previous
                            .set(error.code == adocweave_config::ConfigErrorCode::ReadFailed);
                        return Err(error.to_string());
                    }
                };
                let workspace_root = paths
                    .iter()
                    .filter(|root| path.starts_with(root))
                    .max_by_key(|root| root.components().count())
                    .cloned()
                    .expect("discovered resource belongs to a canonical workspace root");
                let scope = ProjectScopeId {
                    workspace_root,
                    config_path: config.as_ref().map(|snapshot| snapshot.path.clone()),
                };
                if !resource_path_is_allowed(config.as_ref(), &path) {
                    continue;
                }
                let plan = config.as_ref().map_or_else(
                    adocweave_config::ResolvedResourceLimitPlan::default,
                    |snapshot| snapshot.config.resources.limit_plan,
                );
                if let Some(previous) = project_plans.insert(scope.clone(), plan)
                    && previous != plan
                {
                    return Err(
                        "project resource limit plan changed during workspace scan".to_owned()
                    );
                }
                let filesystem = match filesystems.entry(scope.clone()) {
                    std::collections::btree_map::Entry::Occupied(entry) => entry.into_mut(),
                    std::collections::btree_map::Entry::Vacant(entry) => {
                        let session = LocalFilesystemPolicy::new(
                            [scope.workspace_root.clone()],
                            plan.filesystem_reads,
                        )
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
                retained_layers
                    .entry(scope.clone())
                    .or_default()
                    .try_replace_layers(
                        id.clone(),
                        RetainedLayerCharge::new(Some(text.len() as u64), None),
                        plan.retained_layers,
                    )
                    .map_err(|error| error.to_string())?;
                inner
                    .upsert_disk(id.clone(), Revision::new(next_disk_version), text)
                    .map_err(|error| error.to_string())?;
                if path_is_analysis_root(&path, &directory_roots, &single_file_roots) {
                    inner
                        .register_root(id.clone())
                        .map_err(|error| error.to_string())?;
                    if directory_roots.iter().any(|root| path.starts_with(root)) {
                        analysis_root_paths.push(path.clone());
                    }
                }
                resource_projects.insert(id, scope);
            }
            self.inner = inner;
            self.roots = paths.clone();
            self.directory_roots = directory_roots;
            self.single_file_roots = single_file_roots;
            self.filesystems = filesystems;
            self.project_plans = project_plans;
            self.resource_projects = resource_projects;
            self.retained_layers = retained_layers;
            self.next_disk_version = next_disk_version;
            for path in analysis_root_paths {
                let uri = Url::from_file_path(&path).map_err(|()| {
                    format!("cannot convert workspace path to URI: {}", path.display())
                })?;
                self.preload_include_closure(&uri, cancellation)?;
            }
            Ok(())
        })();
        if let Err(error) = load_result {
            if !preserve_previous.get() {
                self.fail_closed(paths, limits);
            }
            return Err(error);
        }
        Ok(())
    }

    fn fail_closed(&mut self, roots: Vec<PathBuf>, limits: WorkspaceLimits) {
        let seed = Generation::new(self.inner.generation().get().saturating_add(1));
        self.inner = Workspace::new_at_generation(limits, seed);
        self.roots = roots;
        self.directory_roots.clear();
        self.single_file_roots.clear();
        self.filesystems.clear();
        self.project_plans.clear();
        self.resource_projects.clear();
        self.retained_layers.clear();
        self.last_load_failed_closed = true;
    }

    pub(crate) const fn last_load_failed_closed(&self) -> bool {
        self.last_load_failed_closed
    }

    /// Returns the effective text held for one resource, if it is known.
    #[cfg(test)]
    pub(crate) fn resource_text(&self, uri: &Url) -> Option<Arc<str>> {
        let id = uri_id(uri).ok()?;
        self.inner
            .get(&id)
            .map(|resource| Arc::clone(resource.text()))
    }

    /// Returns how many resources the workspace holds.
    #[cfg(test)]
    pub(crate) fn resource_count(&self) -> usize {
        self.inner.snapshot().resources().count()
    }

    pub fn reload_file(&mut self, uri: Url) -> Result<BTreeSet<String>, String> {
        let path = uri
            .to_file_path()
            .map_err(|()| format!("workspace resource is not a file URI: {uri}"))?;
        let id = uri_id(&uri)?;
        let admitted_path = path.canonicalize().unwrap_or_else(|_| path.clone());
        if !self.path_is_analysis_root(&admitted_path) {
            return Ok(BTreeSet::new());
        }
        let (scope, config) = scope_and_config_for_path_typed(&self.roots, &admitted_path)
            .map_err(|error| error.to_string())?;
        if !resource_path_is_allowed(config.as_ref(), &admitted_path) {
            return self.remove_outside_authority(&id, &admitted_path);
        }
        let plan = config.as_ref().map_or_else(
            adocweave_config::ResolvedResourceLimitPlan::default,
            |snapshot| snapshot.config.resources.limit_plan,
        );
        let prepared = self.read_workspace_file(&admitted_path, &scope, plan)?;
        let next_disk_version = self.next_disk_version.saturating_add(1);
        let result = (|| {
            let previous_charge = self.retained_charge(&id);
            let charge = RetainedLayerCharge::new(
                Some(prepared.text.len() as u64),
                previous_charge.overlay_bytes(),
            );
            let retained_layers =
                self.move_retained_charge(&id, &scope, charge, plan.retained_layers)?;
            let mut inner = self.inner.clone();
            let affected = inner
                .upsert_disk(
                    id.clone(),
                    Revision::new(next_disk_version),
                    Arc::clone(&prepared.text),
                )
                .map_err(|error| error.to_string())?;
            if !inner.roots().contains(&id) {
                inner
                    .register_root(id.clone())
                    .map_err(|error| error.to_string())?;
            }
            Ok((retained_layers, inner, affected))
        })();
        let (retained_layers, inner, affected) = match result {
            Ok(committed) => committed,
            Err(error) => {
                prepared.rollback()?;
                return Err(error);
            }
        };
        let previous_scope = self.resource_projects.get(&id).cloned();
        if previous_scope
            .as_ref()
            .is_some_and(|previous| previous != &scope)
            && let Err(error) =
                self.release_filesystem_charge(previous_scope.as_ref(), &admitted_path)
        {
            prepared
                .rollback()
                .map_err(|rollback| format!("{error}; rollback failed: {rollback}"))?;
            return Err(error);
        }
        self.inner = inner;
        self.retained_layers = retained_layers;
        self.filesystems
            .insert(scope.clone(), Arc::clone(&prepared.filesystem));
        self.project_plans.insert(scope.clone(), plan);
        self.resource_projects.insert(id, scope);
        self.next_disk_version = next_disk_version;
        self.gc_scopes();
        Ok(strings(affected))
    }

    fn remove_outside_authority(
        &mut self,
        id: &ResourceId,
        path: &Path,
    ) -> Result<BTreeSet<String>, String> {
        let Some(scope) = self.resource_projects.get(id).cloned() else {
            return Ok(BTreeSet::new());
        };
        let mut inner = self.inner.clone();
        inner.unregister_root(id);
        let mut affected = inner.close_overlay(id).map_err(|error| error.to_string())?;
        affected.extend(inner.remove_disk(id));
        let mut retained_layers = self.retained_layers.clone();
        let budget = retained_layers
            .get(&scope)
            .cloned()
            .unwrap_or_default()
            .without_resource(id);
        retained_layers.insert(scope.clone(), budget);
        self.release_filesystem_charge(Some(&scope), path)?;
        self.inner = inner;
        self.retained_layers = retained_layers;
        self.resource_projects.remove(id);
        self.gc_scopes();
        let mut affected = strings(affected);
        affected.insert(id.to_string());
        Ok(affected)
    }

    fn read_workspace_file(
        &self,
        path: &Path,
        scope: &ProjectScopeId,
        plan: adocweave_config::ResolvedResourceLimitPlan,
    ) -> Result<PreparedWorkspaceRead, String> {
        if path.extension().and_then(|value| value.to_str()) != Some("adoc") {
            return Err(format!(
                "workspace resource is not an .adoc file: {}",
                path.display()
            ));
        }
        if let Some(previous) = self.project_plans.get(scope)
            && previous != &plan
        {
            return Err(
                "workspace resource limit plan changed; a full reload is required".to_owned(),
            );
        }
        let filesystem = if let Some(filesystem) = self.filesystems.get(scope) {
            Arc::clone(filesystem)
        } else {
            let session =
                LocalFilesystemPolicy::new([scope.workspace_root.clone()], plan.filesystem_reads)
                    .map_err(|error| error.to_string())?
                    .session()
                    .map_err(|error| error.to_string())?;
            Arc::new(Mutex::new(session))
        };
        let (loaded, rollback) = filesystem
            .lock()
            .map_err(|_| "workspace resource session lock is poisoned".to_owned())?
            .reread_utf8_with_rollback(
                LogicalSourceId::new(path.to_string_lossy().into_owned())
                    .map_err(|error| error.to_string())?,
                path,
            )
            .map_err(|error| error.to_string())?;
        Ok(PreparedWorkspaceRead {
            text: loaded.into_parts().1,
            filesystem,
            rollback,
        })
    }

    fn retained_charge(&self, id: &ResourceId) -> RetainedLayerCharge {
        self.resource_projects
            .get(id)
            .and_then(|scope| self.retained_layers.get(scope))
            .map_or_else(RetainedLayerCharge::default, |budget| budget.charge(id))
    }

    fn move_retained_charge(
        &self,
        id: &ResourceId,
        scope: &ProjectScopeId,
        charge: RetainedLayerCharge,
        limits: RetainedResourceLimits,
    ) -> Result<BTreeMap<ProjectScopeId, RetainedResourceBudget>, String> {
        let mut retained_layers = self.retained_layers.clone();
        if let Some(previous_scope) = self.resource_projects.get(id)
            && previous_scope != scope
        {
            let previous = retained_layers
                .get(previous_scope)
                .cloned()
                .unwrap_or_default()
                .without_resource(id);
            retained_layers.insert(previous_scope.clone(), previous);
        }
        let replacement = retained_layers
            .get(scope)
            .cloned()
            .unwrap_or_default()
            .with_layers(id.clone(), charge, limits)
            .map_err(|error| error.to_string())?;
        retained_layers.insert(scope.clone(), replacement);
        Ok(retained_layers)
    }

    fn release_filesystem_charge(
        &self,
        scope: Option<&ProjectScopeId>,
        path: &Path,
    ) -> Result<(), String> {
        let Some(filesystem) = scope.and_then(|scope| self.filesystems.get(scope)) else {
            return Ok(());
        };
        filesystem
            .lock()
            .map_err(|_| "workspace resource session lock is poisoned".to_owned())?
            .release(path);
        Ok(())
    }

    fn gc_scopes(&mut self) {
        let retained = self
            .resource_projects
            .values()
            .cloned()
            .collect::<BTreeSet<_>>();
        self.retained_layers
            .retain(|scope, budget| retained.contains(scope) || !budget.is_empty());
        self.project_plans
            .retain(|scope, _| retained.contains(scope));
        self.filesystems.retain(|scope, _| retained.contains(scope));
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
        let Some((scope, plan)) = self
            .open_scope_and_plan(&uri)
            .map_err(|error| error.to_string())?
        else {
            return Err(format!(
                "workspace resource is outside configured resource roots: {uri}"
            ));
        };
        self.upsert_open_with_plan(uri, version, text.into(), scope, plan)
    }

    fn upsert_open_with_plan(
        &mut self,
        uri: Url,
        version: i64,
        text: Arc<str>,
        scope: ProjectScopeId,
        plan: adocweave_config::ResolvedResourceLimitPlan,
    ) -> Result<BTreeSet<String>, String> {
        let id = uri_id(&uri)?;
        let path = uri
            .to_file_path()
            .map_err(|()| format!("workspace resource is not a file URI: {uri}"))?;
        if self
            .project_plans
            .get(&scope)
            .is_some_and(|previous| previous != &plan)
        {
            return Err(
                "workspace resource limit plan changed; a full reload is required".to_owned(),
            );
        }
        let previous_scope = self.resource_projects.get(&id).cloned();
        let previous_charge = self.retained_charge(&id);
        let migrating_disk = previous_scope
            .as_ref()
            .is_some_and(|previous| previous != &scope)
            && previous_charge.disk_bytes().is_some();
        let prepared_disk = migrating_disk
            .then(|| self.read_workspace_file(&path, &scope, plan))
            .transpose()?;
        let next_disk_version = self
            .next_disk_version
            .saturating_add(i64::from(migrating_disk));
        let result = (|| {
            let charge = RetainedLayerCharge::new(
                prepared_disk
                    .as_ref()
                    .map_or(previous_charge.disk_bytes(), |prepared| {
                        Some(prepared.text.len() as u64)
                    }),
                Some(text.len() as u64),
            );
            let retained_layers =
                self.move_retained_charge(&id, &scope, charge, plan.retained_layers)?;
            let mut inner = self.inner.clone();
            if let Some(prepared) = &prepared_disk {
                inner
                    .upsert_disk(
                        id.clone(),
                        Revision::new(next_disk_version),
                        Arc::clone(&prepared.text),
                    )
                    .map_err(|error| error.to_string())?;
            }
            let affected = inner
                .upsert_overlay(id.clone(), Revision::new(version), Arc::clone(&text))
                .map_err(|error| error.to_string())?;
            if !inner.roots().contains(&id) {
                inner
                    .register_root(id.clone())
                    .map_err(|error| error.to_string())?;
            }
            Ok((retained_layers, inner, affected))
        })();
        let (retained_layers, inner, affected) = match result {
            Ok(committed) => committed,
            Err(error) => {
                if let Some(prepared) = prepared_disk {
                    prepared.rollback()?;
                }
                return Err(error);
            }
        };
        if previous_scope
            .as_ref()
            .is_some_and(|previous| previous != &scope)
            && let Err(error) = self.release_filesystem_charge(previous_scope.as_ref(), &path)
        {
            if let Some(prepared) = prepared_disk {
                prepared
                    .rollback()
                    .map_err(|rollback| format!("{error}; rollback failed: {rollback}"))?;
            }
            return Err(error);
        }
        self.inner = inner;
        self.retained_layers = retained_layers;
        if let Some(prepared) = &prepared_disk {
            self.filesystems
                .insert(scope.clone(), Arc::clone(&prepared.filesystem));
        }
        self.project_plans.insert(scope.clone(), plan);
        self.resource_projects.insert(id.clone(), scope);
        self.next_disk_version = next_disk_version;
        self.gc_scopes();
        let mut affected = strings(affected);
        affected.insert(id.to_string());
        Ok(affected)
    }

    pub fn remove_disk(&mut self, uri: &Url) -> Result<BTreeSet<String>, String> {
        let id = uri_id(uri)?;
        let path = uri
            .to_file_path()
            .map_err(|()| format!("workspace resource is not a file URI: {uri}"))?;
        let scope = self.resource_projects.get(&id).cloned();
        let mut retained_layers = self.retained_layers.clone();
        if let Some(scope) = &scope {
            let plan = self
                .project_plans
                .get(scope)
                .copied()
                .ok_or_else(|| "workspace resource limit plan is missing".to_owned())?;
            let charge = self.retained_charge(&id);
            let budget = retained_layers
                .get(scope)
                .cloned()
                .unwrap_or_default()
                .with_layers(
                    id.clone(),
                    RetainedLayerCharge::new(None, charge.overlay_bytes()),
                    plan.retained_layers,
                )
                .map_err(|error| error.to_string())?;
            retained_layers.insert(scope.clone(), budget);
        }
        let mut inner = self.inner.clone();
        let affected = strings(inner.remove_disk(&id));
        self.release_filesystem_charge(scope.as_ref(), &path)?;
        self.inner = inner;
        self.retained_layers = retained_layers;
        if self.inner.get(&id).is_none() {
            self.resource_projects.remove(&id);
        }
        self.gc_scopes();
        Ok(affected)
    }

    pub fn close_open(&mut self, uri: &Url) -> Result<BTreeSet<String>, String> {
        let id = uri_id(uri)?;
        let mut retained_layers = self.retained_layers.clone();
        if let Some(project) = self.resource_projects.get(&id).cloned() {
            let plan = self
                .project_plans
                .get(&project)
                .copied()
                .ok_or_else(|| "workspace resource limit plan is missing".to_owned())?;
            let budget = retained_layers
                .get(&project)
                .cloned()
                .unwrap_or_default()
                .with_overlay(id.clone(), None, plan.retained_layers)
                .map_err(|error| error.to_string())?;
            retained_layers.insert(project, budget);
        }
        let mut inner = self.inner.clone();
        let affected = inner
            .close_overlay(&id)
            .map_err(|error| error.to_string())?;
        self.inner = inner;
        self.retained_layers = retained_layers;
        if self.inner.get(&id).is_none() {
            self.resource_projects.remove(&id);
        }
        self.gc_scopes();
        Ok(strings(affected))
    }

    fn preload_include_closure(
        &mut self,
        root: &Url,
        cancellation: &dyn CancellationCheck,
    ) -> Result<(), String> {
        let root_id = uri_id(root)?;
        let root_scope = self
            .resource_projects
            .get(&root_id)
            .ok_or_else(|| format!("workspace project scope is missing: {root}"))?
            .clone();
        let config_snapshot = self.config_for_uri(root)?;
        let project_config = config_snapshot.as_ref().map_or_else(
            adocweave_config::ResolvedProjectConfig::default,
            |snapshot| snapshot.config.clone(),
        );
        let mut options = project_config.preprocess.clone();
        if config_snapshot.is_none() {
            options.enable_includes = true;
        }
        if !options.enable_includes {
            return Ok(());
        }
        options.base_uri = parent_uri(root);
        options.safe_mode = SafeMode::Server;
        options.allowed_schemes = BTreeSet::from(["file".to_owned()]);
        options.source_id = Some(SourceId::new(root.to_string()));
        let allowed_roots = configured_include_roots(&project_config, &self.roots)?;
        let mut attempted = BTreeSet::new();
        loop {
            if cancellation.is_cancelled() {
                return Err("workspace scan was cancelled".to_owned());
            }
            let root_text = self
                .inner
                .get(&root_id)
                .ok_or_else(|| format!("workspace resource is missing: {root}"))?
                .text()
                .clone();
            let snapshot = self.preprocess_snapshot(&root_id, &root_scope, &allowed_roots);
            let error = match preprocess(&root_text, &snapshot, &options) {
                Ok(_) => return Ok(()),
                Err(error) if error.kind == PreprocessErrorKind::MissingResource => error,
                Err(_) => return Ok(()),
            };
            let Some(target) = error.target else {
                return Ok(());
            };
            if !attempted.insert(target.clone()) {
                return Ok(());
            }
            let Ok(target_uri) = Url::parse(&target) else {
                return Ok(());
            };
            let Ok(target_path) = target_uri.to_file_path() else {
                return Ok(());
            };
            let Ok(canonical) = target_path.canonicalize() else {
                return Ok(());
            };
            let authority_roots = if allowed_roots.is_empty() {
                std::slice::from_ref(&root_scope.workspace_root)
            } else {
                allowed_roots.as_slice()
            };
            if !authority_roots
                .iter()
                .any(|root| canonical.starts_with(root))
            {
                return Ok(());
            }
            let target_id = uri_id(&target_uri)?;
            if self.inner.get(&target_id).is_some() {
                return Ok(());
            }
            let (scope, config) = scope_and_config_for_path_typed(&self.roots, &canonical)
                .map_err(|error| error.to_string())?;
            if root_scope.config_path.is_none() && scope != root_scope {
                return Ok(());
            }
            if !resource_path_is_allowed(config.as_ref(), &canonical) {
                return Ok(());
            }
            let plan = config.as_ref().map_or_else(
                adocweave_config::ResolvedResourceLimitPlan::default,
                |snapshot| snapshot.config.resources.limit_plan,
            );
            self.insert_include_resource(target_uri, canonical, scope, plan)?;
        }
    }

    fn preprocess_snapshot(
        &self,
        root_id: &ResourceId,
        root_scope: &ProjectScopeId,
        allowed_roots: &[PathBuf],
    ) -> ResourceSnapshot {
        self.inner
            .snapshot()
            .resources()
            .filter(|(id, _)| *id != root_id)
            .filter(|(id, _)| {
                let same_scope = self.resource_projects.get(*id).is_some_and(|scope| {
                    scope.workspace_root == root_scope.workspace_root
                        && (root_scope.config_path.is_some() || scope == root_scope)
                });
                same_scope
                    && (allowed_roots.is_empty()
                        || Url::parse(id.as_str())
                            .ok()
                            .and_then(|uri| uri.to_file_path().ok())
                            .is_some_and(|path| {
                                allowed_roots.iter().any(|root| path.starts_with(root))
                            }))
            })
            .map(|(id, resource)| {
                (
                    id.to_string(),
                    ResourceDocument {
                        source_id: SourceId::new(id.to_string()),
                        source: Arc::clone(resource.text()),
                    },
                )
            })
            .collect()
    }

    fn insert_include_resource(
        &mut self,
        uri: Url,
        path: PathBuf,
        scope: ProjectScopeId,
        plan: adocweave_config::ResolvedResourceLimitPlan,
    ) -> Result<(), String> {
        let id = uri_id(&uri)?;
        let prepared = self.read_workspace_file(&path, &scope, plan)?;
        let next_disk_version = self.next_disk_version.saturating_add(1);
        let result = (|| {
            let charge = RetainedLayerCharge::new(Some(prepared.text.len() as u64), None);
            let retained_layers =
                self.move_retained_charge(&id, &scope, charge, plan.retained_layers)?;
            let mut inner = self.inner.clone();
            inner
                .upsert_disk(
                    id.clone(),
                    Revision::new(next_disk_version),
                    Arc::clone(&prepared.text),
                )
                .map_err(|error| error.to_string())?;
            Ok::<_, String>((retained_layers, inner))
        })();
        let (retained_layers, inner) = match result {
            Ok(committed) => committed,
            Err(error) => {
                prepared.rollback()?;
                return Err(error);
            }
        };
        self.inner = inner;
        self.retained_layers = retained_layers;
        self.filesystems
            .insert(scope.clone(), Arc::clone(&prepared.filesystem));
        self.project_plans.insert(scope.clone(), plan);
        self.resource_projects.insert(id, scope);
        self.next_disk_version = next_disk_version;
        Ok(())
    }

    pub fn input(&mut self, root: &Url) -> Result<WorkspaceInput, String> {
        let root_id = uri_id(root)?;
        if self.inner.get(&root_id).is_none() {
            return Err(format!("workspace resource is missing: {root}"));
        }
        let root_scope = self
            .resource_projects
            .get(&root_id)
            .ok_or_else(|| format!("workspace project scope is missing: {root}"))?
            .clone();
        let root_scope = &root_scope;
        let mut allowed_schemes = BTreeSet::new();
        allowed_schemes.insert("file".to_owned());
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
            configured_include_roots(&project_config, &self.roots)?
        } else {
            Vec::new()
        };
        let limits = project_config.resources.limit_plan.analysis_snapshot;
        let mut budget = adocweave_config::AnalysisSnapshotBudget::new(limits);
        let snapshot = self.inner.try_snapshot_resources(|id, resource| {
            let resource_scope = self.resource_projects.get(id);
            let same_scope = resource_scope.is_some_and(|scope| {
                scope.workspace_root == root_scope.workspace_root
                    && (root_scope.config_path.is_some() || scope == root_scope)
            });
            let allowed = if !same_scope {
                false
            } else if id == &root_id {
                true
            } else if !options.enable_includes {
                false
            } else if allowed_roots.is_empty() {
                true
            } else {
                Url::parse(id.as_str())
                    .ok()
                    .and_then(|uri| uri.to_file_path().ok())
                    .is_some_and(|path| allowed_roots.iter().any(|root| path.starts_with(root)))
            };
            if !allowed {
                return Ok::<bool, String>(false);
            }
            budget
                .charge(resource.text().len() as u64)
                .map_err(|error| error.to_string())?;
            Ok::<bool, String>(true)
        })?;
        Ok(WorkspaceInput {
            generation: snapshot.generation(),
            root: root_id,
            snapshot,
            options,
            config_sha256: config_snapshot.map(|snapshot| snapshot.content_sha256),
            project_config,
        })
    }

    pub fn input_is_current(&mut self, input: &WorkspaceInput) -> bool {
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
        &mut self,
        id: &ResourceId,
    ) -> Result<Option<adocweave_config::ConfigSnapshot>, String> {
        let uri = Url::parse(id.as_str()).map_err(|error| error.to_string())?;
        self.config_for_uri(&uri)
    }

    fn config_for_uri(
        &mut self,
        uri: &Url,
    ) -> Result<Option<adocweave_config::ConfigSnapshot>, String> {
        let path = uri
            .to_file_path()
            .map_err(|()| format!("workspace resource is not a file URI: {uri}"))?;
        self.cached_config_for_path(&path)
            .map_err(|error| error.to_string())
    }

    /// Resolves a path's project file, reading it at most once per directory.
    fn cached_config_for_path(
        &mut self,
        path: &Path,
    ) -> Result<Option<adocweave_config::ConfigSnapshot>, adocweave_config::ConfigError> {
        let Some(start) = existing_ancestor(path) else {
            return Ok(None);
        };
        if let Some(cached) = self.config_cache.get(&start) {
            return Ok(cached.clone());
        }
        let config = config_for_path_typed(&self.roots, path)?;
        self.config_cache.insert(start, config.clone());
        Ok(config)
    }

    /// Forgets every remembered project file.
    ///
    /// Called when a project file or the set of roots changes. A snapshot found
    /// for one directory can come from an ancestor, so a single edited file can
    /// invalidate entries recorded under many directories; clearing all of them
    /// keeps the cache from ever answering with a stale configuration.
    fn forget_configs(&mut self) {
        self.config_cache.clear();
    }

    fn open_scope_and_plan(
        &self,
        uri: &Url,
    ) -> Result<
        Option<(ProjectScopeId, adocweave_config::ResolvedResourceLimitPlan)>,
        ScopeConfigError,
    > {
        let path = uri.to_file_path().map_err(|()| {
            ScopeConfigError::Other(format!("workspace resource is not a file URI: {uri}"))
        })?;
        let admission_path = path.canonicalize().unwrap_or_else(|_| path.clone());
        if !self.path_is_analysis_root(&admission_path) {
            return Ok(None);
        }
        let (scope, config) = scope_and_config_for_path_typed(&self.roots, &admission_path)?;
        if !resource_path_is_allowed(config.as_ref(), &admission_path) {
            return Ok(None);
        }
        let plan = config.as_ref().map_or_else(
            adocweave_config::ResolvedResourceLimitPlan::default,
            |snapshot| snapshot.config.resources.limit_plan,
        );
        Ok(Some((scope, plan)))
    }

    fn path_is_analysis_root(&self, path: &Path) -> bool {
        path_is_analysis_root(path, &self.directory_roots, &self.single_file_roots)
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

fn path_is_analysis_root(
    path: &Path,
    directory_roots: &[PathBuf],
    single_file_roots: &BTreeSet<PathBuf>,
) -> bool {
    (directory_roots.is_empty() && single_file_roots.is_empty())
        || single_file_roots.contains(path)
        || directory_roots.iter().any(|root| path.starts_with(root))
}

fn resource_path_is_allowed(
    config: Option<&adocweave_config::ConfigSnapshot>,
    path: &Path,
) -> bool {
    config.is_none_or(|snapshot| {
        snapshot.config.resources.roots.is_empty()
            || snapshot
                .config
                .resources
                .roots
                .iter()
                .any(|root| path.starts_with(root))
    })
}

fn configured_include_roots(
    config: &adocweave_config::ResolvedProjectConfig,
    workspace_roots: &[PathBuf],
) -> Result<Vec<PathBuf>, String> {
    config
        .resources
        .roots
        .iter()
        .map(|root| {
            let canonical = root
                .canonicalize()
                .map_err(|error| format!("cannot canonicalize configured root: {error}"))?;
            if !workspace_roots
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
        .collect()
}

/// Walks up to the nearest directory that exists.
///
/// A document being created does not exist on disk yet, and configuration
/// discovery has to start somewhere real.
fn existing_ancestor(path: &Path) -> Option<PathBuf> {
    let mut start = path;
    while !start.exists() {
        start = start.parent()?;
    }
    Some(start.to_owned())
}

fn config_for_path_typed(
    roots: &[PathBuf],
    path: &Path,
) -> Result<Option<adocweave_config::ConfigSnapshot>, adocweave_config::ConfigError> {
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
    adocweave_config::discover_and_load(start, boundary)
}

fn scope_and_config_for_path_typed(
    roots: &[PathBuf],
    path: &Path,
) -> Result<(ProjectScopeId, Option<adocweave_config::ConfigSnapshot>), ScopeConfigError> {
    let boundary = roots
        .iter()
        .filter(|root| path.starts_with(root))
        .max_by_key(|root| root.components().count());
    let Some(boundary) = boundary else {
        if roots.is_empty() {
            return Ok((
                ProjectScopeId {
                    workspace_root: path.parent().unwrap_or_else(|| Path::new("")).to_owned(),
                    config_path: None,
                },
                None,
            ));
        }
        return Err(ScopeConfigError::Other(
            "workspace resource is outside every workspace root".to_owned(),
        ));
    };
    let mut start = path;
    while !start.exists() {
        let Some(parent) = start.parent() else {
            return Ok((
                ProjectScopeId {
                    workspace_root: boundary.clone(),
                    config_path: None,
                },
                None,
            ));
        };
        start = parent;
    }
    let config =
        adocweave_config::discover_and_load(start, boundary).map_err(ScopeConfigError::Config)?;
    Ok((
        ProjectScopeId {
            workspace_root: boundary.clone(),
            config_path: config.as_ref().map(|snapshot| snapshot.path.clone()),
        },
        config,
    ))
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
    fn a_project_file_is_read_once_per_directory_and_forgotten_when_it_changes() {
        let root = TestDirectory::new();
        let source = root.0.join("a.adoc");
        std::fs::write(&source, "first\n").expect("source");
        write_resource_config(&root.0, 8, 4096, 4096, true);
        let root_uri = Url::from_directory_path(&root.0).expect("root URI");
        let source_uri = Url::from_file_path(&source).expect("source URI");
        let mut resources = WorkspaceResources::default();
        resources
            .load_roots(std::slice::from_ref(&root_uri))
            .expect("load workspace");

        let first = resources.input(&source_uri).expect("workspace input");
        assert_eq!(resources.config_cache.len(), 1);

        // Replacing the file on disk without reloading must not change the
        // answer: repeated keystrokes read the remembered configuration.
        write_resource_config(&root.0, 4, 2048, 2048, true);
        let repeated = resources.input(&source_uri).expect("workspace input");
        assert_eq!(repeated.config_sha256, first.config_sha256);

        // A reload is what tells the server the project file may have changed.
        resources.load_roots(&[root_uri]).expect("reload workspace");
        let reloaded = resources.input(&source_uri).expect("workspace input");
        assert_ne!(reloaded.config_sha256, first.config_sha256);
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
    fn scan_exclusion_keeps_included_resource_out_of_the_analysis_root_set() {
        let root = TestDirectory::new();
        let generated = root.0.join("nested/generated");
        std::fs::create_dir_all(&generated).expect("generated directory");
        std::fs::write(
            root.0.join(adocweave_config::FILE_NAME),
            concat!(
                "schema-version = 1\n",
                "[resources]\ninclude = true\nroots = [\".\"]\n",
                "[workspace.scan]\nexclude = [\"**/generated\"]\n",
            ),
        )
        .expect("project configuration");
        let source = root.0.join("root.adoc");
        let included = generated.join("part.adoc");
        std::fs::write(&source, "include::nested/generated/part.adoc[]\n").expect("source");
        std::fs::write(&included, "included\n").expect("included source");
        let root_uri = Url::from_directory_path(&root.0).expect("root URI");
        let source_uri = Url::from_file_path(&source).expect("source URI");
        let included_uri = Url::from_file_path(&included).expect("included URI");
        let mut resources = WorkspaceResources::default();

        resources.load_roots(&[root_uri]).expect("load workspace");

        assert_eq!(
            resources.inner.roots(),
            &BTreeSet::from([uri_id(&source_uri).expect("source ID")])
        );
        assert!(resources.get(&included_uri).is_some());
        let input = resources.input(&source_uri).expect("workspace input");
        let analysis = input
            .analyze(
                &adocweave::AnalysisOptions::default(),
                &adocweave::CancellationToken::new(),
            )
            .expect("workspace analysis");
        assert!(analysis.analysis.source().contains("included"));
    }

    #[test]
    fn each_directory_root_uses_only_its_own_scan_patterns() {
        let first = TestDirectory::new();
        let second = TestDirectory::new();
        for (root, excluded, retained) in [
            (&first, "first-only", "second-only"),
            (&second, "second-only", "first-only"),
        ] {
            std::fs::write(
                root.0.join(adocweave_config::FILE_NAME),
                format!("schema-version = 1\n[workspace.scan]\nexclude = [\"{excluded}\"]\n"),
            )
            .expect("project configuration");
            std::fs::create_dir(root.0.join(excluded)).expect("excluded directory");
            std::fs::create_dir(root.0.join(retained)).expect("retained directory");
            std::fs::write(root.0.join(excluded).join("hidden.adoc"), "hidden\n")
                .expect("excluded source");
            std::fs::write(root.0.join(retained).join("kept.adoc"), "kept\n")
                .expect("retained source");
        }
        let roots =
            [&first, &second].map(|root| Url::from_directory_path(&root.0).expect("root URI"));
        let expected = BTreeSet::from([
            uri_id(
                &Url::from_file_path(first.0.join("second-only/kept.adoc"))
                    .expect("first retained URI"),
            )
            .expect("first retained ID"),
            uri_id(
                &Url::from_file_path(second.0.join("first-only/kept.adoc"))
                    .expect("second retained URI"),
            )
            .expect("second retained ID"),
        ]);
        let mut resources = WorkspaceResources::default();

        resources.load_roots(&roots).expect("load workspaces");

        assert_eq!(resources.inner.roots(), &expected);
    }

    #[test]
    fn nested_directory_root_applies_its_own_scan_patterns() {
        let outer = TestDirectory::new();
        let inner = outer.0.join("nested");
        let excluded = inner.join("generated");
        std::fs::create_dir_all(&excluded).expect("excluded directory");
        std::fs::write(
            inner.join(adocweave_config::FILE_NAME),
            "schema-version = 1\n[workspace.scan]\nexclude = [\"generated\"]\n",
        )
        .expect("inner project configuration");
        std::fs::write(excluded.join("hidden.adoc"), "hidden\n").expect("excluded source");
        let roots = [
            Url::from_directory_path(&outer.0).expect("outer root URI"),
            Url::from_directory_path(&inner).expect("inner root URI"),
        ];
        let hidden =
            uri_id(&Url::from_file_path(excluded.join("hidden.adoc")).expect("hidden source URI"))
                .expect("hidden source ID");
        let mut resources = WorkspaceResources::default();

        resources
            .load_roots(&roots)
            .expect("load nested workspaces");

        assert!(!resources.inner.roots().contains(&hidden));
    }

    #[test]
    fn single_file_root_registers_only_the_selected_document() {
        let root = TestDirectory::new();
        let selected = root.0.join("selected.adoc");
        let included = root.0.join("included.adoc");
        let unrelated = root.0.join("unrelated.adoc");
        std::fs::write(&selected, "include::included.adoc[]\n").expect("selected source");
        std::fs::write(&included, "included\n").expect("included source");
        std::fs::write(&unrelated, "unrelated\n").expect("unrelated source");
        let selected_uri = Url::from_file_path(&selected).expect("selected URI");
        let included_uri = Url::from_file_path(&included).expect("included URI");
        let unrelated_uri = Url::from_file_path(&unrelated).expect("unrelated URI");
        let mut resources = WorkspaceResources::default();

        resources
            .load_roots(std::slice::from_ref(&selected_uri))
            .expect("load single-file workspace");

        assert_eq!(
            resources.inner.roots(),
            &BTreeSet::from([uri_id(&selected_uri).expect("selected resource ID")])
        );
        assert!(resources.get(&included_uri).is_none());
        assert!(resources.get(&unrelated_uri).is_none());
        resources
            .reload_file(unrelated_uri.clone())
            .expect("ignore unrelated resource");
        assert!(resources.get(&unrelated_uri).is_none());

        assert!(resources.input(&selected_uri).is_ok());
    }

    #[test]
    fn directory_root_supersedes_a_nested_single_file_root() {
        let root = TestDirectory::new();
        let nested = root.0.join("docs");
        std::fs::create_dir_all(&nested).expect("nested directory");
        let first = nested.join("first.adoc");
        let second = nested.join("second.adoc");
        std::fs::write(&first, "first\n").expect("first source");
        std::fs::write(&second, "second\n").expect("second source");
        let directory_uri = Url::from_directory_path(&root.0).expect("directory URI");
        let first_uri = Url::from_file_path(&first).expect("first URI");
        let mut resources = WorkspaceResources::default();

        resources
            .load_roots(&[directory_uri, first_uri])
            .expect("load mixed roots");

        assert!(resources.single_file_roots.is_empty());
        assert_eq!(
            resources.roots,
            vec![root.0.canonicalize().expect("canonical directory")]
        );
        assert_eq!(resources.inner.roots().len(), 2);
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
            .load_roots_with_limits(&[root_uri], limits, &NeverCancel)
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
            .load_roots_with_limits(&[root_uri], limits, &NeverCancel)
            .expect("load workspace");

        std::fs::remove_file(&first).expect("remove first");
        resources.remove_disk(&first_uri).expect("remove disk");
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
    fn separate_project_sessions_and_retained_budgets_do_not_compete() {
        let root = TestDirectory::new();
        let first = root.0.join("first");
        let second = root.0.join("second");
        std::fs::create_dir(&first).expect("first project");
        std::fs::create_dir(&second).expect("second project");
        write_resource_config(&first, 1, 4, 4, false);
        write_resource_config(&second, 1, 4, 4, false);
        let first_path = first.join("document.adoc");
        let second_path = second.join("document.adoc");
        std::fs::write(&first_path, "one").expect("first source");
        std::fs::write(&second_path, "two").expect("second source");
        let root_uri = Url::from_directory_path(&root.0).expect("root URI");
        let first_uri = Url::from_file_path(first_path).expect("first URI");
        let second_uri = Url::from_file_path(second_path).expect("second URI");
        let mut resources = WorkspaceResources::default();

        resources.load_roots(&[root_uri]).expect("load workspace");

        assert!(resources.get(&first_uri).is_some());
        assert!(resources.get(&second_uri).is_some());
        assert_eq!(resources.filesystems.len(), 2);
        assert_eq!(resources.retained_layers.len(), 2);
    }

    #[test]
    fn unconfigured_workspace_roots_have_independent_scopes() {
        let first = TestDirectory::new();
        let second = TestDirectory::new();
        std::fs::write(first.0.join("document.adoc"), "one").expect("first source");
        std::fs::write(second.0.join("document.adoc"), "two").expect("second source");
        let mut resources = WorkspaceResources::default();

        resources
            .load_roots(&[
                Url::from_directory_path(&first.0).expect("first root"),
                Url::from_directory_path(&second.0).expect("second root"),
            ])
            .expect("load roots");

        assert_eq!(resources.filesystems.len(), 2);
        assert_eq!(resources.retained_layers.len(), 2);
        assert!(
            resources
                .project_plans
                .keys()
                .all(|scope| scope.config_path.is_none())
        );
    }

    #[test]
    fn configless_multi_root_input_excludes_an_include_from_another_scope() {
        let root = TestDirectory::new();
        let second = root.0.join("second");
        std::fs::create_dir(&second).expect("second root");
        let first_path = root.0.join("document.adoc");
        let second_path = second.join("private.adoc");
        std::fs::write(&first_path, "include::second/private.adoc[]\n").expect("first source");
        std::fs::write(&second_path, "private\n").expect("second source");
        let first_uri = Url::from_file_path(&first_path).expect("first URI");
        let second_id = ResourceId::new(
            Url::from_file_path(&second_path)
                .expect("second URI")
                .as_str(),
        )
        .expect("second ID");
        let mut resources = WorkspaceResources::default();
        resources
            .load_roots(&[
                Url::from_directory_path(&root.0).expect("first root URI"),
                Url::from_directory_path(&second).expect("second root URI"),
            ])
            .expect("load roots");

        let input = resources.input(&first_uri).expect("first input");
        assert!(input.options.enable_includes);
        assert_eq!(input.snapshot.resources().count(), 1);
        assert!(input.snapshot.get(&second_id).is_none());
        let error = input
            .analyze(
                &adocweave::AnalysisOptions::default(),
                &adocweave::CancellationToken::new(),
            )
            .expect_err("cross-scope include is unavailable");
        assert_eq!(
            error.code,
            adocweave_workspace::WorkspaceErrorCode::Preprocess
        );
    }

    #[test]
    fn configured_multi_root_without_explicit_roots_excludes_another_workspace_root() {
        let first = TestDirectory::new();
        let second = TestDirectory::new();
        std::fs::write(
            first.0.join(adocweave_config::FILE_NAME),
            "schema-version = 1\n[resources]\ninclude = true\nroots = []\n",
        )
        .expect("first config");
        let first_path = first.0.join("document.adoc");
        let second_path = second.0.join("private.adoc");
        std::fs::write(&first_path, "first").expect("first source");
        std::fs::write(&second_path, "private").expect("second source");
        let first_uri = Url::from_file_path(&first_path).expect("first URI");
        let second_id =
            uri_id(&Url::from_file_path(&second_path).expect("second URI")).expect("second ID");
        let mut resources = WorkspaceResources::default();
        resources
            .load_roots(&[
                Url::from_directory_path(&first.0).expect("first root URI"),
                Url::from_directory_path(&second.0).expect("second root URI"),
            ])
            .expect("load roots");

        let input = resources.input(&first_uri).expect("first input");
        assert!(input.options.enable_includes);
        assert_eq!(input.snapshot.resources().count(), 1);
        assert!(input.snapshot.get(&second_id).is_none());
    }

    #[test]
    fn open_outside_configured_roots_preserves_workspace_and_budgets() {
        let root = TestDirectory::new();
        let docs = root.0.join("docs");
        let other = root.0.join("other");
        std::fs::create_dir(&docs).expect("docs");
        std::fs::create_dir(&other).expect("other");
        std::fs::write(
            root.0.join(adocweave_config::FILE_NAME),
            "schema-version = 1\n[resources]\nroots = [\"docs\"]\n",
        )
        .expect("config");
        let accepted = docs.join("accepted.adoc");
        let rejected = other.join("rejected.adoc");
        std::fs::write(&accepted, "accepted").expect("accepted source");
        std::fs::write(&rejected, "rejected").expect("rejected source");
        let rejected_uri = Url::from_file_path(&rejected).expect("rejected URI");
        let rejected_id = uri_id(&rejected_uri).expect("rejected ID");
        let mut resources = WorkspaceResources::default();
        resources
            .load_roots(&[Url::from_directory_path(&root.0).expect("root URI")])
            .expect("load root");
        let generation = resources.generation();
        let projects = resources.resource_projects.clone();
        let budgets = resources.retained_layers.clone();

        let error = resources
            .upsert_open(rejected_uri, 1, "open")
            .expect_err("outside authority");
        assert!(
            error.contains("outside configured resource roots"),
            "{error}"
        );
        assert_eq!(resources.generation(), generation);
        assert_eq!(resources.resource_projects, projects);
        assert!(
            resources
                .get(&Url::from_file_path(rejected).expect("URI"))
                .is_none()
        );
        assert!(!resources.resource_projects.contains_key(&rejected_id));
        assert_eq!(resources.retained_layers, budgets);
    }

    #[test]
    fn project_migration_releases_the_previous_scope_and_collects_it() {
        let root = TestDirectory::new();
        let nested = root.0.join("nested");
        std::fs::create_dir(&nested).expect("nested");
        let path = nested.join("document.adoc");
        std::fs::write(&path, "old").expect("source");
        let root_uri = Url::from_directory_path(&root.0).expect("root URI");
        let document_uri = Url::from_file_path(&path).expect("document URI");
        let mut resources = WorkspaceResources::default();
        resources.load_roots(&[root_uri]).expect("load workspace");
        let previous_scope = resources
            .resource_projects
            .get(&uri_id(&document_uri).expect("resource ID"))
            .cloned()
            .expect("previous scope");

        write_resource_config(&nested, 1, 8, 8, false);
        std::fs::write(&path, "new").expect("new source");
        resources
            .reload_file(document_uri.clone())
            .expect("migrate project");

        let current_scope = resources
            .resource_projects
            .get(&uri_id(&document_uri).expect("resource ID"))
            .expect("current scope");
        assert_ne!(current_scope, &previous_scope);
        assert!(!resources.filesystems.contains_key(&previous_scope));
        assert!(!resources.retained_layers.contains_key(&previous_scope));
        assert!(!resources.project_plans.contains_key(&previous_scope));
        assert_eq!(
            resources
                .get(&document_uri)
                .expect("migrated")
                .text()
                .as_ref(),
            "new"
        );
    }

    #[test]
    fn failed_project_migration_preserves_every_committed_layer() {
        let root = TestDirectory::new();
        let nested = root.0.join("nested");
        std::fs::create_dir(&nested).expect("nested");
        let path = nested.join("document.adoc");
        std::fs::write(&path, "old").expect("source");
        let root_uri = Url::from_directory_path(&root.0).expect("root URI");
        let document_uri = Url::from_file_path(&path).expect("document URI");
        let mut resources = WorkspaceResources::default();
        resources.load_roots(&[root_uri]).expect("load workspace");
        let id = uri_id(&document_uri).expect("resource ID");
        let previous_scope = resources
            .resource_projects
            .get(&id)
            .cloned()
            .expect("previous scope");
        let previous_generation = resources.generation();
        let previous_budget = resources
            .filesystems
            .get(&previous_scope)
            .expect("filesystem")
            .lock()
            .expect("lock")
            .budget();

        write_resource_config(&nested, 1, 2, 2, false);
        std::fs::write(&path, "oversized").expect("oversized source");
        resources
            .reload_file(document_uri.clone())
            .expect_err("migration limit");

        assert_eq!(resources.generation(), previous_generation);
        assert_eq!(resources.resource_projects.get(&id), Some(&previous_scope));
        assert_eq!(
            resources
                .get(&document_uri)
                .expect("old state")
                .text()
                .as_ref(),
            "old"
        );
        assert_eq!(
            resources
                .filesystems
                .get(&previous_scope)
                .expect("filesystem")
                .lock()
                .expect("lock")
                .budget(),
            previous_budget
        );
        assert_eq!(resources.filesystems.len(), 1);
        assert_eq!(resources.retained_layers.len(), 1);
    }

    #[test]
    fn failed_overlay_registration_is_atomic_across_workspace_and_budget() {
        let root = TestDirectory::new();
        let disk = root.0.join("disk.adoc");
        let overlay = root.0.join("overlay.adoc");
        std::fs::write(&disk, "disk").expect("disk source");
        let mut resources = WorkspaceResources::default();
        resources
            .load_roots_with_limits(
                &[Url::from_directory_path(&root.0).expect("root URI")],
                WorkspaceLimits {
                    resources: RetainedResourceLimits {
                        max_files: usize::MAX,
                        max_total_bytes: u64::MAX,
                        max_resource_bytes: u64::MAX,
                    },
                    max_roots: 1,
                },
                &NeverCancel,
            )
            .expect("load workspace");
        let overlay_uri = Url::from_file_path(overlay).expect("overlay URI");
        let previous_generation = resources.generation();
        let previous_retained = resources.retained_layers.clone();

        resources
            .upsert_open(overlay_uri.clone(), 1, "open")
            .expect_err("root limit");

        assert_eq!(resources.generation(), previous_generation);
        assert!(resources.get(&overlay_uri).is_none());
        assert_eq!(resources.retained_layers.len(), previous_retained.len());
        assert!(
            !resources
                .resource_projects
                .contains_key(&uri_id(&overlay_uri).expect("resource ID"))
        );
    }

    #[test]
    fn valid_stricter_reload_clears_disk_and_open_overlay() {
        let root = TestDirectory::new();
        write_resource_config(&root.0, 1, 8, 8, false);
        let path = root.0.join("document.adoc");
        std::fs::write(&path, "disk").expect("disk source");
        let root_uri = Url::from_directory_path(&root.0).expect("root URI");
        let document_uri = Url::from_file_path(&path).expect("document URI");
        let mut resources = WorkspaceResources::default();
        resources
            .load_roots(std::slice::from_ref(&root_uri))
            .expect("load workspace");
        resources
            .upsert_open(document_uri.clone(), 1, "open")
            .expect("open overlay");
        write_resource_config(&root.0, 1, 4, 4, false);
        resources
            .reload_roots_with_open_sources(
                &[root_uri],
                &[(document_uri.clone(), 2, Arc::from("too large"))],
            )
            .expect_err("overlay limit");

        assert!(resources.last_load_failed_closed());
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
        resources.remove_disk(&first_uri).expect("remove disk");
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
    fn changed_project_plan_is_rejected_before_the_existing_session_reads() {
        let root = TestDirectory::new();
        write_resource_config(&root.0, 2, 8, 8, false);
        let first = root.0.join("first.adoc");
        let second = root.0.join("second.adoc");
        std::fs::write(&first, "a").expect("first source");
        let root_uri = Url::from_directory_path(&root.0).expect("root URI");
        let first_uri = Url::from_file_path(&first).expect("first URI");
        let second_uri = Url::from_file_path(&second).expect("second URI");
        let mut resources = WorkspaceResources::default();
        resources.load_roots(&[root_uri]).expect("load workspace");

        write_resource_config(&root.0, 2, 1, 1, false);
        std::fs::write(&first, "bb").expect("oversized replacement");
        let error = resources
            .reload_file(first_uri)
            .expect_err("changed plan requires full reload");
        assert!(error.contains("full reload"), "{error}");

        write_resource_config(&root.0, 2, 8, 8, false);
        std::fs::write(&second, "1234567").expect("second source");
        resources
            .reload_file(second_uri)
            .expect("rejected reread did not consume the old session budget");
    }

    #[test]
    fn retained_byte_rejection_rolls_back_replaced_filesystem_charge() {
        let root = TestDirectory::new();
        write_resource_config(&root.0, 2, 4, 4, false);
        let first = root.0.join("first.adoc");
        let second = root.0.join("second.adoc");
        std::fs::write(&first, "a").expect("first source");
        let root_uri = Url::from_directory_path(&root.0).expect("root URI");
        let first_uri = Url::from_file_path(&first).expect("first URI");
        let second_uri = Url::from_file_path(&second).expect("second URI");
        let mut resources = WorkspaceResources::default();
        resources.load_roots(&[root_uri]).expect("load workspace");
        resources
            .upsert_open(first_uri.clone(), 1, "xxx")
            .expect("overlay");

        std::fs::write(&first, "bb").expect("grown disk source");
        let error = resources
            .reload_file(first_uri.clone())
            .expect_err("disk and overlay exceed retained budget");
        assert!(error.contains("retained resource byte"), "{error}");
        resources.close_open(&first_uri).expect("close overlay");

        std::fs::write(&second, "yyy").expect("second source");
        resources
            .reload_file(second_uri)
            .expect("old filesystem charge was restored");
    }

    #[test]
    fn retained_count_rejection_rolls_back_a_new_filesystem_charge() {
        let root = TestDirectory::new();
        write_resource_config(&root.0, 1, 8, 8, false);
        let overlay = root.0.join("overlay.adoc");
        let rejected = root.0.join("rejected.adoc");
        let accepted = root.0.join("accepted.adoc");
        let root_uri = Url::from_directory_path(&root.0).expect("root URI");
        let overlay_uri = Url::from_file_path(&overlay).expect("overlay URI");
        let rejected_uri = Url::from_file_path(&rejected).expect("rejected URI");
        let accepted_uri = Url::from_file_path(&accepted).expect("accepted URI");
        let mut resources = WorkspaceResources::default();
        resources.load_roots(&[root_uri]).expect("load workspace");
        resources
            .upsert_open(overlay_uri.clone(), 1, "open")
            .expect("overlay");

        std::fs::write(&rejected, "disk").expect("rejected source");
        let error = resources
            .reload_file(rejected_uri)
            .expect_err("overlay already consumes retained count");
        assert!(error.contains("retained resource count"), "{error}");
        resources.close_open(&overlay_uri).expect("close overlay");

        std::fs::write(&accepted, "disk").expect("accepted source");
        resources
            .reload_file(accepted_uri)
            .expect("new filesystem charge was removed");
    }

    #[test]
    fn transient_configuration_read_failure_preserves_the_previous_snapshot() {
        let root = TestDirectory::new();
        write_resource_config(&root.0, 1, 8, 8, false);
        let path = root.0.join("document.adoc");
        std::fs::write(&path, "old").expect("source");
        let root_uri = Url::from_directory_path(&root.0).expect("root URI");
        let document_uri = Url::from_file_path(&path).expect("document URI");
        let mut resources = WorkspaceResources::default();
        resources
            .load_roots(std::slice::from_ref(&root_uri))
            .expect("load workspace");

        std::fs::remove_file(root.0.join(adocweave_config::FILE_NAME)).expect("remove config");
        std::fs::create_dir(root.0.join(adocweave_config::FILE_NAME))
            .expect("unreadable config path");
        let error = resources
            .load_roots(&[root_uri])
            .expect_err("configuration read failure");

        assert!(error.contains("read-failed"), "{error}");
        assert!(!resources.last_load_failed_closed());
        assert_eq!(
            resources
                .get(&document_uri)
                .expect("previous snapshot")
                .text()
                .as_ref(),
            "old"
        );
    }

    #[test]
    fn read_failure_after_fail_closed_reload_is_classified_for_the_current_attempt() {
        let root = TestDirectory::new();
        let config = root.0.join(adocweave_config::FILE_NAME);
        std::fs::write(&config, "schema-version = 99\n").expect("invalid config");
        std::fs::write(root.0.join("document.adoc"), "source").expect("source");
        let root_uri = Url::from_directory_path(&root.0).expect("root URI");
        let mut resources = WorkspaceResources::default();

        resources
            .reload_roots_with_open_sources(std::slice::from_ref(&root_uri), &[])
            .expect_err("invalid configuration");
        assert!(resources.last_load_failed_closed());
        let failed_closed_generation = resources.generation();

        std::fs::remove_file(&config).expect("remove invalid config");
        std::fs::create_dir(&config).expect("unreadable config path");
        let error = resources
            .reload_roots_with_open_sources(&[root_uri], &[])
            .expect_err("configuration read failure");

        assert!(error.contains("read-failed"), "{error}");
        assert!(!resources.last_load_failed_closed());
        assert_eq!(resources.generation(), failed_closed_generation);
    }

    #[test]
    fn read_failure_after_load_preserves_previous_view_before_invalid_failure_closes_it() {
        let root = TestDirectory::new();
        write_resource_config(&root.0, 2, 64, 64, false);
        let config = root.0.join(adocweave_config::FILE_NAME);
        let path = root.0.join("document.adoc");
        std::fs::write(&path, "disk").expect("source");
        let root_uri = Url::from_directory_path(&root.0).expect("root URI");
        let document_uri = Url::from_file_path(&path).expect("document URI");
        let mut resources = WorkspaceResources::default();
        resources
            .load_roots(std::slice::from_ref(&root_uri))
            .expect("initial load");
        resources
            .upsert_open(document_uri.clone(), 1, "previous overlay")
            .expect("initial overlay");
        let previous_generation = resources.generation();

        let error = resources
            .reload_roots_with_open_sources_after_load(
                std::slice::from_ref(&root_uri),
                &[(document_uri.clone(), 2, Arc::from("new overlay"))],
                || {
                    std::fs::remove_file(&config).expect("remove config");
                    std::fs::create_dir(&config).expect("make config unreadable");
                },
            )
            .expect_err("post-load configuration read failure");
        assert!(error.contains("read-failed"), "{error}");
        assert!(!resources.last_load_failed_closed());
        assert_eq!(resources.generation(), previous_generation);
        assert_eq!(
            resources
                .get(&document_uri)
                .expect("previous view")
                .text()
                .as_ref(),
            "previous overlay"
        );

        std::fs::remove_dir(&config).expect("remove unreadable config");
        std::fs::write(&config, "invalid = true\n").expect("invalid config");
        resources
            .reload_roots_with_open_sources(&[root_uri], &[])
            .expect_err("invalid configuration");
        assert!(resources.last_load_failed_closed());
        assert!(resources.get(&document_uri).is_none());
    }

    #[test]
    fn invalid_configuration_clears_state_and_rejects_new_input() {
        let root = TestDirectory::new();
        write_resource_config(&root.0, 1, 8, 8, false);
        let path = root.0.join("document.adoc");
        std::fs::write(&path, "old").expect("source");
        let root_uri = Url::from_directory_path(&root.0).expect("root URI");
        let document_uri = Url::from_file_path(&path).expect("document URI");
        let mut resources = WorkspaceResources::default();
        resources
            .load_roots(std::slice::from_ref(&root_uri))
            .expect("load workspace");

        std::fs::write(root.0.join(adocweave_config::FILE_NAME), "invalid = true\n")
            .expect("invalid config");
        resources
            .load_roots(&[root_uri])
            .expect_err("invalid configuration");

        assert!(resources.last_load_failed_closed());
        assert!(resources.get(&document_uri).is_none());
        assert!(resources.input(&document_uri).is_err());
    }

    #[test]
    fn initial_invalid_configuration_commits_an_empty_trusted_state() {
        let root = TestDirectory::new();
        std::fs::write(root.0.join("document.adoc"), "source").expect("source");
        std::fs::write(root.0.join(adocweave_config::FILE_NAME), "invalid = true\n")
            .expect("invalid config");
        let root_uri = Url::from_directory_path(&root.0).expect("root URI");
        let mut resources = WorkspaceResources::default();
        let generation = resources.generation();
        let next_disk_version = resources.next_disk_version;

        resources
            .load_roots(&[root_uri])
            .expect_err("invalid configuration");

        assert!(resources.generation() > generation);
        assert_eq!(resources.next_disk_version, next_disk_version);
        assert_eq!(resources.roots, vec![root.0.canonicalize().expect("root")]);
        assert!(resources.inner.roots().is_empty());
        assert!(resources.last_load_failed_closed());
        assert!(resources.filesystems.is_empty());
        assert!(resources.project_plans.is_empty());
        assert!(resources.resource_projects.is_empty());
        assert!(resources.retained_layers.is_empty());
    }

    #[test]
    fn failed_old_scope_release_rolls_back_reload_and_open_migrations() {
        for migrate_open in [false, true] {
            let root = TestDirectory::new();
            let nested = root.0.join("nested");
            std::fs::create_dir(&nested).expect("nested");
            let path = nested.join("document.adoc");
            std::fs::write(&path, "disk").expect("source");
            let root_uri = Url::from_directory_path(&root.0).expect("root URI");
            let document_uri = Url::from_file_path(&path).expect("document URI");
            let mut resources = WorkspaceResources::default();
            resources.load_roots(&[root_uri]).expect("load workspace");
            if migrate_open {
                resources
                    .upsert_open(document_uri.clone(), 1, "old overlay")
                    .expect("initial overlay");
            }
            let id = uri_id(&document_uri).expect("resource ID");
            let previous_scope = resources
                .resource_projects
                .get(&id)
                .cloned()
                .expect("previous scope");
            let previous_generation = resources.generation();
            let filesystem = Arc::clone(
                resources
                    .filesystems
                    .get(&previous_scope)
                    .expect("old filesystem"),
            );
            let _ = std::thread::spawn(move || {
                let _guard = filesystem.lock().expect("lock before poison");
                panic!("poison old scope");
            })
            .join();
            write_resource_config(&nested, 2, 64, 64, false);

            let error = if migrate_open {
                resources
                    .upsert_open(document_uri.clone(), 2, "new overlay")
                    .expect_err("old release failure")
            } else {
                std::fs::write(&path, "new disk").expect("changed source");
                resources
                    .reload_file(document_uri.clone())
                    .expect_err("old release failure")
            };

            assert!(error.contains("lock is poisoned"), "{error}");
            assert_eq!(resources.generation(), previous_generation);
            assert_eq!(resources.resource_projects.get(&id), Some(&previous_scope));
            assert_eq!(resources.filesystems.len(), 1);
            assert_eq!(
                resources
                    .get(&document_uri)
                    .expect("previous resource")
                    .text()
                    .as_ref(),
                if migrate_open { "old overlay" } else { "disk" }
            );
        }
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

    #[test]
    fn shared_scope_fixture_has_the_same_root_and_include_count_contract() {
        let root = TestDirectory::new();
        write_resource_config(&root.0, 1, 64, 64, true);
        let root_path = root.0.join("root.adoc");
        std::fs::write(
            &root_path,
            include_bytes!("../../../fixtures/resource-limits/root-with-include.adoc"),
        )
        .expect("root source");
        std::fs::write(
            root.0.join("part.adoc"),
            include_bytes!("../../../fixtures/resource-limits/part.adoc"),
        )
        .expect("included source");
        let root_uri = Url::from_directory_path(&root.0).expect("root URI");
        let mut resources = WorkspaceResources::default();

        let error = resources
            .load_roots(&[root_uri])
            .expect_err("root and include exceed count");

        assert!(error.contains("file limit"), "{error}");
    }

    #[test]
    fn analysis_snapshot_does_not_charge_resources_outside_configured_roots() {
        let root = TestDirectory::new();
        std::fs::create_dir(root.0.join("docs")).expect("docs");
        std::fs::create_dir(root.0.join("other")).expect("other");
        std::fs::write(
            root.0.join(adocweave_config::FILE_NAME),
            "schema-version = 1\n[resources]\ninclude = true\nroots = [\"docs\"]\nmax-files = 1\nmax-total-bytes = 8\nmax-resource-bytes = 8\n",
        )
        .expect("root config");
        std::fs::write(root.0.join("docs/root.adoc"), "root").expect("root source");
        write_resource_config(&root.0.join("other"), 1, 8, 8, false);
        std::fs::write(root.0.join("other/outside.adoc"), "outside").expect("outside source");
        let root_uri = Url::from_directory_path(&root.0).expect("root URI");
        let document_uri =
            Url::from_file_path(root.0.join("docs/root.adoc")).expect("document URI");
        let mut resources = WorkspaceResources::default();
        resources.load_roots(&[root_uri]).expect("load workspace");

        let input = resources
            .input(&document_uri)
            .expect("outside resource is not charged");
        assert_eq!(input.snapshot.resources().count(), 1);
    }

    #[test]
    fn watched_resource_outside_configured_roots_is_not_ingested() {
        let root = TestDirectory::new();
        let docs = root.0.join("docs");
        let other = root.0.join("other");
        std::fs::create_dir(&docs).expect("docs");
        std::fs::create_dir(&other).expect("other");
        std::fs::write(
            root.0.join(adocweave_config::FILE_NAME),
            "schema-version = 1\n[resources]\nroots = [\"docs\"]\nmax-files = 1\nmax-total-bytes = 8\nmax-resource-bytes = 8\n",
        )
        .expect("project configuration");
        std::fs::write(docs.join("root.adoc"), "root").expect("root document");
        let outside = other.join("new.adoc");
        let root_uri = Url::from_directory_path(&root.0).expect("root URI");
        let outside_uri = Url::from_file_path(&outside).expect("outside URI");
        let mut resources = WorkspaceResources::default();
        resources.load_roots(&[root_uri]).expect("load workspace");
        std::fs::write(&outside, "outside").expect("outside document");

        let affected = resources
            .reload_file(outside_uri.clone())
            .expect("ignored outside resource");

        assert!(affected.is_empty());
        assert!(resources.get(&outside_uri).is_none());
    }

    #[test]
    fn analysis_snapshot_applies_root_single_resource_limit_to_nested_projects() {
        let root = TestDirectory::new();
        write_resource_config(&root.0, 2, 6, 2, true);
        let root_path = root.0.join("root.adoc");
        std::fs::write(&root_path, "a").expect("root source");
        let nested = root.0.join("nested");
        std::fs::create_dir(&nested).expect("nested");
        write_resource_config(&nested, 1, 4, 4, false);
        std::fs::write(nested.join("child.adoc"), "bbb").expect("child source");
        let root_uri = Url::from_directory_path(&root.0).expect("root URI");
        let document_uri = Url::from_file_path(root_path).expect("document URI");
        let mut resources = WorkspaceResources::default();
        resources.load_roots(&[root_uri]).expect("load workspace");

        let error = resources
            .input(&document_uri)
            .expect_err("root single-resource snapshot limit");
        assert!(error.contains("analysis snapshot"), "{error}");
    }

    #[test]
    fn analysis_snapshot_checked_addition_applies_root_total_limit() {
        let root = TestDirectory::new();
        write_resource_config(&root.0, 2, 3, 3, true);
        let root_path = root.0.join("root.adoc");
        std::fs::write(&root_path, "aa").expect("root source");
        let nested = root.0.join("nested");
        std::fs::create_dir(&nested).expect("nested");
        write_resource_config(&nested, 1, 3, 3, false);
        std::fs::write(nested.join("child.adoc"), "bb").expect("child source");
        let root_uri = Url::from_directory_path(&root.0).expect("root URI");
        let document_uri = Url::from_file_path(root_path).expect("document URI");
        let mut resources = WorkspaceResources::default();
        resources.load_roots(&[root_uri]).expect("load workspace");

        let error = resources
            .input(&document_uri)
            .expect_err("root total snapshot limit");
        assert!(error.contains("analysis snapshot"), "{error}");
    }
}
