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
    service.workspace_files_changed(typed(json!({
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

    let jobs = service.workspace_folders_changed(typed(json!({
        "event": {
            "removed": [{"uri": removed_uri, "name": "removed"}],
            "added": [{"uri": added_uri, "name": "added"}]
        }
    })));
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
    let jobs = service.workspace_files_changed(typed(json!({
        "changes": [{"uri": config_uri, "type": 2}]
    })));
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
    let jobs = service.workspace_files_changed(typed(json!({
        "changes": [{"uri": config_uri, "type": 2}]
    })));

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
    open(&mut service, document_uri.as_str(), 1, "trailing \n");

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
    service.workspace_folders_changed(typed(json!({
        "event": {"added": [{"uri": root_uri, "name": "root"}], "removed": []}
    })));
    open(&mut service, document_uri.as_str(), 1, source);
    assert!(service.documents.snapshot(document_uri.as_str()).is_some());

    fs::write(&config_path, "schema-version = 99\n").expect("invalid configuration");
    let jobs = service.workspace_files_changed(typed(json!({
        "changes": [{"uri": config_uri, "type": 2}]
    })));

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

    let jobs = service.workspace_files_changed(typed(json!({
        "changes": [{"uri": config_uri, "type": 2}]
    })));

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

    let scan = service.plan_workspace_scan().expect("roots to scan");
    assert!(
        service.workspace_analysis_count() == 0,
        "planning must not install what it read",
    );

    let jobs = service.apply_workspace_scan(scan);
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

    let scan = service.plan_workspace_scan().expect("roots to scan");
    // The client opens the file, with unsaved edits, before the walk lands.
    open(&mut service, opened.as_str(), 1, "= Edited in the editor\n");
    let jobs = service.apply_workspace_scan(scan);

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
