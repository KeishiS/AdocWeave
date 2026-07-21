//! Versioned, I/O-free workspace resource graph for include analysis.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::sync::Arc;
use std::{fs, path::PathBuf};

use adocweave::SourceId;
use adocweave::preprocessor::{
    PreprocessOptions, ResourceDocument, ResourceSnapshot, SafeMode, discover_includes,
};
use async_lsp::lsp_types::Url;

const MAX_WORKSPACE_FILES: usize = 10_000;
const MAX_WORKSPACE_BYTES: u64 = 50 * 1024 * 1024;
const MAX_RESOURCE_BYTES: u64 = 10 * 1024 * 1024;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WorkspaceResource {
    pub uri: Url,
    pub version: i64,
    pub text: Arc<str>,
    pub dependencies: BTreeSet<String>,
}

#[derive(Clone, Debug)]
pub struct WorkspaceInput {
    pub root: WorkspaceResource,
    pub snapshot: ResourceSnapshot,
    pub options: PreprocessOptions,
    pub resource_versions: BTreeMap<String, i64>,
}

#[derive(Clone, Debug, Default)]
pub struct WorkspaceResources {
    resources: BTreeMap<String, WorkspaceResource>,
    reverse_dependencies: BTreeMap<String, BTreeSet<String>>,
}

impl WorkspaceResources {
    pub fn load_roots(&mut self, roots: &[Url]) -> Result<(), String> {
        let mut files = Vec::new();
        let mut total_bytes = 0_u64;
        for root in roots {
            let root_path = root
                .to_file_path()
                .map_err(|()| format!("workspace root is not a file URI: {root}"))?;
            let canonical_root = root_path
                .canonicalize()
                .map_err(|error| format!("cannot canonicalize workspace root: {error}"))?;
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
                if metadata.len() > MAX_RESOURCE_BYTES {
                    return Err(format!(
                        "workspace resource is too large: {}",
                        path.display()
                    ));
                }
                total_bytes = total_bytes.saturating_add(metadata.len());
                if total_bytes > MAX_WORKSPACE_BYTES {
                    return Err("workspace resource byte limit exceeded".to_owned());
                }
                files.push(canonical);
                if files.len() > MAX_WORKSPACE_FILES {
                    return Err("workspace resource file limit exceeded".to_owned());
                }
            }
        }
        files.sort();
        files.dedup();
        for path in files {
            let uri = Url::from_file_path(&path)
                .map_err(|()| format!("cannot convert path to URI: {}", path.display()))?;
            self.load_file_with_version(uri, path, 0)?;
        }
        Ok(())
    }

    pub fn reload_file(&mut self, uri: Url) -> Result<BTreeSet<String>, String> {
        let path = uri
            .to_file_path()
            .map_err(|()| format!("workspace resource is not a file URI: {uri}"))?;
        let version = self
            .resources
            .get(uri.as_str())
            .map_or(0, |resource| resource.version.saturating_add(1));
        self.load_file_with_version(uri, path, version)
    }

    fn load_file_with_version(
        &mut self,
        uri: Url,
        path: PathBuf,
        version: i64,
    ) -> Result<BTreeSet<String>, String> {
        let metadata = fs::metadata(&path)
            .map_err(|error| format!("cannot inspect {}: {error}", path.display()))?;
        if metadata.len() > MAX_RESOURCE_BYTES {
            return Err(format!(
                "workspace resource is too large: {}",
                path.display()
            ));
        }
        let text = fs::read_to_string(&path)
            .map_err(|error| format!("cannot read {} as UTF-8: {error}", path.display()))?;
        self.upsert(uri, version, text)
    }

    pub fn get(&self, uri: &Url) -> Option<&WorkspaceResource> {
        self.resources.get(uri.as_str())
    }

    /// Inserts a newer immutable resource and returns every transitively affected root URI.
    pub fn upsert(
        &mut self,
        uri: Url,
        version: i64,
        text: impl Into<Arc<str>>,
    ) -> Result<BTreeSet<String>, String> {
        if self
            .resources
            .get(uri.as_str())
            .is_some_and(|current| version <= current.version)
        {
            return Ok(BTreeSet::new());
        }
        let text = text.into();
        let dependencies = dependencies(&uri, &text)?;
        if let Some(previous) = self.resources.get(uri.as_str()) {
            for dependency in &previous.dependencies {
                remove_reverse(&mut self.reverse_dependencies, dependency, uri.as_str());
            }
        }
        for dependency in &dependencies {
            self.reverse_dependencies
                .entry(dependency.clone())
                .or_default()
                .insert(uri.to_string());
        }
        self.resources.insert(
            uri.to_string(),
            WorkspaceResource {
                uri: uri.clone(),
                version,
                text,
                dependencies,
            },
        );
        Ok(self.affected(uri.as_str()))
    }

    pub fn remove(&mut self, uri: &Url) -> BTreeSet<String> {
        let affected = self.affected(uri.as_str());
        if let Some(previous) = self.resources.remove(uri.as_str()) {
            for dependency in previous.dependencies {
                remove_reverse(&mut self.reverse_dependencies, &dependency, uri.as_str());
            }
        }
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
            .upsert(uri("file:///book/root.adoc"), 1, "include::part.adoc[]\n")
            .expect("root");
        resources
            .upsert(uri("file:///book/other.adoc"), 1, "other\n")
            .expect("other");
        let affected = resources
            .upsert(uri("file:///book/part.adoc"), 1, "part\n")
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
                .upsert(uri("file:///book/part.adoc"), 1, "stale\n")
                .expect("stale")
                .is_empty()
        );
    }

    #[test]
    fn snapshot_contains_only_transitive_file_dependencies_and_versions() {
        let mut resources = WorkspaceResources::default();
        resources
            .upsert(
                uri("file:///book/root.adoc"),
                4,
                "include::parts/a.adoc[]\n",
            )
            .expect("root");
        resources
            .upsert(
                uri("file:///book/parts/a.adoc"),
                2,
                "include::b.adoc[]\nA\n",
            )
            .expect("a");
        resources
            .upsert(uri("file:///book/parts/b.adoc"), 7, "B\n")
            .expect("b");
        resources
            .upsert(uri("file:///unrelated.adoc"), 1, "no\n")
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
}
