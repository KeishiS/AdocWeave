//! Behaviour tests for the LSP workspace adapter.

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

/// Analyses one root the way an analysis worker does.
///
/// The reads happen on a copy, so `resources` is unchanged until the result
/// is installed with [`WorkspaceResources::apply_analyzed_root`].
fn analyze_root(resources: &mut WorkspaceResources, root: &Url) -> Result<AnalyzedRoot, String> {
    let input = resources.input(root)?;
    let job = FilesystemJobCoordinator::new(document_analysis_job_limits())
        .map_err(|error| error.to_string())?;
    resources.analyze_root_detached(
        &input,
        &adocweave::AnalysisOptions::default(),
        &NeverCancel,
        &job,
    )
}

/// Analyses one root and installs the result, as the server does on completion.
fn analyze_and_apply(
    resources: &mut WorkspaceResources,
    root: &Url,
) -> Result<Option<WorkspaceAnalysis>, String> {
    let analyzed = analyze_root(resources, root)?;
    resources.apply_analyzed_root(analyzed)
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
fn scan_candidate_disappearance_does_not_hide_a_remaining_resource() {
    let root = TestDirectory::new();
    let vanished = root.0.join("vanished.adoc");
    let remaining = root.0.join("remaining.adoc");
    std::fs::write(&vanished, "vanished\n").expect("vanishing source");
    std::fs::write(&remaining, "remaining\n").expect("remaining source");
    let candidates = [vanished.clone(), remaining.clone()];
    let session = LocalFilesystemPolicy::new(
        [root.0.clone()],
        adocweave_host::FilesystemReadLimits::default(),
    )
    .expect("filesystem policy")
    .session()
    .expect("filesystem session");
    let job = FilesystemJobCoordinator::new(workspace_scan_job_limits()).expect("scan job");
    let mut filesystem = session.draft(&job).expect("filesystem draft");
    std::fs::remove_file(&vanished).expect("remove discovered source");

    assert!(
        read_scan_candidate(&mut filesystem, &candidates[0])
            .expect("vanished candidate")
            .is_none()
    );
    let read = read_scan_candidate(&mut filesystem, &candidates[1])
        .expect("remaining candidate")
        .expect("remaining source");
    assert_eq!(
        read.source_id.as_str(),
        Url::from_file_path(&remaining).unwrap().as_str()
    );
    assert_eq!(read.text.as_ref(), "remaining\n");
}

#[cfg(unix)]
#[test]
fn project_config_replaced_by_a_symlink_after_discovery_is_not_read() {
    use std::os::unix::fs::symlink;

    let root = TestDirectory::new();
    let config = root.0.join(adocweave_config::FILE_NAME);
    let replacement = root.0.join("replacement.toml");
    std::fs::write(&config, "schema-version = 1\n").expect("project configuration");
    std::fs::write(&replacement, "schema-version = 1\n").expect("replacement");
    let policy = LocalFilesystemPolicy::new([root.0.clone()], FilesystemReadLimits::DEFAULT)
        .expect("filesystem policy");
    let discovered = adocweave_config::discover_with_policy(
        &root.0,
        policy.root_policy(&root.0).expect("root policy"),
    )
    .expect("configuration discovery")
    .expect("configuration path");
    std::fs::remove_file(&config).expect("remove discovered configuration");
    symlink(&replacement, &config).expect("replace configuration with symlink");
    let session = policy
        .access_existing([root.0.clone()], workspace_config_read_limits())
        .and_then(|access| access.session())
        .expect("configuration session");
    let job = FilesystemJobCoordinator::new(workspace_scan_job_limits()).expect("scan job");
    let mut draft = session.draft(&job).expect("configuration draft");
    let source_id = LogicalSourceId::new(
        Url::from_file_path(&discovered)
            .expect("configuration URI")
            .to_string(),
    )
    .expect("source ID");

    assert!(
        draft
            .read_utf8_no_symlinks_outcome(source_id, &discovered)
            .is_err()
    );
    let usage = job.usage().expect("job usage");
    assert_eq!(usage.read_operations, 1);
    assert_eq!(usage.read_bytes, 0);
}

#[test]
fn workspace_scan_accounts_for_discovery_and_multiple_project_scopes_in_one_job() {
    let root = TestDirectory::new();
    let nested = root.0.join("nested");
    std::fs::create_dir(&nested).expect("nested project");
    std::fs::write(root.0.join("root.adoc"), "root\n").expect("root source");
    std::fs::write(
        nested.join(adocweave_config::FILE_NAME),
        "schema-version = 1\n",
    )
    .expect("nested project configuration");
    std::fs::write(nested.join("nested.adoc"), "nested\n").expect("nested source");
    let root_uri = Url::from_directory_path(&root.0).expect("root URI");
    let resources = WorkspaceResources::default();
    let job = FilesystemJobCoordinator::new(workspace_scan_job_limits()).expect("scan job");

    let loaded =
        resources.load_roots_detached_with_job(std::slice::from_ref(&root_uri), &NeverCancel, &job);

    assert_eq!(loaded.error, None);
    let usage = job.usage().expect("job usage");
    assert_eq!(usage.sessions, 4);
    assert_eq!(usage.read_operations, 3);
    assert_eq!(usage.read_bytes, 31);
    assert_eq!(usage.candidate_changes, 3);
}

#[test]
fn workspace_scan_read_limit_is_shared_across_project_scopes() {
    let root = TestDirectory::new();
    let nested = root.0.join("nested");
    std::fs::create_dir(&nested).expect("nested project");
    std::fs::write(root.0.join("root.adoc"), "root\n").expect("root source");
    std::fs::write(
        nested.join(adocweave_config::FILE_NAME),
        "schema-version = 1\n",
    )
    .expect("nested project configuration");
    std::fs::write(nested.join("nested.adoc"), "nested\n").expect("nested source");
    let root_uri = Url::from_directory_path(&root.0).expect("root URI");
    let mut resources = WorkspaceResources::default();
    resources
        .load_roots(std::slice::from_ref(&root_uri))
        .expect("initial workspace");
    assert!(!resources.inner.roots().is_empty());
    let job = FilesystemJobCoordinator::new(FilesystemJobLimits {
        max_read_operations: 2,
        ..workspace_scan_job_limits()
    })
    .expect("scan job");

    let loaded =
        resources.load_roots_detached_with_job(std::slice::from_ref(&root_uri), &NeverCancel, &job);

    assert_eq!(
        loaded.error.as_deref(),
        Some("filesystem job limit exceeded: read operations (2)")
    );
    let usage = job.usage().expect("job usage");
    assert_eq!(usage.sessions, 4);
    assert_eq!(usage.read_operations, 2);
    assert_eq!(usage.candidate_changes, 2);
    assert!(
        resources
            .apply_loaded_roots(loaded, &[])
            .expect_err("job limit must fail closed")
            .contains("filesystem job limit exceeded")
    );
    assert!(resources.inner.roots().is_empty());
    assert!(resources.last_load_failed_closed());
}

#[test]
fn cancelled_workspace_scan_cancels_its_filesystem_job() {
    let root = TestDirectory::new();
    std::fs::write(root.0.join("document.adoc"), "document\n").expect("source");
    let root_uri = Url::from_directory_path(&root.0).expect("root URI");
    let resources = WorkspaceResources::default();
    let cancellation = adocweave::CancellationToken::new();
    cancellation.cancel();
    let job = FilesystemJobCoordinator::new(workspace_scan_job_limits()).expect("scan job");

    let loaded = resources.load_roots_detached_with_job(
        std::slice::from_ref(&root_uri),
        &cancellation,
        &job,
    );

    assert_eq!(
        loaded.error.as_deref(),
        Some("local resource cannot be verified: local filesystem scan was cancelled")
    );
    assert_eq!(
        job.finish(),
        Err(adocweave_host::FilesystemJobError::Cancelled)
    );
    assert!(resources.inner.roots().is_empty());
}

#[test]
fn cancellation_after_the_last_read_discards_the_candidate_before_commit() {
    let root = TestDirectory::new();
    let document = root.0.join("document.adoc");
    std::fs::write(&document, "before\n").expect("source");
    let root_uri = Url::from_directory_path(&root.0).expect("root URI");
    let document_uri = Url::from_file_path(&document).expect("document URI");
    let mut resources = WorkspaceResources::default();
    resources
        .load_roots(std::slice::from_ref(&root_uri))
        .expect("initial workspace");
    let previous = resources
        .get(&document_uri)
        .expect("previous source")
        .text()
        .clone();
    std::fs::write(&document, "after\n").expect("replacement source");
    let mut replacement = resources.clone();
    let cancellation = adocweave::CancellationToken::new();
    let job = FilesystemJobCoordinator::new(workspace_scan_job_limits()).expect("scan job");

    let error = replacement
        .load_roots_with_limits_after_hooks_and_job(
            std::slice::from_ref(&root_uri),
            adapter_managed_workspace_limits(),
            &cancellation,
            &job,
            (|| {}, || {}, || cancellation.cancel()),
        )
        .expect_err("cancelled candidate");

    assert_eq!(error, "workspace scan was cancelled");
    assert_eq!(
        job.finish(),
        Err(adocweave_host::FilesystemJobError::Cancelled)
    );
    assert_eq!(
        resources.get(&document_uri).map(|resource| resource.text()),
        Some(&previous)
    );
    assert!(replacement.inner.roots().is_empty());
}

#[test]
fn scan_exclusion_defers_include_loading_without_promoting_the_resource_to_a_root() {
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
    assert!(resources.get(&included_uri).is_none());

    let analysis = analyze_and_apply(&mut resources, &source_uri)
        .expect("workspace analysis")
        .expect("adopted analysis");

    assert_eq!(
        resources.inner.roots(),
        &BTreeSet::from([uri_id(&source_uri).expect("source ID")]),
        "an excluded include acquired during analysis must not become a root",
    );
    assert!(analysis.analysis.source().contains("included"));

    std::fs::write(&included, "updated include\n").expect("updated include");
    resources
        .reload_file(included_uri.clone())
        .expect("reload known include");
    assert_eq!(
        resources.inner.roots(),
        &BTreeSet::from([uri_id(&source_uri).expect("source ID")]),
        "a watched include must not become an analysis root",
    );
    assert_eq!(
        resources
            .get(&included_uri)
            .expect("updated include resource")
            .text()
            .as_ref(),
        "updated include\n",
    );

    let unrelated = generated.join("unrelated.adoc");
    let unrelated_uri = Url::from_file_path(&unrelated).expect("unrelated URI");
    std::fs::write(&unrelated, "unrelated\n").expect("unrelated source");
    assert!(
        resources
            .reload_file(unrelated_uri.clone())
            .expect("ignore excluded watcher discovery")
            .is_empty()
    );
    assert!(resources.get(&unrelated_uri).is_none());
    assert_eq!(
        resources.inner.roots(),
        &BTreeSet::from([uri_id(&source_uri).expect("source ID")]),
    );
}

/// A result the workspace has moved past is rejected without installing any
/// part of it, including the includes the run acquired along the way.
#[test]
fn a_result_from_an_older_generation_installs_nothing() {
    let root = TestDirectory::new();
    let generated = root.0.join("generated");
    std::fs::create_dir_all(&generated).expect("generated directory");
    std::fs::write(
        root.0.join(adocweave_config::FILE_NAME),
        concat!(
            "schema-version = 1\n",
            "[resources]\ninclude = true\nroots = [\".\"]\n",
            "[workspace.scan]\nexclude = [\"generated\"]\n",
        ),
    )
    .expect("project configuration");
    let source = root.0.join("root.adoc");
    let included = generated.join("part.adoc");
    let unrelated = root.0.join("unrelated.adoc");
    std::fs::write(&source, "include::generated/part.adoc[]\n").expect("source");
    std::fs::write(&included, "included\n").expect("included source");
    let root_uri = Url::from_directory_path(&root.0).expect("root URI");
    let source_uri = Url::from_file_path(&source).expect("source URI");
    let included_uri = Url::from_file_path(&included).expect("included URI");
    let mut resources = WorkspaceResources::default();
    resources.load_roots(&[root_uri]).expect("load workspace");
    let analyzed = analyze_root(&mut resources, &source_uri).expect("workspace analysis");

    // The workspace moves on before the result comes back.
    std::fs::write(&unrelated, "unrelated\n").expect("unrelated source");
    resources
        .reload_file(Url::from_file_path(&unrelated).expect("unrelated URI"))
        .expect("discover an unrelated source");
    let generation = resources.generation();

    assert!(
        resources
            .apply_analyzed_root(analyzed)
            .expect("apply a superseded analysis")
            .is_none()
    );
    assert_eq!(resources.generation(), generation);
    assert!(
        resources.get(&included_uri).is_none(),
        "a superseded result must not install the include it acquired"
    );
}

#[test]
fn an_analysis_that_is_never_adopted_leaves_no_include_behind() {
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
    let before = resources.generation();

    let analyzed = analyze_root(&mut resources, &source_uri).expect("workspace analysis");
    drop(analyzed);

    assert!(
        resources.get(&included_uri).is_none(),
        "an abandoned analysis must not leave the include it read"
    );
    assert_eq!(resources.generation(), before);
}

#[test]
fn closing_an_open_include_removes_only_its_open_root_role() {
    let root = TestDirectory::new();
    let generated = root.0.join("generated");
    std::fs::create_dir_all(&generated).expect("generated directory");
    std::fs::write(
        root.0.join(adocweave_config::FILE_NAME),
        concat!(
            "schema-version = 1\n",
            "[resources]\ninclude = true\nroots = [\".\"]\n",
            "[workspace.scan]\nexclude = [\"generated\"]\n",
        ),
    )
    .expect("project configuration");
    let source = root.0.join("root.adoc");
    let included = generated.join("part.adoc");
    std::fs::write(&source, "include::generated/part.adoc[]\n").expect("source");
    std::fs::write(&included, "included\n").expect("included source");
    let root_uri = Url::from_directory_path(&root.0).expect("root URI");
    let source_uri = Url::from_file_path(&source).expect("source URI");
    let included_uri = Url::from_file_path(&included).expect("included URI");
    let included_id = uri_id(&included_uri).expect("included ID");
    let mut resources = WorkspaceResources::default();
    resources.load_roots(&[root_uri]).expect("load workspace");

    analyze_and_apply(&mut resources, &source_uri)
        .expect("workspace analysis")
        .expect("adopted analysis");
    resources
        .upsert_open(included_uri.clone(), 1, "open include\n")
        .expect("open include");
    assert!(resources.inner.roots().contains(&included_id));

    resources.close_open(&included_uri).expect("close include");

    assert!(resources.get(&included_uri).is_some());
    assert!(resources.include_interests.contains(&included_id));
    assert!(!resources.inner.roots().contains(&included_id));
}

#[test]
fn closing_an_open_scan_root_preserves_its_scan_root_role() {
    let root = TestDirectory::new();
    let source = root.0.join("root.adoc");
    std::fs::write(&source, "disk source\n").expect("source");
    let root_uri = Url::from_directory_path(&root.0).expect("root URI");
    let source_uri = Url::from_file_path(&source).expect("source URI");
    let source_id = uri_id(&source_uri).expect("source ID");
    let mut resources = WorkspaceResources::default();
    resources.load_roots(&[root_uri]).expect("load workspace");

    resources
        .upsert_open(source_uri.clone(), 1, "open source\n")
        .expect("open scan root");
    resources.close_open(&source_uri).expect("close scan root");

    assert!(resources.inner.roots().contains(&source_id));
    assert_eq!(
        resources.analysis_root_roles.get(&source_id),
        Some(&AnalysisRootRoles {
            scan_root: true,
            open_overlay: false,
        })
    );
    assert_eq!(
        resources
            .get(&source_uri)
            .expect("retained disk source")
            .text()
            .as_ref(),
        "disk source\n"
    );
}

#[test]
fn failed_initial_include_read_keeps_a_bounded_watch_interest() {
    let root = TestDirectory::new();
    let generated = root.0.join("generated");
    std::fs::create_dir_all(&generated).expect("generated directory");
    std::fs::write(
        root.0.join(adocweave_config::FILE_NAME),
        concat!(
            "schema-version = 1\n",
            "[resources]\ninclude = true\nroots = [\".\"]\n",
            "[workspace.scan]\nexclude = [\"generated\"]\n",
        ),
    )
    .expect("project configuration");
    let source = root.0.join("root.adoc");
    let included = generated.join("part.txt");
    std::fs::write(&source, "include::generated/part.txt[]\n").expect("source");
    std::fs::write(&included, [0xff]).expect("invalid include");
    let root_uri = Url::from_directory_path(&root.0).expect("root URI");
    let source_uri = Url::from_file_path(&source).expect("source URI");
    let included_uri = Url::from_file_path(&included).expect("included URI");
    let included_id = uri_id(&included_uri).expect("included ID");
    let mut resources = WorkspaceResources::default();
    resources.load_roots(&[root_uri]).expect("load workspace");

    let analyzed = analyze_root(&mut resources, &source_uri).expect("workspace analysis");
    assert!(
        resources
            .apply_analyzed_root(analyzed)
            .expect("apply failed analysis")
            .is_none(),
        "an unreadable include must not produce a published analysis"
    );

    assert!(resources.include_interests.contains(&included_id));
    assert!(
        resources
            .include_dependencies
            .get(&uri_id(&source_uri).expect("source ID"))
            .is_some_and(|dependencies| dependencies.contains(&included_id)),
        "a failed read must keep the document waiting for the repair"
    );

    std::fs::write(&included, "repaired\n").expect("repair include");
    let update = resources
        .apply_watched_file(included_uri.clone(), WatchedFileKind::Upsert)
        .expect("reload repaired include");
    assert!(update.affected.contains(source_uri.as_str()));
    assert_eq!(
        resources
            .get(&included_uri)
            .expect("include")
            .text()
            .as_ref(),
        "repaired\n"
    );
    assert!(!resources.inner.roots().contains(&included_id));
}

#[test]
fn created_excluded_adoc_include_recovers_without_a_scan_root_role() {
    let root = TestDirectory::new();
    let generated = root.0.join("generated");
    std::fs::create_dir_all(&generated).expect("generated directory");
    std::fs::write(
        root.0.join(adocweave_config::FILE_NAME),
        concat!(
            "schema-version = 1\n",
            "[resources]\ninclude = true\nroots = [\".\"]\n",
            "[workspace.scan]\nexclude = [\"generated\"]\n",
        ),
    )
    .expect("project configuration");
    let source = root.0.join("root.adoc");
    let included = generated.join("part.adoc");
    std::fs::write(&source, "include::generated/part.adoc[]\n").expect("source");
    let root_uri = Url::from_directory_path(&root.0).expect("root URI");
    let source_uri = Url::from_file_path(&source).expect("source URI");
    let included_uri = Url::from_file_path(&included).expect("included URI");
    let included_id = uri_id(&included_uri).expect("included ID");
    let mut resources = WorkspaceResources::default();
    resources.load_roots(&[root_uri]).expect("load workspace");

    let analyzed = analyze_root(&mut resources, &source_uri).expect("workspace analysis");
    resources
        .apply_analyzed_root(analyzed)
        .expect("apply analysis with a missing include");
    assert!(
        resources.get(&included_uri).is_none(),
        "the include does not exist yet"
    );

    std::fs::write(&included, "created\n").expect("create include");
    let update = resources
        .apply_watched_file(included_uri.clone(), WatchedFileKind::Upsert)
        .expect("load created include");

    assert!(update.affected.contains(source_uri.as_str()));
    assert!(resources.get(&included_uri).is_some());
    assert!(!resources.inner.roots().contains(&included_id));
    assert_eq!(
        resources.analysis_root_roles.get(&included_id),
        None,
        "include interest must not imply a scan root role"
    );
}

#[test]
fn missing_include_interests_share_the_dependency_count_limit() {
    let root = TestDirectory::new();
    std::fs::write(
        root.0.join(adocweave_config::FILE_NAME),
        "schema-version = 1\n[resources]\ninclude = true\nroots = [\".\"]\n",
    )
    .expect("project configuration");
    let source = root.0.join("root.adoc");
    std::fs::write(&source, "include::missing.txt[]\n").expect("source");
    let root_uri = Url::from_directory_path(&root.0).expect("root URI");
    let source_uri = Url::from_file_path(&source).expect("source URI");
    let target_uri = Url::from_file_path(root.0.join("missing.txt")).expect("target URI");
    let target = uri_id(&target_uri).expect("target ID");
    let mut resources = WorkspaceResources::default();
    resources.load_roots(&[root_uri]).expect("load workspace");
    resources.include_interests = (0..MAX_WATCHED_INCLUDE_RESOURCES)
        .map(|index| ResourceId::new(format!("file:///retained/{index}.txt")).expect("interest ID"))
        .collect();
    resources.include_dependencies.insert(
        uri_id(&source_uri).expect("source ID"),
        resources.include_interests.clone(),
    );

    let error = analyze_root(&mut resources, &source_uri)
        .err()
        .expect("interest count limit");

    assert!(error.contains("include dependency limit"));
    assert_eq!(
        resources.include_interests.len(),
        MAX_WATCHED_INCLUDE_RESOURCES
    );
    assert!(!resources.include_interests.contains(&target));
}

#[test]
fn excluded_unknown_watch_candidate_is_ignored_before_nested_config_is_read() {
    let root = TestDirectory::new();
    let generated = root.0.join("generated");
    std::fs::create_dir_all(&generated).expect("generated directory");
    std::fs::write(
        root.0.join(adocweave_config::FILE_NAME),
        "schema-version = 1\n[workspace.scan]\nexclude = [\"generated\"]\n",
    )
    .expect("root configuration");
    std::fs::write(
        generated.join(adocweave_config::FILE_NAME),
        "schema-version = 99\n",
    )
    .expect("invalid nested configuration");
    let hidden = generated.join("hidden.adoc");
    std::fs::write(&hidden, "hidden\n").expect("hidden source");
    let root_uri = Url::from_directory_path(&root.0).expect("root URI");
    let hidden_uri = Url::from_file_path(&hidden).expect("hidden URI");
    let mut resources = WorkspaceResources::default();
    resources.load_roots(&[root_uri]).expect("load workspace");

    let update = resources
        .apply_watched_file(hidden_uri.clone(), WatchedFileKind::Upsert)
        .expect("ignore excluded candidate");

    assert!(!update.journal_relevant);
    assert!(resources.get(&hidden_uri).is_none());
}

#[test]
#[cfg(unix)]
fn recursive_scan_exclusion_prunes_a_non_utf8_subtree_before_reading_it() {
    use std::ffi::OsString;
    use std::os::unix::ffi::OsStringExt;

    let root = TestDirectory::new();
    std::fs::write(
        root.0.join(adocweave_config::FILE_NAME),
        "schema-version = 1\n[workspace.scan]\nexclude = [\"**\"]\n",
    )
    .expect("project configuration");
    let opaque = root.0.join(OsString::from_vec(vec![b'n', 0x80]));
    std::fs::create_dir(&opaque).expect("non-UTF-8 directory");
    std::fs::write(opaque.join("invalid.adoc"), [0xff]).expect("invalid source");
    let root_uri = Url::from_directory_path(&root.0).expect("root URI");
    let mut resources = WorkspaceResources::default();

    resources
        .load_roots(&[root_uri])
        .expect("excluded subtree is not read");
    assert!(resources.inner.roots().is_empty());
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
        std::fs::write(root.0.join(retained).join("kept.adoc"), "kept\n").expect("retained source");
    }
    let roots = [&first, &second].map(|root| Url::from_directory_path(&root.0).expect("root URI"));
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

#[cfg(target_os = "linux")]
#[test]
fn one_root_authority_covers_configuration_scan_and_document_read() {
    let root = TestDirectory::new();
    let document = root.0.join("root.adoc");
    std::fs::write(
        root.0.join(adocweave_config::FILE_NAME),
        "schema-version = 1\n",
    )
    .expect("trusted configuration");
    std::fs::write(&document, "= Trusted\n").expect("trusted document");
    let root_uri = Url::from_directory_path(&root.0).expect("root URI");
    let document_uri = Url::from_file_path(&document).expect("document URI");
    let displaced = root.0.with_extension("anchored-workspace");
    let mut resources = WorkspaceResources::default();

    let loaded = resources.load_roots_with_limits_after_authority(
        std::slice::from_ref(&root_uri),
        adapter_managed_workspace_limits(),
        &NeverCancel,
        || {
            std::fs::rename(&root.0, &displaced).expect("displace trusted workspace");
            std::fs::create_dir(&root.0).expect("replacement workspace");
            std::fs::write(
                root.0.join(adocweave_config::FILE_NAME),
                "schema-version = 99\n",
            )
            .expect("replacement configuration");
            std::fs::write(root.0.join("root.adoc"), "= Replacement\n")
                .expect("replacement document");
        },
    );

    std::fs::remove_dir_all(&root.0).expect("remove replacement workspace");
    std::fs::rename(&displaced, &root.0).expect("restore trusted workspace");
    loaded.expect("load through retained authority");
    assert_eq!(
        resources
            .resource_text(&document_uri)
            .expect("trusted resource")
            .as_ref(),
        "= Trusted\n",
    );
}

#[cfg(target_os = "linux")]
#[test]
fn retained_root_covers_reload_open_and_missing_include_after_replacement() {
    let root = TestDirectory::new();
    let generated = root.0.join("generated");
    std::fs::create_dir(&generated).expect("generated directory");
    std::fs::write(
        root.0.join(adocweave_config::FILE_NAME),
        concat!(
            "schema-version = 1\n",
            "[resources]\ninclude = true\nroots = [\".\"]\n",
            "[workspace.scan]\nexclude = [\"generated\"]\n",
        ),
    )
    .expect("trusted configuration");
    let document = root.0.join("root.adoc");
    let included = generated.join("part.txt");
    std::fs::write(&document, "include::generated/part.txt[]\n").expect("trusted document");
    std::fs::write(&included, "trusted include\n").expect("trusted include");
    let root_uri = Url::from_directory_path(&root.0).expect("root URI");
    let document_uri = Url::from_file_path(&document).expect("document URI");
    let _include_uri = Url::from_file_path(&included).expect("include URI");
    let mut resources = WorkspaceResources::default();
    resources
        .load_roots(std::slice::from_ref(&root_uri))
        .expect("initial load");

    let displaced = root.0.with_extension("retained-reload");
    std::fs::rename(&root.0, &displaced).expect("displace trusted workspace");
    std::fs::create_dir_all(root.0.join("generated")).expect("replacement workspace");
    std::fs::write(
        root.0.join(adocweave_config::FILE_NAME),
        "schema-version = 99\n",
    )
    .expect("replacement configuration");
    std::fs::write(&document, "replacement document\n").expect("replacement document");
    std::fs::write(root.0.join("generated/part.txt"), "replacement include\n")
        .expect("replacement include");
    std::fs::write(
        displaced.join("root.adoc"),
        "include::generated/part.txt[]\ntrusted reload\n",
    )
    .expect("trusted reload");

    resources
        .reload_file(document_uri.clone())
        .expect("reload through retained root");
    assert!(
        resources
            .resource_text(&document_uri)
            .expect("reloaded resource")
            .contains("trusted reload")
    );
    resources
        .upsert_open(
            document_uri.clone(),
            1,
            "include::generated/part.txt[]\noverlay\n",
        )
        .expect("open through retained configuration");
    let analysis = analyze_and_apply(&mut resources, &document_uri)
        .expect("workspace analysis")
        .expect("adopted analysis");
    assert!(analysis.analysis.source().contains("trusted include"));
    assert!(!analysis.analysis.source().contains("replacement include"));

    std::fs::remove_dir_all(&root.0).expect("remove replacement workspace");
    std::fs::rename(&displaced, &root.0).expect("restore trusted workspace");
}

#[cfg(target_os = "linux")]
#[test]
fn root_replacement_before_authority_fails_without_panicking() {
    use std::os::unix::fs::symlink;

    let root = TestDirectory::new();
    let outside = TestDirectory::new();
    std::fs::write(root.0.join("trusted.adoc"), "trusted\n").expect("trusted source");
    std::fs::write(outside.0.join("outside.adoc"), "outside\n").expect("outside source");
    let root_uri = Url::from_directory_path(&root.0).expect("root URI");
    let displaced = root.0.with_extension("before-authority");
    let mut resources = WorkspaceResources::default();

    let loaded = resources.load_roots_with_limits_after_hooks(
        std::slice::from_ref(&root_uri),
        adapter_managed_workspace_limits(),
        &NeverCancel,
        || {
            std::fs::rename(&root.0, &displaced).expect("displace trusted workspace");
            symlink(&outside.0, &root.0).expect("redirect workspace root");
        },
        || {},
    );

    std::fs::remove_file(&root.0).expect("remove replacement symlink");
    std::fs::rename(&displaced, &root.0).expect("restore trusted workspace");
    let error = loaded.expect_err("changed authority must fail closed");
    assert!(
        error.contains("workspace root changed while its filesystem authority was established")
    );
    assert!(resources.inner.roots().is_empty());
    assert!(resources.last_load_failed_closed());
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
        plan.analysis_snapshot.max_files,
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
    let analyzed = analyze_root(&mut resources, &first_uri).expect("workspace analysis");

    // The include is refused by the root's authority, so the run answers
    // that the resource is absent rather than leaving the preprocessor to
    // fail on a lookup it cannot complete. The classification names what
    // actually happened: the resource is not available to this root.
    assert_eq!(
        analyzed
            .failure()
            .expect("cross-scope include is unavailable")
            .code,
        adocweave_workspace::WorkspaceErrorCode::MissingResource.as_str()
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
    std::fs::create_dir(root.0.join(adocweave_config::FILE_NAME)).expect("unreadable config path");
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
        include_bytes!("../../../../fixtures/resource-limits/root-with-include.adoc"),
    )
    .expect("root source");
    std::fs::write(
        root.0.join("part.adoc"),
        include_bytes!("../../../../fixtures/resource-limits/part.adoc"),
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
    let document_uri = Url::from_file_path(root.0.join("docs/root.adoc")).expect("document URI");
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
