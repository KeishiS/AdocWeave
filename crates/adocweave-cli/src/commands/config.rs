use adocweave::output::diagnostics as diagnostic;
use adocweave::output::html::HtmlDocumentMode;

pub(crate) fn render(
    snapshot: Option<&adocweave_config::ConfigSnapshot>,
    config: &adocweave_config::ResolvedProjectConfig,
) -> String {
    serde_json::to_string_pretty(&resolved_config_json(snapshot, config))
        .expect("resolved configuration is serializable")
}

fn resolved_config_json(
    snapshot: Option<&adocweave_config::ConfigSnapshot>,
    config: &adocweave_config::ResolvedProjectConfig,
) -> serde_json::Value {
    let attributes = config
        .analysis
        .attributes
        .iter()
        .map(|(name, value)| {
            (
                name.clone(),
                serde_json::json!({ "state": if value.is_some() { "set" } else { "unset" } }),
            )
        })
        .collect::<serde_json::Map<_, _>>();
    let rules = diagnostic::LINT_RULES
        .iter()
        .map(|descriptor| {
            let settings = config.analysis.diagnostics.lint.rule(descriptor.id);
            (
                descriptor.id.as_str().to_owned(),
                serde_json::json!({
                    "enabled": settings.enabled,
                    "severity": settings.severity.as_str(),
                }),
            )
        })
        .collect::<serde_json::Map<_, _>>();
    let path = |path: &std::path::Path| path.to_string_lossy().into_owned();
    serde_json::json!({
        "schemaVersion": config.schema_version,
        "source": snapshot.map(|snapshot| path(&snapshot.path)),
        "analysis": {
            "syntaxMode": match config.analysis.syntax.syntax_mode {
                adocweave::SyntaxMode::Permissive => "permissive",
                adocweave::SyntaxMode::Strict => "strict",
            },
            "attributes": attributes,
        },
        "lint": {
            "rules": rules,
            "maxLineLength": config.analysis.diagnostics.lint.max_line_length,
            "maxConsecutiveBlankLines":
                config.analysis.diagnostics.lint.max_consecutive_blank_lines,
            "maxDiagnostics": config.analysis.diagnostics.lint.max_diagnostics,
        },
        "resources": {
            "include": config.resources.include,
            "roots": config.resources.roots.iter().map(|value| path(value)).collect::<Vec<_>>(),
            "maxFiles": config.resources.limits.max_files,
            "maxTotalBytes": config.resources.limits.max_total_bytes,
            "maxResourceBytes": config.resources.limits.max_resource_bytes,
        },
        "localTargets": {
            "enabled": config.local_targets.enabled,
            "projectRoot": config.local_targets.project_root.as_deref().map(path),
        },
        "format": {
            "newline": match config.format.newline {
                adocweave::output::formatter::NewlineStyle::Lf => "lf",
                adocweave::output::formatter::NewlineStyle::CrLf => "cr-lf",
            },
            "finalNewline": config.format.final_newline,
            "maxConsecutiveBlankLines": config.format.max_consecutive_blank_lines,
        },
        "html": {
            "complete": config.html.policy.document_mode == HtmlDocumentMode::Complete,
            "stylesheetFiles":
                config.html.stylesheet_files.iter().map(|value| path(value)).collect::<Vec<_>>(),
            "stylesheetUrls": config.html.stylesheet_urls,
        }
    })
}
