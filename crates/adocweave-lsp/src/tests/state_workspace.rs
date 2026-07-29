use super::*;

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
    service.initialize(&params);
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
    service.initialize(&params);
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
    service.initialize(&params);
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
    let result = service.initialize(&params);
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
    service.initialize(&typed(json!({
        "processId": null,
        "rootUri": root_uri,
        "capabilities": {}
    })));
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
    service.initialize(&typed(json!({
        "processId": null,
        "rootUri": root_uri,
        "capabilities": {}
    })));
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
    service.initialize(&typed(json!({
        "processId": null,
        "workspaceFolders": [
            {"uri": enabled_root_uri, "name": "enabled"},
            {"uri": disabled_root_uri, "name": "disabled"}
        ],
        "capabilities": {"workspace": {"workspaceFolders": true}}
    })));
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
    service.initialize(&typed(json!({
        "processId": null,
        "rootUri": root_uri,
        "capabilities": {}
    })));
    open(&mut service, document_uri.as_str(), 1, "trailing \n");

    let diagnostics = service.diagnostics(&document_uri).expect("diagnostics");
    assert!(diagnostics.diagnostics.iter().any(|diagnostic| {
        diagnostic.code
            == Some(lsp::NumberOrString::String(
                "workspace-input-error".to_owned(),
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
