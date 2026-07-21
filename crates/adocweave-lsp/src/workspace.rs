//! Versioned, I/O-free workspace resource graph for include analysis.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::io::Read;
use std::sync::Arc;
use std::{
    fs,
    path::{Path, PathBuf},
};

use adocweave::SourceId;
use adocweave::preprocessor::{
    PreprocessOptions, ResourceDocument, ResourceSnapshot, SafeMode, discover_includes,
};
use async_lsp::lsp_types::Url;

const MAX_WORKSPACE_FILES: usize = 10_000;
const MAX_WORKSPACE_BYTES: u64 = 50 * 1024 * 1024;
const MAX_RESOURCE_BYTES: u64 = 10 * 1024 * 1024;

#[derive(Clone, Copy, Debug, Default, Eq, Ord, PartialEq, PartialOrd)]
pub struct WorkspaceGeneration(u64);

impl WorkspaceGeneration {
    fn next(self) -> Self {
        Self(self.0.saturating_add(1))
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WorkspaceResource {
    pub uri: Url,
    pub version: i64,
    pub text: Arc<str>,
    pub dependencies: BTreeSet<String>,
}

#[derive(Clone, Debug)]
pub struct WorkspaceInput {
    pub generation: WorkspaceGeneration,
    pub root: WorkspaceResource,
    pub snapshot: ResourceSnapshot,
    pub options: PreprocessOptions,
    pub resource_versions: BTreeMap<String, i64>,
}

#[derive(Clone, Debug, Default)]
pub struct WorkspaceResources {
    generation: WorkspaceGeneration,
    roots: Vec<PathBuf>,
    disk_resources: BTreeMap<String, WorkspaceResource>,
    open_resources: BTreeSet<String>,
    resources: BTreeMap<String, WorkspaceResource>,
    reverse_dependencies: BTreeMap<String, BTreeSet<String>>,
    next_disk_version: i64,
}

impl WorkspaceResources {
    pub fn load_roots(&mut self, roots: &[Url]) -> Result<(), String> {
        let mut loaded = Self::default();
        let mut files = Vec::new();
        for root in roots {
            let root_path = root
                .to_file_path()
                .map_err(|()| format!("workspace root is not a file URI: {root}"))?;
            let canonical_root = root_path
                .canonicalize()
                .map_err(|error| format!("cannot canonicalize workspace root: {error}"))?;
            if !canonical_root.is_dir() {
                return Err(format!(
                    "workspace root is not a directory: {}",
                    canonical_root.display()
                ));
            }
            loaded.roots.push(canonical_root.clone());
            let mut pending = VecDeque::from([canonical_root.clone()]);
            while let Some(path) = pending.pop_front() {
                let metadata = fs::symlink_metadata(&path)
                    .map_err(|error| format!("cannot inspect {}: {error}", path.display()))?;
                if metadata.file_type().is_symlink() {
                    continue;
                }
                if metadata.is_dir() {
                    let mut children = fs::read_dir(&path)
                        .map_err(|error| format!("cannot read {}: {error}", path.display()))?
                        .collect::<Result<Vec<_>, _>>()
                        .map_err(|error| format!("cannot read {}: {error}", path.display()))?;
                    children.sort_by_key(std::fs::DirEntry::file_name);
                    pending.extend(children.into_iter().map(|entry| entry.path()));
                    continue;
                }
                if path.extension().and_then(|value| value.to_str()) != Some("adoc") {
                    continue;
                }
                let canonical = path
                    .canonicalize()
                    .map_err(|error| format!("cannot canonicalize {}: {error}", path.display()))?;
                if !canonical.starts_with(&canonical_root) {
                    return Err(format!(
                        "workspace resource escapes root: {}",
                        path.display()
                    ));
                }
                files.push(canonical);
                if files.len() > MAX_WORKSPACE_FILES {
                    return Err("workspace resource file limit exceeded".to_owned());
                }
            }
        }
        files.sort();
        files.dedup();
        let mut total_bytes = 0_u64;
        for path in files {
            let uri = Url::from_file_path(&path)
                .map_err(|()| format!("cannot convert path to URI: {}", path.display()))?;
            let text = loaded.read_workspace_file(&path)?;
            total_bytes = total_bytes.saturating_add(text.len() as u64);
            if total_bytes > MAX_WORKSPACE_BYTES {
                return Err("workspace resource byte limit exceeded".to_owned());
            }
            loaded.set_disk_resource(uri, text)?;
        }
        loaded.roots.sort();
        loaded.roots.dedup();
        loaded.generation = self.generation.next();
        *self = loaded;
        Ok(())
    }

    pub fn reload_file(&mut self, uri: Url) -> Result<BTreeSet<String>, String> {
        let path = uri
            .to_file_path()
            .map_err(|()| format!("workspace resource is not a file URI: {uri}"))?;
        let text = self.read_workspace_file(&path)?;
        self.set_disk_resource(uri, text)
    }

    fn read_workspace_file(&self, path: &Path) -> Result<String, String> {
        if path.extension().and_then(|value| value.to_str()) != Some("adoc") {
            return Err(format!(
                "workspace resource is not an .adoc file: {}",
                path.display()
            ));
        }
        let canonical = path
            .canonicalize()
            .map_err(|error| format!("cannot canonicalize {}: {error}", path.display()))?;
        if !self.roots.iter().any(|root| canonical.starts_with(root)) {
            return Err(format!(
                "workspace resource escapes configured roots: {}",
                path.display()
            ));
        }
        let file = fs::File::open(&canonical)
            .map_err(|error| format!("cannot open {}: {error}", canonical.display()))?;
        let metadata = file
            .metadata()
            .map_err(|error| format!("cannot inspect {}: {error}", canonical.display()))?;
        if !metadata.is_file() {
            return Err(format!(
                "workspace resource is not a regular file: {}",
                canonical.display()
            ));
        }
        let mut bytes = Vec::new();
        file.take(MAX_RESOURCE_BYTES + 1)
            .read_to_end(&mut bytes)
            .map_err(|error| format!("cannot read {}: {error}", canonical.display()))?;
        if bytes.len() as u64 > MAX_RESOURCE_BYTES {
            return Err(format!(
                "workspace resource is too large: {}",
                canonical.display()
            ));
        }
        String::from_utf8(bytes)
            .map_err(|error| format!("cannot read {} as UTF-8: {error}", canonical.display()))
    }

    pub fn get(&self, uri: &Url) -> Option<&WorkspaceResource> {
        self.resources.get(uri.as_str())
    }

    /// Installs an open-buffer overlay and returns every transitively affected root URI.
    pub fn upsert_open(
        &mut self,
        uri: Url,
        version: i64,
        text: impl Into<Arc<str>>,
    ) -> Result<BTreeSet<String>, String> {
        if self.open_resources.contains(uri.as_str())
            && self
                .resources
                .get(uri.as_str())
                .is_some_and(|current| version <= current.version)
        {
            return Ok(BTreeSet::new());
        }
        self.open_resources.insert(uri.to_string());
        self.replace_effective(uri, version, text.into())
    }

    fn set_disk_resource(
        &mut self,
        uri: Url,
        text: impl Into<Arc<str>>,
    ) -> Result<BTreeSet<String>, String> {
        let text = text.into();
        let retained_bytes = self
            .disk_resources
            .iter()
            .filter(|(key, _)| key.as_str() != uri.as_str())
            .try_fold(0_u64, |total, (_, resource)| {
                total.checked_add(resource.text.len() as u64)
            })
            .ok_or_else(|| "workspace resource byte limit exceeded".to_owned())?;
        if retained_bytes.saturating_add(text.len() as u64) > MAX_WORKSPACE_BYTES {
            return Err("workspace resource byte limit exceeded".to_owned());
        }
        self.next_disk_version = self.next_disk_version.saturating_add(1);
        let resource = make_resource(uri.clone(), self.next_disk_version, text)?;
        self.disk_resources
            .insert(uri.to_string(), resource.clone());
        if self.open_resources.contains(uri.as_str()) {
            return Ok(BTreeSet::new());
        }
        self.replace_effective_resource(resource)
    }

    fn replace_effective(
        &mut self,
        uri: Url,
        version: i64,
        text: Arc<str>,
    ) -> Result<BTreeSet<String>, String> {
        let resource = make_resource(uri, version, text)?;
        self.replace_effective_resource(resource)
    }

    fn replace_effective_resource(
        &mut self,
        resource: WorkspaceResource,
    ) -> Result<BTreeSet<String>, String> {
        let uri = resource.uri.clone();
        if let Some(previous) = self.resources.get(uri.as_str()) {
            for dependency in &previous.dependencies {
                remove_reverse(&mut self.reverse_dependencies, dependency, uri.as_str());
            }
        }
        for dependency in &resource.dependencies {
            self.reverse_dependencies
                .entry(dependency.clone())
                .or_default()
                .insert(uri.to_string());
        }
        self.resources.insert(uri.to_string(), resource);
        self.generation = self.generation.next();
        Ok(self.affected(uri.as_str()))
    }

    pub fn remove_disk(&mut self, uri: &Url) -> BTreeSet<String> {
        self.disk_resources.remove(uri.as_str());
        if self.open_resources.contains(uri.as_str()) {
            return BTreeSet::new();
        }
        self.remove_effective(uri)
    }

    pub fn close_open(&mut self, uri: &Url) -> Result<BTreeSet<String>, String> {
        if !self.open_resources.remove(uri.as_str()) {
            return Ok(BTreeSet::new());
        }
        if let Some(disk) = self.disk_resources.get(uri.as_str()).cloned() {
            self.replace_effective_resource(disk)
        } else {
            Ok(self.remove_effective(uri))
        }
    }

    fn remove_effective(&mut self, uri: &Url) -> BTreeSet<String> {
        let affected = self.affected(uri.as_str());
        if let Some(previous) = self.resources.remove(uri.as_str()) {
            for dependency in previous.dependencies {
                remove_reverse(&mut self.reverse_dependencies, &dependency, uri.as_str());
            }
        }
        self.generation = self.generation.next();
        affected
    }

    pub fn affected(&self, uri: &str) -> BTreeSet<String> {
        let mut affected = BTreeSet::from([uri.to_owned()]);
        let mut pending = VecDeque::from([uri.to_owned()]);
        while let Some(resource) = pending.pop_front() {
            if let Some(dependents) = self.reverse_dependencies.get(&resource) {
                for dependent in dependents {
                    if affected.insert(dependent.clone()) {
                        pending.push_back(dependent.clone());
                    }
                }
            }
        }
        affected
    }

    pub fn input(&self, root: &Url) -> Result<WorkspaceInput, String> {
        let root = self
            .get(root)
            .cloned()
            .ok_or_else(|| format!("workspace resource is missing: {root}"))?;
        let mut snapshot = ResourceSnapshot::default();
        let mut resource_versions = BTreeMap::new();
        let mut visited = BTreeSet::new();
        let mut pending = VecDeque::from(root.dependencies.iter().cloned().collect::<Vec<_>>());
        while let Some(uri) = pending.pop_front() {
            if !visited.insert(uri.clone()) {
                continue;
            }
            let Some(resource) = self.resources.get(&uri) else {
                continue;
            };
            snapshot.insert(
                uri.clone(),
                ResourceDocument {
                    source_id: SourceId::new(uri.clone()),
                    source: resource.text.to_string(),
                },
            );
            resource_versions.insert(uri, resource.version);
            pending.extend(resource.dependencies.iter().cloned());
        }
        let mut allowed_schemes = BTreeSet::new();
        allowed_schemes.insert("file".to_owned());
        Ok(WorkspaceInput {
            generation: self.generation,
            options: PreprocessOptions {
                source_id: Some(SourceId::new(root.uri.to_string())),
                base_uri: parent_uri(&root.uri),
                safe_mode: SafeMode::Server,
                allowed_schemes,
                ..PreprocessOptions::default()
            },
            root,
            snapshot,
            resource_versions,
        })
    }

    pub const fn generation(&self) -> WorkspaceGeneration {
        self.generation
    }
}

fn make_resource(uri: Url, version: i64, text: Arc<str>) -> Result<WorkspaceResource, String> {
    let dependencies = dependencies(&uri, &text)?;
    Ok(WorkspaceResource {
        uri,
        version,
        text,
        dependencies,
    })
}

fn dependencies(uri: &Url, text: &str) -> Result<BTreeSet<String>, String> {
    let mut dependencies = BTreeSet::new();
    for request in discover_includes(text).map_err(|error| error.to_string())? {
        let target = uri
            .join(&request.target)
            .map_err(|error| format!("invalid include target {}: {error}", request.target))?;
        if target.scheme() == "file" {
            dependencies.insert(target.to_string());
        }
    }
    Ok(dependencies)
}

fn parent_uri(uri: &Url) -> Option<String> {
    uri.join(".").ok().map(|uri| uri.to_string())
}

fn remove_reverse(
    reverse: &mut BTreeMap<String, BTreeSet<String>>,
    dependency: &str,
    dependent: &str,
) {
    let remove_entry = reverse.get_mut(dependency).is_some_and(|dependents| {
        dependents.remove(dependent);
        dependents.is_empty()
    });
    if remove_entry {
        reverse.remove(dependency);
    }
}

#[cfg(test)]
mod tests {
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::*;

    fn uri(value: &str) -> Url {
        Url::parse(value).expect("URL")
    }

    #[test]
    fn versions_and_reverse_dependencies_select_only_affected_roots() {
        let mut resources = WorkspaceResources::default();
        resources
            .upsert_open(uri("file:///book/root.adoc"), 1, "include::part.adoc[]\n")
            .expect("root");
        resources
            .upsert_open(uri("file:///book/other.adoc"), 1, "other\n")
            .expect("other");
        let affected = resources
            .upsert_open(uri("file:///book/part.adoc"), 1, "part\n")
            .expect("part");
        assert_eq!(
            affected,
            BTreeSet::from([
                "file:///book/part.adoc".to_owned(),
                "file:///book/root.adoc".to_owned(),
            ])
        );
        assert!(
            resources
                .upsert_open(uri("file:///book/part.adoc"), 1, "stale\n")
                .expect("stale")
                .is_empty()
        );
    }

    #[test]
    fn snapshot_contains_only_transitive_file_dependencies_and_versions() {
        let mut resources = WorkspaceResources::default();
        resources
            .upsert_open(
                uri("file:///book/root.adoc"),
                4,
                "include::parts/a.adoc[]\n",
            )
            .expect("root");
        resources
            .upsert_open(
                uri("file:///book/parts/a.adoc"),
                2,
                "include::b.adoc[]\nA\n",
            )
            .expect("a");
        resources
            .upsert_open(uri("file:///book/parts/b.adoc"), 7, "B\n")
            .expect("b");
        resources
            .upsert_open(uri("file:///unrelated.adoc"), 1, "no\n")
            .expect("unrelated");

        let input = resources
            .input(&uri("file:///book/root.adoc"))
            .expect("input");
        assert_eq!(input.root.version, 4);
        assert_eq!(input.resource_versions.len(), 2);
        assert_eq!(
            input
                .snapshot
                .get("file:///book/parts/b.adoc")
                .map(|doc| doc.source.as_str()),
            Some("B\n")
        );
        assert!(input.snapshot.get("file:///unrelated.adoc").is_none());
    }

    #[test]
    fn workspace_roots_load_adoc_files_without_following_symlinks() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let root = std::env::temp_dir().join(format!("adocweave-lsp-{unique}"));
        let parts = root.join("parts");
        fs::create_dir_all(&parts).expect("directories");
        fs::write(root.join("root.adoc"), "include::parts/a.adoc[]\n").expect("root");
        fs::write(parts.join("a.adoc"), "loaded\n").expect("part");
        fs::write(root.join("ignored.txt"), "ignored\n").expect("ignored");

        let root_uri = Url::from_directory_path(&root).expect("root URI");
        let document_uri = Url::from_file_path(root.join("root.adoc")).expect("document URI");
        let mut resources = WorkspaceResources::default();
        resources.load_roots(&[root_uri]).expect("load workspace");
        let input = resources.input(&document_uri).expect("workspace input");
        assert_eq!(input.resource_versions.len(), 1);
        assert!(
            input
                .snapshot
                .get(
                    Url::from_file_path(parts.join("a.adoc"))
                        .expect("part URI")
                        .as_str()
                )
                .is_some()
        );

        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn watched_files_cannot_escape_configured_workspace_roots() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let root = std::env::temp_dir().join(format!("adocweave-lsp-root-{unique}"));
        let outside = std::env::temp_dir().join(format!("adocweave-lsp-outside-{unique}.adoc"));
        fs::create_dir_all(&root).expect("workspace");
        fs::write(root.join("root.adoc"), "root\n").expect("root document");
        fs::write(&outside, "outside\n").expect("outside document");

        let mut resources = WorkspaceResources::default();
        resources
            .load_roots(&[Url::from_directory_path(&root).expect("root URI")])
            .expect("load workspace");
        let error = resources
            .reload_file(Url::from_file_path(&outside).expect("outside URI"))
            .expect_err("outside resource must be rejected");
        assert!(error.contains("escapes configured roots"));

        fs::remove_dir_all(root).expect("cleanup workspace");
        fs::remove_file(outside).expect("cleanup outside");
    }

    #[test]
    fn closing_an_open_overlay_restores_disk_and_allows_version_restart() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let root = std::env::temp_dir().join(format!("adocweave-lsp-overlay-{unique}"));
        let path = root.join("root.adoc");
        fs::create_dir_all(&root).expect("workspace");
        fs::write(&path, "disk\n").expect("disk document");
        let root_uri = Url::from_directory_path(&root).expect("root URI");
        let document_uri = Url::from_file_path(&path).expect("document URI");

        let mut resources = WorkspaceResources::default();
        resources.load_roots(&[root_uri]).expect("load workspace");
        resources
            .upsert_open(document_uri.clone(), 50, "open 50\n")
            .expect("open overlay");
        assert_eq!(
            resources.get(&document_uri).map(|item| item.text.as_ref()),
            Some("open 50\n")
        );
        resources.close_open(&document_uri).expect("close overlay");
        assert_eq!(
            resources.get(&document_uri).map(|item| item.text.as_ref()),
            Some("disk\n")
        );
        resources
            .upsert_open(document_uri.clone(), 1, "open 1\n")
            .expect("reopened overlay");
        assert_eq!(
            resources.get(&document_uri).map(|item| item.text.as_ref()),
            Some("open 1\n")
        );

        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn workspace_inputs_are_bound_to_the_generation_that_built_their_snapshot() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let root = std::env::temp_dir().join(format!("adocweave-lsp-generation-{unique}"));
        let root_path = root.join("root.adoc");
        let part_path = root.join("part.adoc");
        fs::create_dir_all(&root).expect("workspace");
        fs::write(&root_path, "include::part.adoc[]\n").expect("root document");
        fs::write(&part_path, "old\n").expect("part document");

        let root_uri = Url::from_directory_path(&root).expect("root URI");
        let document_uri = Url::from_file_path(&root_path).expect("document URI");
        let part_uri = Url::from_file_path(&part_path).expect("part URI");
        let mut resources = WorkspaceResources::default();
        resources.load_roots(&[root_uri]).expect("load workspace");
        let old = resources.input(&document_uri).expect("old input");

        resources
            .upsert_open(part_uri, 1, "new\n")
            .expect("replace dependency");
        let new = resources.input(&document_uri).expect("new input");

        assert_ne!(old.generation, new.generation);
        assert_eq!(new.generation, resources.generation());
        fs::remove_dir_all(root).expect("cleanup");
    }
}
