use super::*;

#[test]
fn initial_workspace_scan_prunes_configured_directories() {
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let root = std::env::temp_dir().join(format!("adocweave-scan-exclude-{unique}"));
    let excluded = root.join("generated");
    fs::create_dir_all(&excluded).expect("excluded directory");
    fs::write(
        root.join(adocweave_config::FILE_NAME),
        "schema-version = 1\n[workspace.scan]\nexclude = [\"generated\"]\n",
    )
    .expect("configuration");
    fs::write(root.join("root.adoc"), "= Root\n").expect("root document");
    fs::write(excluded.join("unreadable.adoc"), [0xff]).expect("invalid UTF-8 document");
    let root_uri = lsp::Url::from_directory_path(&root).expect("root URI");
    let document_uri = lsp::Url::from_file_path(root.join("root.adoc")).expect("document URI");

    let mut service = LanguageService::default();
    initialize_with_params(
        &mut service,
        typed(json!({
            "processId": null,
            "rootUri": root_uri,
            "capabilities": {}
        })),
    );
    open(&mut service, document_uri.as_str(), 1, "= Root\n");

    assert!(
        service
            .documents
            .get(document_uri.as_str())
            .expect("root document")
            .workspace_analysis()
            .is_some()
    );
    fs::remove_dir_all(root).expect("cleanup");
}

#[test]
fn excluded_include_targets_are_loaded_without_becoming_analysis_roots() {
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let root = std::env::temp_dir().join(format!("adocweave-excluded-include-{unique}"));
    let excluded = root.join("generated");
    fs::create_dir_all(&excluded).expect("excluded directory");
    fs::write(
        root.join(adocweave_config::FILE_NAME),
        concat!(
            "schema-version = 1\n",
            "[resources]\ninclude = true\nroots = [\".\"]\n",
            "[workspace.scan]\nexclude = [\"generated\"]\n",
        ),
    )
    .expect("configuration");
    let source = "= Root\n\ninclude::generated/part.adoc[]\n";
    fs::write(root.join("root.adoc"), source).expect("root document");
    fs::write(excluded.join("part.adoc"), "included marker\n").expect("included document");
    let root_uri = lsp::Url::from_directory_path(&root).expect("root URI");
    let document_uri = lsp::Url::from_file_path(root.join("root.adoc")).expect("document URI");

    let mut service = LanguageService::default();
    initialize_with_params(
        &mut service,
        typed(json!({
            "processId": null,
            "rootUri": root_uri,
            "capabilities": {}
        })),
    );
    open(&mut service, document_uri.as_str(), 1, source);

    let analysis = service
        .documents
        .get(document_uri.as_str())
        .expect("root document")
        .workspace_analysis()
        .expect("workspace analysis");
    assert!(analysis.analysis.source().contains("included marker"));
    fs::remove_dir_all(root).expect("cleanup");
}

#[test]
fn non_adoc_include_is_loaded_and_watched_without_becoming_an_analysis_root() {
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let root = std::env::temp_dir().join(format!("adocweave-text-include-{unique}"));
    fs::create_dir_all(&root).expect("workspace");
    fs::write(
        root.join(adocweave_config::FILE_NAME),
        "schema-version = 1\n[resources]\ninclude = true\nroots = [\".\"]\n",
    )
    .expect("configuration");
    let document_path = root.join("root.adoc");
    let include_path = root.join("part.txt");
    let source = "include::part.txt[]\n";
    fs::write(&document_path, source).expect("document");
    fs::write(&include_path, "first marker\n").expect("include");
    let root_uri = lsp::Url::from_directory_path(&root).expect("root URI");
    let document_uri = lsp::Url::from_file_path(&document_path).expect("document URI");
    let include_uri = lsp::Url::from_file_path(&include_path).expect("include URI");
    let mut service = LanguageService::default();
    initialize_with_params(
        &mut service,
        typed(json!({
            "processId": null,
            "rootUri": root_uri,
            "capabilities": {}
        })),
    );

    open(&mut service, document_uri.as_str(), 1, source);
    let analysis = service
        .documents
        .get(document_uri.as_str())
        .and_then(|document| document.workspace_analysis())
        .expect("workspace analysis");
    assert!(analysis.analysis.source().contains("first marker"));
    assert_eq!(service.workspace_analysis_count(), 2);

    fs::write(&include_path, "second marker\n").expect("changed include");
    let jobs = service
        .workspace_files_changed_with_journal(typed(json!({
            "changes": [{"uri": include_uri, "type": 2}]
        })))
        .jobs;
    assert_eq!(jobs.len(), 1);
    for job in jobs {
        adopt(&mut service, job);
    }
    let analysis = service
        .documents
        .get(document_uri.as_str())
        .and_then(|document| document.workspace_analysis())
        .expect("updated workspace analysis");
    assert!(analysis.analysis.source().contains("second marker"));

    fs::remove_file(&include_path).expect("remove include");
    let jobs = service
        .workspace_files_changed_with_journal(typed(json!({
            "changes": [{"uri": include_uri, "type": 3}]
        })))
        .jobs;
    for job in jobs {
        adopt(&mut service, job);
    }
    fs::write(&include_path, [0xff]).expect("invalid recreated include");
    let failed = service.workspace_files_changed_with_journal(typed(json!({
        "changes": [{"uri": include_uri, "type": 1}]
    })));
    assert_eq!(
        failed.journal.len(),
        1,
        "a known non-adoc include failure must survive an in-flight scan"
    );
    fs::write(&include_path, "restored marker\n").expect("restore include");
    let jobs = service
        .workspace_files_changed_with_journal(typed(json!({
            "changes": [{"uri": include_uri, "type": 1}]
        })))
        .jobs;
    assert_eq!(jobs.len(), 1, "a known deleted include must remain watched");
    for job in jobs {
        adopt(&mut service, job);
    }
    let analysis = service
        .documents
        .get(document_uri.as_str())
        .and_then(|document| document.workspace_analysis())
        .expect("restored workspace analysis");
    assert!(analysis.analysis.source().contains("restored marker"));
    assert_eq!(service.workspace_analysis_count(), 2);

    assert!(
        change(
            &mut service,
            document_uri.as_str(),
            2,
            json!([{"text": "without include\n"}]),
        )
        .expect("remove include reference")
    );
    assert_eq!(
        service.workspace_analysis_count(),
        1,
        "an unreferenced include must release its retained resource"
    );
    fs::write(&include_path, "ignored marker\n").expect("change unreferenced include");
    assert!(
        service
            .workspace_files_changed_with_journal(typed(json!({
                "changes": [{"uri": include_uri, "type": 2}]
            })))
            .jobs
            .is_empty(),
        "an unreferenced non-adoc include must no longer be watched"
    );

    fs::remove_dir_all(root).expect("cleanup");
}

#[test]
fn initially_missing_non_adoc_include_recovers_when_created() {
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let root = std::env::temp_dir().join(format!("adocweave-created-text-include-{unique}"));
    let generated = root.join("generated");
    fs::create_dir_all(&generated).expect("workspace");
    fs::write(
        root.join(adocweave_config::FILE_NAME),
        concat!(
            "schema-version = 1\n",
            "[resources]\ninclude = true\nroots = [\".\"]\n",
            "[workspace.scan]\nexclude = [\"generated\"]\n",
        ),
    )
    .expect("configuration");
    let document_path = root.join("root.adoc");
    let include_path = generated.join("part.txt");
    let source = "include::generated/part.txt[]\n";
    fs::write(&document_path, source).expect("document");
    let root_uri = lsp::Url::from_directory_path(&root).expect("root URI");
    let document_uri = lsp::Url::from_file_path(&document_path).expect("document URI");
    let include_uri = lsp::Url::from_file_path(&include_path).expect("include URI");
    let mut service = LanguageService::default();
    initialize_with_params(
        &mut service,
        typed(json!({
            "processId": null,
            "rootUri": root_uri,
            "capabilities": {}
        })),
    );

    open(&mut service, document_uri.as_str(), 1, source);
    assert!(
        service
            .diagnostics(&document_uri)
            .expect("missing diagnostics")
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code
                == Some(lsp::NumberOrString::String("missing-resource".to_owned())))
    );

    fs::write(&include_path, "created marker\n").expect("create include");
    let jobs = service
        .workspace_files_changed_with_journal(typed(json!({
            "changes": [{"uri": include_uri, "type": 1}]
        })))
        .jobs;
    assert_eq!(jobs.len(), 1);
    for job in jobs {
        adopt(&mut service, job);
    }
    let analysis = service
        .documents
        .get(document_uri.as_str())
        .and_then(|document| document.workspace_analysis())
        .expect("recovered workspace analysis");
    assert!(analysis.analysis.source().contains("created marker"));
    fs::remove_dir_all(root).expect("cleanup");
}

#[test]
fn watched_disk_change_is_retained_below_an_open_overlay() {
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let root = std::env::temp_dir().join(format!("adocweave-open-disk-watch-{unique}"));
    fs::create_dir_all(&root).expect("workspace");
    let document_path = root.join("root.adoc");
    fs::write(&document_path, "disk before\n").expect("document");
    let root_uri = lsp::Url::from_directory_path(&root).expect("root URI");
    let document_uri = lsp::Url::from_file_path(&document_path).expect("document URI");
    let mut service = LanguageService::default();
    initialize_with_params(
        &mut service,
        typed(json!({
            "processId": null,
            "rootUri": root_uri,
            "capabilities": {}
        })),
    );
    open(&mut service, document_uri.as_str(), 1, "open overlay\n");

    fs::write(&document_path, "disk after\n").expect("change disk source");
    assert!(
        service
            .workspace_files_changed_with_journal(typed(json!({
                "changes": [{"uri": document_uri, "type": 2}]
            })))
            .jobs
            .is_empty(),
        "the unchanged overlay must not be reanalyzed"
    );
    let (_, jobs) = service.close(&document_uri);
    assert!(jobs.is_empty());
    assert_eq!(
        service
            .workspace_resource(&document_uri)
            .expect("retained disk source")
            .as_ref(),
        "disk after\n"
    );

    open(&mut service, document_uri.as_str(), 1, "second overlay\n");
    fs::remove_file(&document_path).expect("delete disk source");
    assert!(
        service
            .workspace_files_changed_with_journal(typed(json!({
                "changes": [{"uri": document_uri, "type": 3}]
            })))
            .jobs
            .is_empty()
    );
    let _ = service.close(&document_uri);
    assert!(service.workspace_resource(&document_uri).is_none());
    fs::remove_dir_all(root).expect("cleanup");
}

#[test]
fn pending_non_adoc_include_recovers_from_a_workspace_problem() {
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let root = std::env::temp_dir().join(format!("adocweave-pending-text-include-{unique}"));
    fs::create_dir_all(&root).expect("workspace");
    fs::write(
        root.join(adocweave_config::FILE_NAME),
        "schema-version = 1\n[resources]\ninclude = true\nroots = [\".\"]\n",
    )
    .expect("configuration");
    let document_path = root.join("root.adoc");
    let include_path = root.join("part.txt");
    let source = "include::part.txt[]\n";
    fs::write(&document_path, source).expect("document");
    fs::write(&include_path, "include::root.adoc[]\n").expect("cyclic include");
    let root_uri = lsp::Url::from_directory_path(&root).expect("root URI");
    let document_uri = lsp::Url::from_file_path(&document_path).expect("document URI");
    let include_uri = lsp::Url::from_file_path(&include_path).expect("include URI");
    let mut service = LanguageService::default();
    initialize_with_params(
        &mut service,
        typed(json!({
            "processId": null,
            "rootUri": root_uri,
            "capabilities": {}
        })),
    );

    open(&mut service, document_uri.as_str(), 1, source);
    assert!(
        service
            .documents
            .get(document_uri.as_str())
            .and_then(|document| document.workspace_analysis())
            .is_none(),
        "the circular include must remain a recoverable workspace problem"
    );

    fs::write(&include_path, "recovered marker\n").expect("repair include");
    let jobs = service
        .workspace_files_changed_with_journal(typed(json!({
            "changes": [{"uri": include_uri, "type": 2}]
        })))
        .jobs;
    assert_eq!(jobs.len(), 1, "the pending include must reanalyze its root");
    for job in jobs {
        adopt(&mut service, job);
    }
    let analysis = service
        .documents
        .get(document_uri.as_str())
        .and_then(|document| document.workspace_analysis())
        .expect("recovered workspace analysis");
    assert!(analysis.analysis.source().contains("recovered marker"));
    fs::remove_dir_all(root).expect("cleanup");
}

#[test]
fn oversized_watched_file_batch_requests_a_quiet_full_scan() {
    let mut service = LanguageService::default();
    let changes = (0..=10_000)
        .map(|index| {
            json!({
                "uri": format!("file:///tmp/adocweave-watch-limit/{index}.adoc"),
                "type": 2
            })
        })
        .collect::<Vec<_>>();
    let outcome = service.workspace_files_changed_with_journal(typed(json!({
        "changes": changes
    })));

    assert!(outcome.recovery_required);
    assert!(outcome.jobs.is_empty());
    assert!(outcome.journal.is_empty());
}

#[test]
fn oversized_watched_file_error_requests_a_quiet_full_scan() {
    let mut service = LanguageService::default();
    initialize_with_params(
        &mut service,
        typed(json!({
            "processId": null,
            "rootUri": "file:///tmp",
            "capabilities": {}
        })),
    );
    let uri = format!("file:///tmp/{}{}.adoc", "a".repeat(70_000), "missing");
    let outcome = service.workspace_files_changed_with_journal(typed(json!({
        "changes": [{"uri": uri, "type": 2}]
    })));

    assert!(outcome.recovery_required);
    assert!(outcome.journal.len() <= 1);
}

#[test]
fn failed_quiet_recovery_is_rearmed_by_the_next_watch_notification() {
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let root = std::env::temp_dir().join(format!("adocweave-watch-recovery-{unique}"));
    fs::create_dir_all(&root).expect("workspace");
    let invalid_path = root.join("invalid.adoc");
    fs::write(&invalid_path, [0xff]).expect("invalid document");
    let root_uri = lsp::Url::from_directory_path(&root).expect("root URI");
    let invalid_uri = lsp::Url::from_file_path(&invalid_path).expect("document URI");
    let mut service = LanguageService::default();
    initialize_with_params(
        &mut service,
        typed(json!({
            "processId": null,
            "rootUri": root_uri.clone(),
            "capabilities": {}
        })),
    );
    let oversized_uri = format!(
        "{}/{}.adoc",
        root_uri.as_str().trim_end_matches('/'),
        "a".repeat(70_000)
    );
    let overflow = service.workspace_files_changed_with_journal(typed(json!({
        "changes": [{"uri": oversized_uri, "type": 2}]
    })));
    assert!(overflow.recovery_required);

    let failed_scan = service.plan_workspace_scan(&adocweave::NeverCancel);
    let _ = service.apply_workspace_scan(failed_scan);
    fs::write(&invalid_path, "recovered\n").expect("repair document");
    let repaired = service.workspace_files_changed_with_journal(typed(json!({
        "changes": [{"uri": invalid_uri, "type": 2}]
    })));
    assert!(
        repaired.recovery_required,
        "a failed recovery must wait for and rearm on the next notification"
    );
    fs::remove_dir_all(root).expect("cleanup");
}

#[test]
fn failed_initial_scan_is_rearmed_by_the_next_watch_notification() {
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let root = std::env::temp_dir().join(format!("adocweave-initial-scan-recovery-{unique}"));
    fs::create_dir_all(&root).expect("workspace");
    let invalid_path = root.join("invalid.adoc");
    fs::write(&invalid_path, [0xff]).expect("invalid document");
    let root_uri = lsp::Url::from_directory_path(&root).expect("root URI");
    let invalid_uri = lsp::Url::from_file_path(&invalid_path).expect("document URI");
    let mut service = LanguageService::default();
    initialize_with_params(
        &mut service,
        typed(json!({
            "processId": null,
            "rootUri": root_uri,
            "capabilities": {}
        })),
    );

    fs::write(&invalid_path, "recovered\n").expect("repair document");
    let repaired = service.workspace_files_changed_with_journal(typed(json!({
        "changes": [{"uri": invalid_uri, "type": 2}]
    })));
    assert!(
        repaired.recovery_required,
        "a failed initial scan must rearm after the repaired file changes"
    );

    let recovered_scan = service.plan_workspace_scan(&adocweave::NeverCancel);
    let _ = service.apply_workspace_scan(recovered_scan);
    let settled = service.workspace_files_changed_with_journal(typed(json!({
        "changes": []
    })));
    assert!(
        !settled.recovery_required,
        "a successful full scan must clear the recovery request"
    );
    fs::remove_dir_all(root).expect("cleanup");
}

#[test]
fn include_added_after_initial_scan_loads_an_excluded_target() {
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let root = std::env::temp_dir().join(format!("adocweave-dynamic-include-{unique}"));
    let excluded = root.join("generated");
    fs::create_dir_all(&excluded).expect("excluded directory");
    fs::write(
        root.join(adocweave_config::FILE_NAME),
        concat!(
            "schema-version = 1\n",
            "[resources]\ninclude = true\nroots = [\".\"]\n",
            "[workspace.scan]\nexclude = [\"generated\"]\n",
        ),
    )
    .expect("configuration");
    let initial = "= Root\n";
    let changed = "= Root\n\ninclude::generated/part.adoc[]\n";
    let document_path = root.join("root.adoc");
    fs::write(&document_path, initial).expect("root document");
    fs::write(excluded.join("part.adoc"), "dynamic included marker\n").expect("included document");
    let root_uri = lsp::Url::from_directory_path(&root).expect("root URI");
    let document_uri = lsp::Url::from_file_path(&document_path).expect("document URI");
    let mut service = LanguageService::default();
    initialize_with_params(
        &mut service,
        typed(json!({
            "processId": null,
            "rootUri": root_uri,
            "capabilities": {}
        })),
    );
    open(&mut service, document_uri.as_str(), 1, initial);

    let mut jobs = service
        .begin_change(typed(json!({
            "textDocument": {"uri": document_uri, "version": 2},
            "contentChanges": [{"text": changed}]
        })))
        .expect("change");
    assert_eq!(jobs.len(), 1);
    let job = jobs.pop().expect("changed analysis");
    let error = job
        .workspace
        .as_ref()
        .expect("workspace input")
        .analyze(&job.request.options, job.cancellation.as_ref())
        .expect_err("missing excluded include");
    let target = error
        .requested_resource()
        .expect("structured missing resource")
        .clone();

    let retry = service
        .resolve_missing_include(&job, &target)
        .expect("resolve missing include")
        .expect("replacement analysis");
    assert_eq!(retry.include_resolution_attempts.len(), 1);
    assert!(
        service
            .resolve_missing_include(&retry, &target)
            .expect("reject repeated target")
            .is_none()
    );
    adopt(&mut service, retry);

    let analysis = service
        .documents
        .get(document_uri.as_str())
        .expect("root document")
        .workspace_analysis()
        .expect("workspace analysis");
    assert!(
        analysis
            .analysis
            .source()
            .contains("dynamic included marker")
    );
    fs::remove_dir_all(root).expect("cleanup");
}

#[test]
fn stale_analysis_cannot_load_a_new_include_resource() {
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let root = std::env::temp_dir().join(format!("adocweave-stale-include-{unique}"));
    let excluded = root.join("generated");
    fs::create_dir_all(&excluded).expect("workspace directory");
    fs::write(
        root.join(adocweave_config::FILE_NAME),
        concat!(
            "schema-version = 1\n",
            "[resources]\ninclude = true\nroots = [\".\"]\n",
            "[workspace.scan]\nexclude = [\"generated\"]\n",
        ),
    )
    .expect("configuration");
    let document_path = root.join("root.adoc");
    let target_path = excluded.join("part.adoc");
    fs::write(&document_path, "= Root\n").expect("root document");
    fs::write(&target_path, "stale included marker\n").expect("included document");
    let root_uri = lsp::Url::from_directory_path(&root).expect("root URI");
    let document_uri = lsp::Url::from_file_path(&document_path).expect("document URI");
    let mut service = LanguageService::default();
    initialize_with_params(
        &mut service,
        typed(json!({
            "processId": null,
            "rootUri": root_uri,
            "capabilities": {}
        })),
    );
    open(&mut service, document_uri.as_str(), 1, "= Root\n");

    let stale_job = service
        .begin_change(typed(json!({
            "textDocument": {"uri": document_uri, "version": 2},
            "contentChanges": [{"text": "include::generated/part.adoc[]\n"}]
        })))
        .expect("first change")
        .pop()
        .expect("stale analysis");
    let target = stale_job
        .workspace
        .as_ref()
        .expect("workspace input")
        .analyze(&stale_job.request.options, stale_job.cancellation.as_ref())
        .expect_err("missing include")
        .requested_resource()
        .expect("structured missing resource")
        .clone();
    service
        .begin_change(typed(json!({
            "textDocument": {"uri": document_uri, "version": 3},
            "contentChanges": [{"text": "= Newer root\n"}]
        })))
        .expect("newer change");

    assert!(
        service
            .resolve_missing_include(&stale_job, &target)
            .expect("ignore stale analysis")
            .is_none()
    );
    fs::remove_dir_all(root).expect("cleanup");
}

#[test]
fn concurrent_missing_includes_converge_on_the_current_workspace_generation() {
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let root = std::env::temp_dir().join(format!("adocweave-concurrent-include-{unique}"));
    let excluded = root.join("generated");
    fs::create_dir_all(&excluded).expect("workspace directory");
    fs::write(
        root.join(adocweave_config::FILE_NAME),
        concat!(
            "schema-version = 1\n",
            "[resources]\ninclude = true\nroots = [\".\"]\n",
            "[workspace.scan]\nexclude = [\"generated\"]\n",
        ),
    )
    .expect("configuration");
    let first_path = root.join("first.adoc");
    let second_path = root.join("second.adoc");
    let first_source = "include::generated/first-part.adoc[]\n";
    let second_source = "include::generated/second-part.adoc[]\n";
    fs::write(&first_path, first_source).expect("first root");
    fs::write(&second_path, second_source).expect("second root");
    fs::write(excluded.join("first-part.adoc"), "first marker\n").expect("first include");
    fs::write(excluded.join("second-part.adoc"), "second marker\n").expect("second include");
    let root_uri = lsp::Url::from_directory_path(&root).expect("root URI");
    let first_uri = lsp::Url::from_file_path(&first_path).expect("first URI");
    let second_uri = lsp::Url::from_file_path(&second_path).expect("second URI");
    let mut service = LanguageService::default();
    initialize_with_params(
        &mut service,
        typed(json!({
            "processId": null,
            "rootUri": root_uri,
            "capabilities": {}
        })),
    );
    let first_job = service
        .begin_open(typed(json!({
            "textDocument": {
                "uri": first_uri,
                "languageId": "asciidoc",
                "version": 1,
                "text": first_source
            }
        })))
        .pop()
        .expect("first job");
    let second_job = service
        .begin_open(typed(json!({
            "textDocument": {
                "uri": second_uri,
                "languageId": "asciidoc",
                "version": 1,
                "text": second_source
            }
        })))
        .pop()
        .expect("second job");

    let first_job = service
        .refresh_stale_workspace(&first_job)
        .expect("first job follows the generation changed by opening the second document");
    let first_target = first_job
        .workspace
        .as_ref()
        .expect("first workspace")
        .analyze(&first_job.request.options, first_job.cancellation.as_ref())
        .expect_err("first missing include")
        .requested_resource()
        .expect("first target")
        .clone();
    let first_retry = service
        .resolve_missing_include(&first_job, &first_target)
        .expect("load first include")
        .expect("first retry");

    let second_current = service
        .refresh_stale_workspace(&second_job)
        .expect("second job follows the generation changed by the first include");
    let second_target = second_current
        .workspace
        .as_ref()
        .expect("second current workspace")
        .analyze(
            &second_current.request.options,
            second_current.cancellation.as_ref(),
        )
        .expect_err("second missing include")
        .requested_resource()
        .expect("second target")
        .clone();
    let second_retry = service
        .resolve_missing_include(&second_current, &second_target)
        .expect("load second include")
        .expect("second retry");
    let first_current = service
        .refresh_stale_workspace(&first_retry)
        .expect("first job follows the generation changed by the second include");

    adopt(&mut service, first_current);
    adopt(&mut service, second_retry);
    assert!(
        service
            .documents
            .get(first_uri.as_str())
            .expect("first document")
            .workspace_analysis()
            .expect("first analysis")
            .analysis
            .source()
            .contains("first marker")
    );
    assert!(
        service
            .documents
            .get(second_uri.as_str())
            .expect("second document")
            .workspace_analysis()
            .expect("second analysis")
            .analysis
            .source()
            .contains("second marker")
    );
    fs::remove_dir_all(root).expect("cleanup");
}

#[test]
fn explicitly_opened_document_remains_available_below_an_excluded_directory() {
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let root = std::env::temp_dir().join(format!("adocweave-excluded-open-{unique}"));
    let excluded = root.join("generated");
    fs::create_dir_all(&excluded).expect("excluded directory");
    fs::write(
        root.join(adocweave_config::FILE_NAME),
        "schema-version = 1\n[workspace.scan]\nexclude = [\"generated\"]\n",
    )
    .expect("configuration");
    let document_path = excluded.join("opened.adoc");
    fs::write(&document_path, "= Disk\n").expect("document");
    let root_uri = lsp::Url::from_directory_path(&root).expect("root URI");
    let document_uri = lsp::Url::from_file_path(document_path).expect("document URI");

    let mut service = LanguageService::default();
    initialize_with_params(
        &mut service,
        typed(json!({
            "processId": null,
            "rootUri": root_uri,
            "capabilities": {}
        })),
    );
    open(&mut service, document_uri.as_str(), 1, "= Open overlay\n");

    assert!(
        service
            .documents
            .get(document_uri.as_str())
            .expect("open document")
            .workspace_analysis()
            .is_some()
    );
    fs::remove_dir_all(root).expect("cleanup");
}

#[test]
fn file_workspace_folder_analyzes_only_the_selected_document_as_a_root() {
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let root = std::env::temp_dir().join(format!("adocweave-single-file-{unique}"));
    fs::create_dir_all(&root).expect("workspace");
    let document_path = root.join("document.adoc");
    fs::write(&document_path, "single file\n").expect("document");
    let document_uri = lsp::Url::from_file_path(&document_path).expect("document URI");

    let mut service = LanguageService::default();
    initialize_with_params(
        &mut service,
        typed(json!({
            "processId": null,
            "workspaceFolders": [{"uri": document_uri, "name": "document.adoc"}],
            "capabilities": {"workspace": {"workspaceFolders": true}}
        })),
    );
    open(&mut service, document_uri.as_str(), 1, "single file\n");

    let diagnostics = service.diagnostics(&document_uri).expect("diagnostics");
    assert!(diagnostics.diagnostics.iter().all(|diagnostic| {
        diagnostic.code
            != Some(lsp::NumberOrString::String(
                "workspace-resource-error".to_owned(),
            ))
    }));
    assert!(
        service
            .documents
            .get(document_uri.as_str())
            .expect("open document")
            .workspace_analysis()
            .is_some()
    );
    fs::remove_dir_all(root).expect("cleanup");
}

#[test]
fn analysis_adoption_rejects_a_stale_workspace_generation() {
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let root: PathBuf = std::env::temp_dir().join(format!("adocweave-stale-workspace-{unique}"));
    let root_path = root.join("root.adoc");
    let part_path = root.join("part.adoc");
    fs::create_dir_all(&root).expect("workspace");
    fs::write(&root_path, "include::part.adoc[]\n").expect("root document");
    fs::write(&part_path, "old\n").expect("part document");
    let root_uri = lsp::Url::from_directory_path(&root).expect("root URI");
    let document_uri = lsp::Url::from_file_path(&root_path).expect("document URI");
    let part_uri = lsp::Url::from_file_path(&part_path).expect("part URI");

    let mut service = LanguageService::default();
    let params = typed(json!({
        "processId": null,
        "rootUri": root_uri,
        "capabilities": {}
    }));
    initialize_with_params(&mut service, params);
    let job = service
        .begin_open(typed(json!({
            "textDocument": {
                "uri": document_uri,
                "languageId": "asciidoc",
                "version": 1,
                "text": "include::part.adoc[]\n"
            }
        })))
        .into_iter()
        .next()
        .expect("analysis job");
    let analysis = job
        .request
        .analyze(job.cancellation.as_ref())
        .expect("analysis");

    fs::write(&part_path, "new\n").expect("changed part");
    service.workspace_files_changed_with_journal(typed(json!({
        "changes": [{"uri": part_uri, "type": 2}]
    })));

    assert_eq!(service.adopt(&job, analysis), Adoption::Stale);
    fs::remove_dir_all(root).expect("cleanup");
}

#[test]
fn stale_analysis_never_replaces_published_diagnostics() {
    let mut service = LanguageService::default();
    let document_uri = uri("file:///stale-diagnostics.adoc");
    let stale_job = service
        .begin_open(typed(json!({
            "textDocument": {
                "uri": document_uri,
                "languageId": "asciidoc",
                "version": 1,
                "text": "trailing  \n"
            }
        })))
        .pop()
        .expect("stale job");
    let current_job = service
        .begin_open(typed(json!({
            "textDocument": {
                "uri": document_uri,
                "languageId": "asciidoc",
                "version": 2,
                "text": "current\n"
            }
        })))
        .pop()
        .expect("current job");
    adopt(&mut service, current_job);

    let stale_analysis = stale_job
        .request
        .analyze(&adocweave::NeverCancel)
        .expect("stale analysis");
    assert_eq!(service.adopt(&stale_job, stale_analysis), Adoption::Stale);

    let published = service.diagnostics(&document_uri).expect("diagnostics");
    assert_eq!(published.version, Some(2));
    assert!(published.diagnostics.is_empty());
}

#[test]
fn oversized_did_open_preserves_every_committed_state_and_emits_no_job() {
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let root = std::env::temp_dir().join(format!("adocweave-oversized-open-{unique}"));
    fs::create_dir_all(&root).expect("workspace");
    fs::write(
        root.join(adocweave_config::FILE_NAME),
        "schema-version = 1\n[resources]\nmax-files = 8\nmax-total-bytes = 8\nmax-resource-bytes = 4\n",
    )
    .expect("configuration");
    let document_path = root.join("document.adoc");
    let root_uri = lsp::Url::from_directory_path(&root).expect("root URI");
    let document_uri = lsp::Url::from_file_path(&document_path).expect("document URI");
    let mut service = LanguageService::default();
    initialize_with_params(
        &mut service,
        typed(json!({
            "processId": null,
            "rootUri": root_uri,
            "capabilities": {}
        })),
    );
    let accepted = service.begin_open(typed(json!({
        "textDocument": {
            "uri": document_uri,
            "languageId": "asciidoc",
            "version": 1,
            "text": "old"
        }
    })));
    assert_eq!(accepted.len(), 1);
    let previous = service
        .documents
        .get(document_uri.as_str())
        .expect("committed document")
        .request
        .revision
        .clone();

    let rejected = service.begin_open(typed(json!({
        "textDocument": {
            "uri": document_uri,
            "languageId": "asciidoc",
            "version": 2,
            "text": "oversized"
        }
    })));

    assert!(rejected.is_empty());
    let current = service
        .documents
        .get(document_uri.as_str())
        .expect("previous document");
    assert_eq!(current.request.revision, previous);
    assert_eq!(current.request.source.as_ref(), "old");
    let diagnostics = service.diagnostics(&document_uri).expect("diagnostics");
    assert!(diagnostics.diagnostics.iter().any(|diagnostic| {
        diagnostic.code
            == Some(lsp::NumberOrString::String(
                "workspace-resource-error".to_owned(),
            ))
            && diagnostic.message.contains("retained resource byte")
    }));

    let recovered = service.begin_open(typed(json!({
        "textDocument": {
            "uri": document_uri,
            "languageId": "asciidoc",
            "version": 3,
            "text": "new"
        }
    })));
    assert_eq!(recovered.len(), 1);
    let current = service
        .documents
        .get(document_uri.as_str())
        .expect("recovered document");
    assert_eq!(current.request.revision.version, 3);
    assert_eq!(current.request.source.as_ref(), "new");
    let diagnostics = service.diagnostics(&document_uri).expect("diagnostics");
    assert!(diagnostics.diagnostics.iter().all(|diagnostic| {
        diagnostic.code
            != Some(lsp::NumberOrString::String(
                "workspace-resource-error".to_owned(),
            ))
    }));
    fs::remove_dir_all(root).expect("cleanup");
}

#[test]
fn did_open_outside_configured_roots_preserves_state_and_emits_no_job() {
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let root = std::env::temp_dir().join(format!("adocweave-outside-open-{unique}"));
    let docs = root.join("docs");
    let other = root.join("other");
    fs::create_dir_all(&docs).expect("docs");
    fs::create_dir_all(&other).expect("other");
    fs::write(
        root.join(adocweave_config::FILE_NAME),
        "schema-version = 1\n[resources]\nroots = [\"docs\"]\n",
    )
    .expect("configuration");
    let accepted_path = docs.join("accepted.adoc");
    let rejected_path = other.join("rejected.adoc");
    fs::write(&accepted_path, "accepted").expect("accepted source");
    fs::write(&rejected_path, "rejected").expect("rejected source");
    let root_uri = lsp::Url::from_directory_path(&root).expect("root URI");
    let accepted_uri = lsp::Url::from_file_path(&accepted_path).expect("accepted URI");
    let rejected_uri = lsp::Url::from_file_path(&rejected_path).expect("rejected URI");
    let mut service = LanguageService::default();
    initialize_with_params(
        &mut service,
        typed(json!({
            "processId": null,
            "rootUri": root_uri,
            "capabilities": {}
        })),
    );
    let jobs = service.begin_open(typed(json!({
        "textDocument": {
            "uri": accepted_uri,
            "languageId": "asciidoc",
            "version": 1,
            "text": "accepted"
        }
    })));
    assert_eq!(jobs.len(), 1);
    let open_sources = service.documents.open_sources();

    let rejected = service.begin_open(typed(json!({
        "textDocument": {
            "uri": rejected_uri,
            "languageId": "asciidoc",
            "version": 1,
            "text": "open"
        }
    })));

    assert!(rejected.is_empty());
    assert_eq!(service.documents.open_sources(), open_sources);
    assert!(service.documents.get(rejected_uri.as_str()).is_none());
    let diagnostics = service.diagnostics(&rejected_uri).expect("diagnostics");
    assert!(diagnostics.diagnostics.iter().any(|diagnostic| {
        diagnostic.code
            == Some(lsp::NumberOrString::String(
                "workspace-resource-error".to_owned(),
            ))
            && diagnostic
                .message
                .contains("outside configured resource roots")
    }));
    fs::remove_dir_all(root).expect("cleanup");
}

#[test]
fn workspace_folders_null_does_not_fall_back_to_legacy_root_uri() {
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let root = std::env::temp_dir().join(format!("adocweave-workspace-null-{unique}"));
    fs::create_dir_all(&root).expect("workspace");
    fs::write(root.join("part.adoc"), "included\n").expect("part");
    let root_uri = lsp::Url::from_directory_path(&root).expect("root URI");
    let document_uri = lsp::Url::from_file_path(root.join("root.adoc")).expect("document URI");
    let mut service = LanguageService::default();
    let params = typed(json!({
        "processId": null,
        "rootUri": root_uri,
        "workspaceFolders": null,
        "capabilities": {"workspace": {"workspaceFolders": true}}
    }));
    initialize_with_params(&mut service, params);
    open(
        &mut service,
        document_uri.as_str(),
        1,
        "include::part.adoc[]\n",
    );

    assert!(
        service
            .documents
            .get(document_uri.as_str())
            .expect("document")
            .workspace_analysis()
            .is_none()
    );
    fs::remove_dir_all(root).expect("cleanup");
}

#[test]
fn legacy_root_path_is_used_only_when_root_uri_is_null() {
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let root = std::env::temp_dir().join(format!("adocweave-root-path-{unique}"));
    fs::create_dir_all(&root).expect("workspace");
    fs::write(root.join("part.adoc"), "included\n").expect("part");
    let document_uri = lsp::Url::from_file_path(root.join("root.adoc")).expect("document URI");
    let mut service = LanguageService::default();
    let params = typed(json!({
        "processId": null,
        "rootPath": root,
        "rootUri": null,
        "capabilities": {}
    }));
    initialize_with_params(&mut service, params);
    open(
        &mut service,
        document_uri.as_str(),
        1,
        "include::part.adoc[]\n",
    );

    assert!(
        service
            .documents
            .get(document_uri.as_str())
            .expect("document")
            .workspace_analysis()
            .is_some()
    );
    fs::remove_dir_all(root).expect("cleanup");
}

#[test]
fn workspace_folder_changes_rebuild_roots_and_preserve_open_overlays() {
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let base = std::env::temp_dir().join(format!("adocweave-workspace-change-{unique}"));
    let retained = base.join("retained");
    let removed = base.join("removed");
    let added = base.join("added");
    for root in [&retained, &removed, &added] {
        fs::create_dir_all(root).expect("workspace");
    }
    fs::write(retained.join("part.adoc"), "disk\n").expect("part");
    let retained_uri = lsp::Url::from_directory_path(&retained).expect("retained URI");
    let removed_uri = lsp::Url::from_directory_path(&removed).expect("removed URI");
    let added_uri = lsp::Url::from_directory_path(&added).expect("added URI");
    let document_uri = lsp::Url::from_file_path(retained.join("root.adoc")).expect("document URI");
    let mut service = LanguageService::default();
    let params = typed(json!({
        "processId": null,
        "workspaceFolders": [
            {"uri": retained_uri, "name": "retained"},
            {"uri": removed_uri, "name": "removed"}
        ],
        "capabilities": {"workspace": {"workspaceFolders": true}}
    }));
    let result = initialize_with_params(&mut service, params);
    let value = serde_json::to_value(result).expect("initialize result");
    assert_eq!(
        value["capabilities"]["workspace"]["workspaceFolders"]["supported"],
        true
    );
    assert_eq!(
        value["capabilities"]["workspace"]["workspaceFolders"]["changeNotifications"],
        true
    );
    open(
        &mut service,
        document_uri.as_str(),
        3,
        "include::part.adoc[]\n\noverlay\n",
    );

    assert!(service.workspace_folders_changed(typed(json!({
        "event": {
            "removed": [{"uri": removed_uri, "name": "removed"}],
            "added": [{"uri": added_uri, "name": "added"}]
        }
    }))));
    let scan = service.plan_workspace_scan(&adocweave::NeverCancel);
    let jobs = service.apply_workspace_scan(scan).jobs;
    assert_eq!(jobs.len(), 1);
    assert_eq!(jobs[0].request.revision.version, 3);
    assert!(jobs[0].request.source.contains("overlay"));
    assert_eq!(
        jobs[0]
            .workspace
            .as_ref()
            .expect("retained workspace")
            .root_text()
            .expect("root resource")
            .as_ref(),
        "include::part.adoc[]\n\noverlay\n"
    );
    fs::remove_dir_all(base).expect("cleanup");
}

#[test]
fn removing_a_workspace_folder_does_not_fail_the_retained_folder() {
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let base = std::env::temp_dir().join(format!("adocweave-workspace-removal-{unique}"));
    let retained = base.join("retained");
    let removed = base.join("removed");
    for root in [&retained, &removed] {
        fs::create_dir_all(root).expect("workspace");
        fs::write(root.join("root.adoc"), "disk\n").expect("document");
    }
    let retained_root = lsp::Url::from_directory_path(&retained).expect("retained root URI");
    let removed_root = lsp::Url::from_directory_path(&removed).expect("removed root URI");
    let retained_document =
        lsp::Url::from_file_path(retained.join("root.adoc")).expect("retained document URI");
    let removed_document =
        lsp::Url::from_file_path(removed.join("root.adoc")).expect("removed document URI");
    let mut service = LanguageService::default();
    initialize_with_params(
        &mut service,
        typed(json!({
            "processId": null,
            "workspaceFolders": [
                {"uri": retained_root, "name": "retained"},
                {"uri": removed_root, "name": "removed"}
            ],
            "capabilities": {"workspace": {"workspaceFolders": true}}
        })),
    );
    open(
        &mut service,
        retained_document.as_str(),
        1,
        "retained overlay\n",
    );
    open(
        &mut service,
        removed_document.as_str(),
        1,
        "removed overlay\n",
    );

    assert!(service.workspace_folders_changed(typed(json!({
        "event": {
            "removed": [{"uri": removed_root, "name": "removed"}],
            "added": []
        }
    }))));
    let scan = service.plan_workspace_scan(&adocweave::NeverCancel);
    let jobs = service.apply_workspace_scan(scan).jobs;

    let retained_job = jobs
        .iter()
        .find(|job| job.uri == retained_document.as_str())
        .expect("retained document reanalysis");
    assert!(
        retained_job.workspace.is_some(),
        "retained workspace problem: {:?}",
        retained_job.workspace_problem
    );
    assert!(retained_job.workspace_problem.is_none());
    let removed_job = jobs
        .iter()
        .find(|job| job.uri == removed_document.as_str())
        .expect("removed document reanalysis");
    assert!(removed_job.workspace.is_none());
    assert_eq!(
        removed_job
            .workspace_problem
            .as_ref()
            .expect("removed document problem")
            .code,
        "workspace-input-error"
    );
    assert_eq!(
        service
            .workspace_resource(&retained_document)
            .expect("retained workspace source")
            .as_ref(),
        "retained overlay\n"
    );

    fs::remove_dir_all(base).expect("cleanup");
}

#[test]
fn workspace_configuration_updates_and_caps_debounce() {
    let mut service = LanguageService::default();
    service
        .update_configuration(json!({"adocweave": {"debounceMs": 25}}))
        .expect("configuration");
    assert_eq!(service.debounce_ms(), 25);

    service
        .update_configuration(json!({"debounceMs": 50_000}))
        .expect("configuration");
    assert_eq!(service.debounce_ms(), 1_000);
    assert!(
        service
            .update_configuration(json!({"unknown": true}))
            .is_err()
    );
    assert!(
        service
            .update_configuration(json!({"enabledRules": ["unknown-rule"]}))
            .is_err()
    );
}

#[test]
fn project_configuration_is_shared_with_lsp_and_reloaded_by_generation() {
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let root = std::env::temp_dir().join(format!("adocweave-lsp-config-{unique}"));
    fs::create_dir_all(&root).expect("workspace");
    let document_path = root.join("root.adoc");
    let config_path = root.join(adocweave_config::FILE_NAME);
    let source = "日😀xref:guide.adoc[Guide]\n\nSecond\n";
    fs::write(&document_path, source).expect("document");
    fs::write(
        &config_path,
        include_str!("../../../../fixtures/config/shared-v1/.adocweave.toml"),
    )
    .expect("configuration");
    let root_uri = lsp::Url::from_directory_path(&root).expect("root URI");
    let document_uri = lsp::Url::from_file_path(&document_path).expect("document URI");
    let config_uri = lsp::Url::from_file_path(&config_path).expect("config URI");
    let mut service = LanguageService::default();
    initialize_with_params(
        &mut service,
        typed(json!({
            "processId": null,
            "rootUri": root_uri,
            "capabilities": {}
        })),
    );
    open(&mut service, document_uri.as_str(), 1, source);
    let diagnostics = service.diagnostics(&document_uri).expect("diagnostics");
    assert!(diagnostics.diagnostics.iter().any(|diagnostic| {
        diagnostic.code == Some(lsp::NumberOrString::String("macro-boundary".to_owned()))
            && diagnostic.severity == Some(lsp::DiagnosticSeverity::ERROR)
    }));
    let edits = service
        .formatting(&document_uri)
        .expect("formatting")
        .expect("formatting response");
    assert_eq!(
        apply_edits(source, &edits),
        "日😀xref:guide.adoc[Guide]\r\n\r\nSecond"
    );

    fs::write(
        &config_path,
        "schema-version = 1\n[lint.rules.macro-boundary]\nenabled = false\n",
    )
    .expect("updated configuration");
    assert!(
        service
            .workspace_files_changed_with_journal(typed(json!({
                "changes": [{"uri": config_uri, "type": 2}]
            })))
            .jobs
            .is_empty()
    );
    let scan = service.plan_workspace_scan(&adocweave::NeverCancel);
    let jobs = service.apply_workspace_scan(scan).jobs;
    assert_eq!(jobs.len(), 1);
    for job in jobs {
        adopt(&mut service, job);
    }
    let diagnostics = service.diagnostics(&document_uri).expect("diagnostics");
    assert!(diagnostics.diagnostics.iter().all(|diagnostic| {
        diagnostic.code != Some(lsp::NumberOrString::String("macro-boundary".to_owned()))
    }));

    fs::remove_dir_all(root).expect("cleanup");
}

#[test]
fn configuration_watch_does_not_restore_open_overlay_outside_resource_roots() {
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let root = std::env::temp_dir().join(format!("adocweave-lsp-root-authority-{unique}"));
    let docs = root.join("docs");
    fs::create_dir_all(&docs).expect("workspace");
    let document_path = root.join("outside.adoc");
    let config_path = root.join(adocweave_config::FILE_NAME);
    fs::write(&document_path, "disk").expect("document");
    fs::write(
        &config_path,
        "schema-version = 1\n[resources]\nroots = [\".\"]\nmax-files = 8\nmax-total-bytes = 64\nmax-resource-bytes = 64\n",
    )
    .expect("configuration");
    let root_uri = lsp::Url::from_directory_path(&root).expect("root URI");
    let document_uri = lsp::Url::from_file_path(&document_path).expect("document URI");
    let config_uri = lsp::Url::from_file_path(&config_path).expect("config URI");
    let mut service = LanguageService::default();
    initialize_with_params(
        &mut service,
        typed(json!({
            "processId": null,
            "rootUri": root_uri,
            "capabilities": {}
        })),
    );
    open(&mut service, document_uri.as_str(), 1, "open overlay");

    fs::write(
        &config_path,
        "schema-version = 1\n[resources]\nroots = [\"docs\"]\nmax-files = 8\nmax-total-bytes = 64\nmax-resource-bytes = 64\n",
    )
    .expect("narrowed configuration");
    assert!(
        service
            .workspace_files_changed_with_journal(typed(json!({
                "changes": [{"uri": config_uri, "type": 2}]
            })))
            .jobs
            .is_empty()
    );
    let scan = service.plan_workspace_scan(&adocweave::NeverCancel);
    let jobs = service.apply_workspace_scan(scan).jobs;

    assert_eq!(jobs.len(), 1);
    assert!(jobs[0].workspace.is_none());
    assert_eq!(
        jobs[0]
            .workspace_problem
            .as_ref()
            .expect("fail-closed workspace problem")
            .code,
        "workspace-input-error"
    );
    adopt(&mut service, jobs.into_iter().next().expect("reanalysis"));
    let diagnostics = service.diagnostics(&document_uri).expect("diagnostics");
    assert!(diagnostics.diagnostics.iter().any(|diagnostic| {
        diagnostic.code
            == Some(lsp::NumberOrString::String(
                "workspace-input-error".to_owned(),
            ))
    }));
    fs::remove_dir_all(root).expect("cleanup");
}

#[test]
fn project_configuration_bounds_lsp_diagnostics_before_protocol_projection() {
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let root = std::env::temp_dir().join(format!("adocweave-lsp-config-limit-{unique}"));
    fs::create_dir_all(&root).expect("workspace");
    let document_path = root.join("root.adoc");
    let source = "long \n*x\n";
    fs::write(&document_path, source).expect("document");
    fs::write(
        root.join(adocweave_config::FILE_NAME),
        "schema-version = 1\n[lint]\nmax-line-length = 4\nmax-diagnostics = 1\n",
    )
    .expect("configuration");
    let root_uri = lsp::Url::from_directory_path(&root).expect("root URI");
    let document_uri = lsp::Url::from_file_path(&document_path).expect("document URI");
    let mut service = LanguageService::default();
    initialize_with_params(
        &mut service,
        typed(json!({
            "processId": null,
            "rootUri": root_uri,
            "capabilities": {}
        })),
    );
    open(&mut service, document_uri.as_str(), 1, source);

    let diagnostics = service.diagnostics(&document_uri).expect("diagnostics");
    assert_eq!(diagnostics.diagnostics.len(), 1);
    assert_eq!(
        diagnostics.diagnostics[0].code,
        Some(lsp::NumberOrString::String(
            "trailing-whitespace".to_owned()
        ))
    );

    fs::remove_dir_all(root).expect("cleanup");
}

#[test]
fn each_workspace_folder_uses_its_own_project_configuration() {
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let base = std::env::temp_dir().join(format!("adocweave-lsp-multi-config-{unique}"));
    let enabled_root = base.join("enabled");
    let disabled_root = base.join("disabled");
    fs::create_dir_all(&enabled_root).expect("enabled workspace");
    fs::create_dir_all(&disabled_root).expect("disabled workspace");
    fs::write(
        enabled_root.join(adocweave_config::FILE_NAME),
        "schema-version = 1\n[lint.rules.macro-boundary]\nenabled = true\nseverity = \"error\"\n",
    )
    .expect("enabled configuration");
    fs::write(
        disabled_root.join(adocweave_config::FILE_NAME),
        "schema-version = 1\n[lint.rules.macro-boundary]\nenabled = false\n",
    )
    .expect("disabled configuration");
    let source = "日😀xref:guide.adoc[Guide]\n";
    let enabled_path = enabled_root.join("root.adoc");
    let disabled_path = disabled_root.join("root.adoc");
    fs::write(&enabled_path, source).expect("enabled document");
    fs::write(&disabled_path, source).expect("disabled document");
    let enabled_root_uri = lsp::Url::from_directory_path(&enabled_root).expect("enabled root URI");
    let disabled_root_uri =
        lsp::Url::from_directory_path(&disabled_root).expect("disabled root URI");
    let enabled_uri = lsp::Url::from_file_path(&enabled_path).expect("enabled document URI");
    let disabled_uri = lsp::Url::from_file_path(&disabled_path).expect("disabled document URI");

    let mut service = LanguageService::default();
    initialize_with_params(
        &mut service,
        typed(json!({
            "processId": null,
            "workspaceFolders": [
                {"uri": enabled_root_uri, "name": "enabled"},
                {"uri": disabled_root_uri, "name": "disabled"}
            ],
            "capabilities": {"workspace": {"workspaceFolders": true}}
        })),
    );
    open(&mut service, enabled_uri.as_str(), 1, source);
    open(&mut service, disabled_uri.as_str(), 1, source);

    let enabled = service
        .diagnostics(&enabled_uri)
        .expect("enabled diagnostics");
    assert!(enabled.diagnostics.iter().any(|diagnostic| {
        diagnostic.code == Some(lsp::NumberOrString::String("macro-boundary".to_owned()))
            && diagnostic.severity == Some(lsp::DiagnosticSeverity::ERROR)
    }));
    let disabled = service
        .diagnostics(&disabled_uri)
        .expect("disabled diagnostics");
    assert!(disabled.diagnostics.iter().all(|diagnostic| {
        diagnostic.code != Some(lsp::NumberOrString::String("macro-boundary".to_owned()))
    }));

    fs::remove_dir_all(base).expect("cleanup");
}

#[test]
fn invalid_project_configuration_does_not_fall_back_to_default_analysis() {
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let root = std::env::temp_dir().join(format!("adocweave-lsp-invalid-config-{unique}"));
    fs::create_dir_all(&root).expect("workspace");
    let document_path = root.join("root.adoc");
    fs::write(
        root.join(adocweave_config::FILE_NAME),
        "schema-version = 99\n",
    )
    .expect("configuration");
    fs::write(&document_path, "trailing \n").expect("document");
    let root_uri = lsp::Url::from_directory_path(&root).expect("root URI");
    let document_uri = lsp::Url::from_file_path(&document_path).expect("document URI");
    let mut service = LanguageService::default();
    initialize_with_params(
        &mut service,
        typed(json!({
            "processId": null,
            "rootUri": root_uri,
            "capabilities": {}
        })),
    );
    let jobs = service.begin_open(typed(json!({
        "textDocument": {
            "uri": document_uri,
            "languageId": "asciidoc",
            "version": 1,
            "text": "trailing \n"
        }
    })));
    assert_eq!(jobs.len(), 1, "the workspace error must be publishable");
    assert!(jobs[0].workspace.is_none());
    assert!(jobs[0].workspace_problem.is_some());
    for job in jobs {
        adopt(&mut service, job);
    }
    assert!(service.documents.get(document_uri.as_str()).is_some());

    let diagnostics = service.diagnostics(&document_uri).expect("diagnostics");
    assert!(diagnostics.diagnostics.iter().any(|diagnostic| {
        diagnostic.code
            == Some(lsp::NumberOrString::String(
                "workspace-resource-error".to_owned(),
            ))
    }));
    assert!(diagnostics.diagnostics.iter().all(|diagnostic| {
        diagnostic.code
            != Some(lsp::NumberOrString::String(
                "trailing-whitespace".to_owned(),
            ))
    }));
    assert!(
        service
            .formatting(&document_uri)
            .expect("formatting")
            .expect("response")
            .is_empty()
    );

    fs::remove_dir_all(root).expect("cleanup");
}

#[test]
fn invalidated_project_configuration_clears_old_feature_views() {
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let root = std::env::temp_dir().join(format!("adocweave-lsp-invalidated-config-{unique}"));
    fs::create_dir_all(&root).expect("workspace");
    let document_path = root.join("root.adoc");
    let config_path = root.join(adocweave_config::FILE_NAME);
    let source = "xref:other.adoc[Other]\ntrailing  \n";
    fs::write(&document_path, source).expect("document");
    fs::write(&config_path, "schema-version = 1\n").expect("configuration");
    let root_uri = lsp::Url::from_directory_path(&root).expect("root URI");
    let document_uri = lsp::Url::from_file_path(&document_path).expect("document URI");
    let config_uri = lsp::Url::from_file_path(&config_path).expect("config URI");
    let mut service = LanguageService::default();
    initialize(&mut service, &["utf-16"]);
    assert!(service.workspace_folders_changed(typed(json!({
        "event": {"added": [{"uri": root_uri, "name": "root"}], "removed": []}
    }))));
    let scan = service.plan_workspace_scan(&adocweave::NeverCancel);
    let _ = service.apply_workspace_scan(scan);
    open(&mut service, document_uri.as_str(), 1, source);
    assert!(service.documents.snapshot(document_uri.as_str()).is_some());

    fs::write(&config_path, "schema-version = 99\n").expect("invalid configuration");
    assert!(
        service
            .workspace_files_changed_with_journal(typed(json!({
                "changes": [{"uri": config_uri, "type": 2}]
            })))
            .jobs
            .is_empty()
    );
    let scan = service.plan_workspace_scan(&adocweave::NeverCancel);
    let jobs = service.apply_workspace_scan(scan).jobs;

    assert_eq!(jobs.len(), 1);
    assert!(service.documents.snapshot(document_uri.as_str()).is_none());
    assert!(
        service
            .hover(&document_uri, lsp::Position::new(0, 6))
            .expect("hover")
            .is_none()
    );
    let diagnostics = service.diagnostics(&document_uri).expect("diagnostics");
    assert!(diagnostics.diagnostics.iter().all(|diagnostic| {
        diagnostic.code
            != Some(lsp::NumberOrString::String(
                "trailing-whitespace".to_owned(),
            ))
    }));
    for job in jobs {
        assert!(job.workspace_problem.is_some());
        adopt(&mut service, job);
    }
    assert!(service.documents.snapshot(document_uri.as_str()).is_none());
    fs::remove_dir_all(root).expect("cleanup");
}

#[test]
fn watched_resource_failure_is_published_and_cleared_after_recovery() {
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let root = std::env::temp_dir().join(format!("adocweave-watch-recovery-{unique}"));
    fs::create_dir_all(&root).expect("workspace");
    fs::write(
        root.join(adocweave_config::FILE_NAME),
        "schema-version = 1\n[resources]\ninclude = true\nroots = [\".\"]\n",
    )
    .expect("configuration");
    let document_path = root.join("root.adoc");
    let include_path = root.join("part.adoc");
    let unrelated_path = root.join("unrelated.adoc");
    fs::write(&document_path, "include::part.adoc[]\n").expect("document");
    fs::write(&include_path, "part\n").expect("include");
    fs::write(&unrelated_path, "unrelated\n").expect("unrelated document");
    let root_uri = lsp::Url::from_directory_path(&root).expect("root URI");
    let document_uri = lsp::Url::from_file_path(&document_path).expect("document URI");
    let include_uri = lsp::Url::from_file_path(&include_path).expect("include URI");
    let unrelated_uri = lsp::Url::from_file_path(&unrelated_path).expect("unrelated URI");
    let mut service = LanguageService::default();
    initialize_with_params(
        &mut service,
        typed(json!({
            "processId": null,
            "rootUri": root_uri,
            "capabilities": {}
        })),
    );
    open(
        &mut service,
        document_uri.as_str(),
        1,
        "include::part.adoc[]\n",
    );

    fs::write(&include_path, [0xff]).expect("invalid include");
    let jobs = service
        .workspace_files_changed_with_journal(typed(json!({
            "changes": [{"uri": include_uri, "type": 2}]
        })))
        .jobs;
    assert_eq!(jobs.len(), 1, "watch failure must be published");
    for job in jobs {
        adopt(&mut service, job);
    }
    assert!(
        service
            .diagnostics(&document_uri)
            .expect("failure diagnostics")
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code
                == Some(lsp::NumberOrString::String(
                    "workspace-resource-error".to_owned()
                )))
    );

    fs::write(&unrelated_path, "changed\n").expect("changed unrelated document");
    for job in service
        .workspace_files_changed_with_journal(typed(json!({
            "changes": [{"uri": unrelated_uri, "type": 2}]
        })))
        .jobs
    {
        adopt(&mut service, job);
    }
    assert!(
        service
            .diagnostics(&document_uri)
            .expect("unrelated change diagnostics")
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code
                == Some(lsp::NumberOrString::String(
                    "workspace-resource-error".to_owned()
                ))),
        "a successful notification for another URI must not clear the failure"
    );

    fs::write(&include_path, "restored\n").expect("restored include");
    let jobs = service
        .workspace_files_changed_with_journal(typed(json!({
            "changes": [{"uri": include_uri, "type": 2}]
        })))
        .jobs;
    assert_eq!(jobs.len(), 1, "watch recovery must clear the failure");
    for job in jobs {
        adopt(&mut service, job);
    }
    assert!(
        service
            .diagnostics(&document_uri)
            .expect("recovered diagnostics")
            .diagnostics
            .iter()
            .all(|diagnostic| diagnostic.code
                != Some(lsp::NumberOrString::String(
                    "workspace-resource-error".to_owned()
                )))
    );

    fs::remove_dir_all(root).expect("cleanup");
}

#[test]
fn watched_file_batch_applies_only_the_final_event_for_each_uri() {
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let root = std::env::temp_dir().join(format!("adocweave-watch-coalesce-{unique}"));
    fs::create_dir_all(&root).expect("workspace");
    let document_path = root.join("root.adoc");
    let include_path = root.join("part.adoc");
    fs::write(&document_path, "include::part.adoc[]\n").expect("document");
    fs::write(&include_path, "part\n").expect("include");
    let root_uri = lsp::Url::from_directory_path(&root).expect("root URI");
    let document_uri = lsp::Url::from_file_path(&document_path).expect("document URI");
    let include_uri = lsp::Url::from_file_path(&include_path).expect("include URI");
    let mut service = LanguageService::default();
    initialize_with_params(
        &mut service,
        typed(json!({
            "processId": null,
            "rootUri": root_uri,
            "capabilities": {}
        })),
    );
    open(
        &mut service,
        document_uri.as_str(),
        1,
        "include::part.adoc[]\n",
    );

    fs::remove_file(&include_path).expect("remove include");
    for job in service
        .workspace_files_changed_with_journal(typed(json!({
            "changes": [
                {"uri": include_uri, "type": 2},
                {"uri": include_uri, "type": 3}
            ]
        })))
        .jobs
    {
        adopt(&mut service, job);
    }

    assert!(
        service
            .diagnostics(&document_uri)
            .expect("diagnostics")
            .diagnostics
            .iter()
            .all(|diagnostic| diagnostic.code
                != Some(lsp::NumberOrString::String(
                    "workspace-resource-error".to_owned()
                ))),
        "the discarded changed event must not leave a watch read failure"
    );
    fs::remove_dir_all(root).expect("cleanup");
}

#[test]
fn stricter_resource_plan_invalidates_the_rejected_open_overlay() {
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let root = std::env::temp_dir().join(format!("adocweave-lsp-overlay-plan-{unique}"));
    fs::create_dir_all(&root).expect("workspace");
    let document_path = root.join("root.adoc");
    let config_path = root.join(adocweave_config::FILE_NAME);
    fs::write(&document_path, "disk\n").expect("document");
    fs::write(
        &config_path,
        "schema-version = 1\n[resources]\nmax-files = 2\nmax-total-bytes = 16\nmax-resource-bytes = 16\n",
    )
    .expect("configuration");
    let root_uri = lsp::Url::from_directory_path(&root).expect("root URI");
    let document_uri = lsp::Url::from_file_path(&document_path).expect("document URI");
    let config_uri = lsp::Url::from_file_path(&config_path).expect("config URI");
    let mut service = LanguageService::default();
    initialize_with_params(
        &mut service,
        typed(json!({
            "processId": null, "rootUri": root_uri, "capabilities": {}
        })),
    );
    open(&mut service, document_uri.as_str(), 1, "`open`\n");
    fs::write(
        &config_path,
        "schema-version = 1\n[resources]\nmax-files = 2\nmax-total-bytes = 8\nmax-resource-bytes = 8\n",
    )
    .expect("stricter configuration");

    assert!(
        service
            .workspace_files_changed_with_journal(typed(json!({
                "changes": [{"uri": config_uri, "type": 2}]
            })))
            .jobs
            .is_empty()
    );
    let scan = service.plan_workspace_scan(&adocweave::NeverCancel);
    let jobs = service.apply_workspace_scan(scan).jobs;

    assert_eq!(jobs.len(), 1);
    assert!(jobs[0].workspace.is_none());
    assert!(
        jobs[0]
            .workspace_problem
            .as_ref()
            .expect("input error")
            .message
            .contains("retained resource byte")
    );
    assert!(service.documents.snapshot(document_uri.as_str()).is_none());
    for job in jobs {
        adopt(&mut service, job);
    }
    assert!(service.documents.snapshot(document_uri.as_str()).is_none());
    let diagnostics = service.diagnostics(&document_uri).expect("diagnostics");
    assert!(diagnostics.diagnostics.iter().any(|diagnostic| {
        diagnostic.code
            == Some(lsp::NumberOrString::String(
                "workspace-input-error".to_owned(),
            ))
    }));
    fs::remove_dir_all(root).expect("cleanup");
}

#[test]
fn workspace_configuration_reanalyzes_open_documents_with_enabled_rules() {
    let mut service = LanguageService::default();
    open(
        &mut service,
        "file:///configured-rule.adoc",
        1,
        "日😀xref:guide.adoc[Guide]\n",
    );
    let document = uri("file:///configured-rule.adoc");
    assert!(
        service
            .diagnostics(&document)
            .expect("default diagnostics")
            .diagnostics
            .iter()
            .all(|diagnostic| {
                diagnostic.code != Some(lsp::NumberOrString::String("macro-boundary".to_owned()))
            })
    );

    let jobs = service
        .update_configuration(json!({"enabledRules": ["macro-boundary"]}))
        .expect("configuration");
    assert_eq!(jobs.len(), 1);
    for job in jobs {
        adopt(&mut service, job);
    }
    assert!(
        service
            .diagnostics(&document)
            .expect("configured diagnostics")
            .diagnostics
            .iter()
            .any(|diagnostic| {
                diagnostic.code == Some(lsp::NumberOrString::String("macro-boundary".to_owned()))
            })
    );

    let jobs = service
        .update_configuration(json!({"enabledRules": []}))
        .expect("disabled configuration");
    assert_eq!(jobs.len(), 1);
    for job in jobs {
        adopt(&mut service, job);
    }
    assert!(
        service
            .diagnostics(&document)
            .expect("disabled diagnostics")
            .diagnostics
            .iter()
            .all(|diagnostic| {
                diagnostic.code != Some(lsp::NumberOrString::String("macro-boundary".to_owned()))
            })
    );
}

#[test]
fn workspace_include_analysis_uses_versioned_resources_and_projects_diagnostics() {
    let mut service = LanguageService::default();
    open(&mut service, "file:///book/part.adoc", 3, "==Part\n");
    open(
        &mut service,
        "file:///book/root.adoc",
        1,
        "= Root\n\ninclude::part.adoc[]\n",
    );

    let root = service
        .documents
        .get("file:///book/root.adoc")
        .expect("root");
    let workspace = root.workspace_analysis().expect("workspace analysis");
    assert!(workspace.analysis.source().contains("==Part"));
    assert_eq!(
        workspace.resource_versions.get("file:///book/part.adoc"),
        Some(&3)
    );
    let links = service
        .document_links(&uri("file:///book/root.adoc"))
        .expect("document links")
        .expect("links");
    assert!(links.iter().any(|link| {
        link.target.as_ref().map(lsp::Url::as_str) == Some("file:///book/part.adoc")
            && link.range.start == lsp::Position::new(2, 9)
    }));
    let definition = service
        .definition(&uri("file:///book/root.adoc"), lsp::Position::new(2, 10))
        .expect("definition")
        .expect("include definition");
    let lsp::GotoDefinitionResponse::Scalar(definition) = definition else {
        panic!("scalar include definition");
    };
    assert_eq!(definition.uri.as_str(), "file:///book/part.adoc");

    let diagnostics = service
        .diagnostics(&uri("file:///book/part.adoc"))
        .expect("diagnostics");
    assert_eq!(
        diagnostics
            .diagnostics
            .iter()
            .filter(|diagnostic| diagnostic.code
                == Some(lsp::NumberOrString::String(
                    "heading-marker-space".to_owned()
                )))
            .count(),
        1,
        "direct and projected diagnostics are deduplicated: {:#?}",
        diagnostics.diagnostics
    );

    let root_generation = service
        .documents
        .get("file:///book/root.adoc")
        .expect("root")
        .request
        .revision
        .generation;
    assert!(
        change(
            &mut service,
            "file:///book/part.adoc",
            4,
            json!([{"text": "== Part\n"}]),
        )
        .expect("change")
    );
    let reanalyzed = service
        .documents
        .get("file:///book/root.adoc")
        .expect("root");
    assert!(reanalyzed.request.revision.generation > root_generation);
    assert!(reanalyzed.workspace_analysis().is_some());
}

#[test]
fn missing_include_is_reported_as_a_project_diagnostic_at_the_directive() {
    let mut service = LanguageService::default();
    open(
        &mut service,
        "file:///book/root.adoc",
        1,
        "= Root\n\ninclude::missing.adoc[]\n",
    );

    let diagnostics = service
        .diagnostics(&uri("file:///book/root.adoc"))
        .expect("diagnostics");
    let problem = diagnostics
        .diagnostics
        .iter()
        .find(|diagnostic| diagnostic.source.as_deref() == Some("adocweave-project"))
        .expect("project diagnostic");
    assert_eq!(
        problem.code,
        Some(lsp::NumberOrString::String("missing-resource".to_owned()))
    );
    assert_eq!(problem.range.start.line, 2);
}

#[test]
fn document_updates_are_ordered_and_stale_versions_are_ignored() {
    let mut service = LanguageService::default();
    open(&mut service, "file:///a.adoc", 2, "= A");
    open(&mut service, "file:///b.adoc", 2, "= B");

    assert!(
        !change(
            &mut service,
            "file:///a.adoc",
            1,
            json!([{"text": "stale"}])
        )
        .expect("stale change")
    );
    assert_eq!(
        service
            .documents
            .get("file:///a.adoc")
            .expect("a")
            .analysis()
            .expect("analysis")
            .source(),
        "= A"
    );
    assert_eq!(
        service
            .documents
            .get("file:///b.adoc")
            .expect("b")
            .analysis()
            .expect("analysis")
            .source(),
        "= B"
    );
}

#[test]
fn incremental_changes_apply_sequentially_with_negotiated_positions() {
    let mut service = LanguageService::default();
    open(&mut service, "file:///a.adoc", 1, "a😀c");
    assert!(
        change(
            &mut service,
            "file:///a.adoc",
            2,
            json!([
                {
                    "range": {
                        "start": {"line": 0, "character": 1},
                        "end": {"line": 0,"character": 3}
                    },
                    "text": "b"
                },
                {
                    "range": {
                        "start": {"line": 0, "character": 2},
                        "end": {"line": 0,"character": 3}
                    },
                    "text": "d"
                }
            ]),
        )
        .expect("incremental change")
    );
    assert_eq!(
        service
            .documents
            .get("file:///a.adoc")
            .expect("document")
            .analysis()
            .expect("analysis")
            .source(),
        "abd"
    );
}

#[test]
fn incremental_changes_preserve_crlf_line_boundaries() {
    let mut service = LanguageService::default();
    open(&mut service, "file:///crlf.adoc", 1, "one\r\ntwo\r\n");
    assert!(
        change(
            &mut service,
            "file:///crlf.adoc",
            2,
            json!([{
                "range": {
                    "start": {"line": 1, "character": 0},
                    "end": {"line": 1, "character": 3}
                },
                "text": "second"
            }])
        )
        .expect("incremental change")
    );
    assert_eq!(
        service
            .documents
            .get("file:///crlf.adoc")
            .expect("document")
            .analysis()
            .expect("analysis")
            .source(),
        "one\r\nsecond\r\n"
    );
}

/// Planning a scan reads the roots without changing the service.
///
/// The walk runs on a worker while the event loop keeps answering requests, so
/// it must not touch state that the loop is free to change meanwhile. The
/// separation is the property under test: planning alone leaves the workspace
/// as it was, and only applying installs what was read.
#[test]
fn planning_a_workspace_scan_leaves_service_state_untouched() {
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let root: PathBuf = std::env::temp_dir().join(format!("adocweave-detached-scan-{unique}"));
    fs::create_dir_all(&root).expect("workspace");
    fs::write(root.join("found.adoc"), "= Found\n\n[[found]]\n== Found\n").expect("document");
    let root_uri = lsp::Url::from_directory_path(&root).expect("root URI");
    let found = lsp::Url::from_file_path(root.join("found.adoc")).expect("document URI");

    let mut service = LanguageService::default();
    let params = typed(json!({
        "processId": null,
        "rootUri": root_uri,
        "capabilities": {}
    }));
    service.initialize(&params);

    let scan = service.plan_workspace_scan(&adocweave::NeverCancel);
    assert!(
        service.workspace_analysis_count() == 0,
        "planning must not install what it read",
    );

    let jobs = service.apply_workspace_scan(scan).jobs;
    assert!(
        jobs.is_empty(),
        "no document is open, so nothing needs reanalysis",
    );
    assert!(service.workspace_resource(&found).is_some());

    fs::remove_dir_all(&root).expect("cleanup");
}

/// A document opened while the scan was running is not lost by applying it.
///
/// The read starts from the state at planning time. Overlaying the documents
/// open when the result lands, rather than the ones open when it started,
/// keeps an editor that opens a file immediately after initialization.
#[test]
fn a_document_opened_during_the_scan_survives_applying_it() {
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let root: PathBuf = std::env::temp_dir().join(format!("adocweave-scan-race-{unique}"));
    fs::create_dir_all(&root).expect("workspace");
    fs::write(root.join("on-disk.adoc"), "= On disk\n").expect("document");
    let root_uri = lsp::Url::from_directory_path(&root).expect("root URI");
    let opened = lsp::Url::from_file_path(root.join("on-disk.adoc")).expect("document URI");

    let mut service = LanguageService::default();
    let params = typed(json!({
        "processId": null,
        "rootUri": root_uri,
        "capabilities": {}
    }));
    service.initialize(&params);

    let scan = service.plan_workspace_scan(&adocweave::NeverCancel);
    // The client opens the file, with unsaved edits, before the walk lands.
    open(&mut service, opened.as_str(), 1, "= Edited in the editor\n");
    let jobs = service.apply_workspace_scan(scan).jobs;

    assert_eq!(jobs.len(), 1, "the open document is reanalyzed");
    assert_eq!(
        service
            .workspace_resource(&opened)
            .expect("open document")
            .as_ref(),
        "= Edited in the editor\n",
        "applying the scan must not replace the editor's text with disk text",
    );

    fs::remove_dir_all(&root).expect("cleanup");
}

#[test]
fn a_scan_worker_failure_keeps_the_last_coherent_workspace_and_reports_it() {
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let root = std::env::temp_dir().join(format!("adocweave-scan-worker-failure-{unique}"));
    fs::create_dir_all(&root).expect("workspace");
    let document_path = root.join("root.adoc");
    fs::write(&document_path, "= Root\n").expect("document");
    let root_uri = lsp::Url::from_directory_path(&root).expect("root URI");
    let document_uri = lsp::Url::from_file_path(&document_path).expect("document URI");

    let mut service = LanguageService::default();
    initialize_with_params(
        &mut service,
        typed(json!({
            "processId": null,
            "rootUri": root_uri,
            "capabilities": {}
        })),
    );
    open(&mut service, document_uri.as_str(), 1, "= Root\n");
    let previous = service
        .workspace_resource(&document_uri)
        .expect("workspace resource")
        .clone();

    let jobs = service.workspace_scan_failed("workspace scan worker failed: panic".to_owned());

    assert_eq!(jobs.len(), 1, "the open document must publish the failure");
    for job in jobs {
        adopt(&mut service, job);
    }
    assert_eq!(
        service
            .workspace_resource(&document_uri)
            .expect("retained workspace resource"),
        previous,
        "an internal worker failure must not replace the last coherent snapshot",
    );
    let diagnostics = service.diagnostics(&document_uri).expect("diagnostics");
    assert!(diagnostics.diagnostics.iter().any(|diagnostic| {
        diagnostic.code
            == Some(lsp::NumberOrString::String(
                "workspace-resource-error".to_owned(),
            ))
            && diagnostic.message.contains("workspace scan worker failed")
    }));

    fs::remove_dir_all(root).expect("cleanup");
}

#[test]
fn transient_scan_failure_is_published_and_cleared_after_recovery() {
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let root = std::env::temp_dir().join(format!("adocweave-scan-recovery-{unique}"));
    fs::create_dir_all(&root).expect("workspace");
    let config = root.join(adocweave_config::FILE_NAME);
    let document_path = root.join("root.adoc");
    fs::write(&config, "schema-version = 1\n").expect("configuration");
    fs::write(&document_path, "= Root\n").expect("document");
    let root_uri = lsp::Url::from_directory_path(&root).expect("root URI");
    let document_uri = lsp::Url::from_file_path(&document_path).expect("document URI");
    let mut service = LanguageService::default();
    initialize_with_params(
        &mut service,
        typed(json!({
            "processId": null,
            "rootUri": root_uri,
            "capabilities": {}
        })),
    );
    open(&mut service, document_uri.as_str(), 1, "= Root\n");
    let previous = service
        .workspace_resource(&document_uri)
        .expect("workspace resource")
        .clone();

    fs::remove_file(&config).expect("remove configuration");
    fs::create_dir(&config).expect("unreadable configuration entry");
    let failed = service.plan_workspace_scan(&adocweave::NeverCancel);
    let jobs = service.apply_workspace_scan(failed).jobs;

    assert_eq!(
        jobs.len(),
        1,
        "the retained snapshot failure must be published"
    );
    for job in jobs {
        adopt(&mut service, job);
    }
    assert_eq!(
        service
            .workspace_resource(&document_uri)
            .expect("retained workspace resource"),
        previous,
    );
    assert!(
        service
            .diagnostics(&document_uri)
            .expect("failure diagnostics")
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code
                == Some(lsp::NumberOrString::String(
                    "workspace-resource-error".to_owned()
                )))
    );

    fs::remove_dir(&config).expect("remove invalid configuration entry");
    fs::write(&config, "schema-version = 1\n").expect("restored configuration");
    let recovered = service.plan_workspace_scan(&adocweave::NeverCancel);
    let jobs = service.apply_workspace_scan(recovered).jobs;
    assert_eq!(jobs.len(), 1, "recovery must clear the published failure");
    for job in jobs {
        adopt(&mut service, job);
    }
    assert!(
        service
            .diagnostics(&document_uri)
            .expect("recovered diagnostics")
            .diagnostics
            .iter()
            .all(|diagnostic| diagnostic.code
                != Some(lsp::NumberOrString::String(
                    "workspace-resource-error".to_owned()
                )))
    );

    fs::remove_dir_all(root).expect("cleanup");
}
