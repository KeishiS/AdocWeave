//! LSP URI and filesystem adapter for the runtime-independent workspace.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use adocweave::preprocess::{PreprocessOptions, ProjectionLimits, SafeMode};
use adocweave_host::{
    LocalFilesystemPolicy, LocalFilesystemSession, LogicalSourceId, ResourceError,
};
use adocweave_workspace::{
    Generation, ResourceId, Revision, Workspace, WorkspaceAnalysis, WorkspaceLimits,
    WorkspaceSnapshot,
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
    filesystem: Option<Arc<Mutex<LocalFilesystemSession>>>,
    next_disk_version: i64,
}

impl WorkspaceResources {
    pub fn load_roots(&mut self, roots: &[Url]) -> Result<(), String> {
        self.load_roots_with_limits(roots, WorkspaceLimits::default())
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
        let mut filesystem = (!paths.is_empty())
            .then(|| LocalFilesystemPolicy::new(paths.clone(), host_limits(limits.resources)))
            .transpose()
            .map_err(|error| error.to_string())?
            .map(|policy| policy.session())
            .transpose()
            .map_err(|error| error.to_string())?;
        let files = match filesystem.as_mut() {
            Some(session) => session
                .scan_utf8(|path| {
                    let uri = Url::from_file_path(path).map_err(|()| {
                        ResourceError::Unverifiable(format!(
                            "cannot convert workspace path to URI: {}",
                            path.display()
                        ))
                    })?;
                    LogicalSourceId::new(uri.to_string())
                })
                .map_err(|error| error.to_string())?,
            None => Vec::new(),
        };
        let seed = Generation::new(self.inner.generation().get().saturating_add(1));
        let mut inner = Workspace::new_at_generation(limits, seed);
        let mut next_disk_version = self.next_disk_version;
        for file in files {
            next_disk_version = next_disk_version.saturating_add(1);
            let (source_id, text) = file.into_parts();
            let id = ResourceId::new(source_id.as_str()).map_err(|error| error.to_string())?;
            inner
                .upsert_disk(id.clone(), Revision::new(next_disk_version), text)
                .map_err(|error| error.to_string())?;
            inner.register_root(id).map_err(|error| error.to_string())?;
        }
        self.inner = inner;
        self.roots = paths;
        self.filesystem = filesystem.map(|session| Arc::new(Mutex::new(session)));
        self.next_disk_version = next_disk_version;
        Ok(())
    }

    pub fn reload_file(&mut self, uri: Url) -> Result<BTreeSet<String>, String> {
        let path = uri
            .to_file_path()
            .map_err(|()| format!("workspace resource is not a file URI: {uri}"))?;
        let text = self.read_workspace_file(&path)?;
        self.next_disk_version = self.next_disk_version.saturating_add(1);
        let id = uri_id(&uri)?;
        let affected = self
            .inner
            .upsert_disk(id.clone(), Revision::new(self.next_disk_version), text)
            .map_err(|error| error.to_string())?;
        if !self.inner.roots().contains(&id) {
            self.inner
                .register_root(id)
                .map_err(|error| error.to_string())?;
        }
        Ok(strings(affected))
    }

    fn read_workspace_file(&mut self, path: &Path) -> Result<Arc<str>, String> {
        if path.extension().and_then(|value| value.to_str()) != Some("adoc") {
            return Err(format!(
                "workspace resource is not an .adoc file: {}",
                path.display()
            ));
        }
        let policy = self
            .filesystem
            .as_mut()
            .ok_or_else(|| "workspace resource policy is not initialized".to_owned())?;
        policy
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
        Ok(affected)
    }

    pub fn remove_disk(&mut self, uri: &Url) -> BTreeSet<String> {
        if let Ok(path) = uri.to_file_path()
            && let Some(filesystem) = &self.filesystem
            && let Ok(mut session) = filesystem.lock()
        {
            session.release(&path);
        }
        uri_id(uri)
            .map(|id| strings(self.inner.remove_disk(&id)))
            .unwrap_or_default()
    }

    pub fn close_open(&mut self, uri: &Url) -> Result<BTreeSet<String>, String> {
        let id = uri_id(uri)?;
        self.inner
            .close_overlay(&id)
            .map(strings)
            .map_err(|error| error.to_string())
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
        let oversized_resource = snapshot.resources().any(|(_, resource)| {
            resource.text().len() as u64 > project_config.resources.limits.max_resource_bytes
        });
        if retained_files > project_config.resources.limits.max_files
            || oversized_resource
            || retained_bytes
                .is_none_or(|bytes| bytes > project_config.resources.limits.max_total_bytes)
        {
            return Err("configured workspace resource limit exceeded".to_owned());
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
        let boundary = self
            .roots
            .iter()
            .filter(|root| path.starts_with(root))
            .max_by_key(|root| root.components().count());
        let Some(boundary) = boundary else {
            return Ok(None);
        };
        let mut start = path.as_path();
        while !start.exists() {
            let Some(parent) = start.parent() else {
                return Ok(None);
            };
            start = parent;
        }
        adocweave_config::discover_and_load(start, boundary).map_err(|error| error.to_string())
    }
}

fn uri_id(uri: &Url) -> Result<ResourceId, String> {
    ResourceId::new(uri.to_string()).map_err(|error| error.to_string())
}

const fn host_limits(
    limits: adocweave_workspace::ResourceLimits,
) -> adocweave_host::ResourceLimits {
    adocweave_host::ResourceLimits {
        max_files: limits.max_files,
        max_total_bytes: limits.max_total_bytes,
        max_resource_bytes: limits.max_resource_bytes,
    }
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
    fn workspace_and_host_default_resource_limits_stay_aligned() {
        assert_eq!(
            host_limits(adocweave_workspace::ResourceLimits::default()),
            adocweave_host::ResourceLimits::default()
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
}
