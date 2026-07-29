use adocweave::output::diagnostics as diagnostic;
use adocweave::output::html::HtmlDocumentMode;

#[derive(Debug, Eq, PartialEq)]
pub(crate) struct Outcome {
    pub(crate) output: String,
}

pub(crate) fn run(snapshot: Option<&adocweave_config::ConfigSnapshot>) -> Outcome {
    let default_config;
    let config = if let Some(snapshot) = snapshot {
        &snapshot.config
    } else {
        default_config = adocweave_config::ResolvedProjectConfig::default();
        &default_config
    };
    Outcome {
        output: serde_json::to_string_pretty(&resolved_config_json(snapshot, config))
            .expect("resolved configuration is serializable"),
    }
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
            "maxFiles": config.resources.limit_plan.filesystem_reads.max_files,
            "maxTotalBytes": config.resources.limit_plan.filesystem_reads.max_total_bytes,
            "maxResourceBytes": config.resources.limit_plan.filesystem_reads.max_resource_bytes,
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

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    #[test]
    fn missing_snapshot_uses_defaults_without_inventing_a_source() {
        let outcome = run(None);
        let value: serde_json::Value = serde_json::from_str(&outcome.output).expect("config JSON");

        assert!(value["source"].is_null());
        assert_eq!(value["schemaVersion"], 1);
        assert_eq!(value["resources"]["include"], false);
        assert_eq!(value["analysis"]["attributes"], serde_json::json!({}));
    }

    #[test]
    fn snapshot_redacts_values_and_internal_content_identity() {
        let mut config = adocweave_config::ResolvedProjectConfig::default();
        config
            .analysis
            .attributes
            .insert("token".to_owned(), Some("do-not-print".to_owned()));
        config.analysis.attributes.insert("hidden".to_owned(), None);
        let snapshot = adocweave_config::ConfigSnapshot {
            path: PathBuf::from("/project/.adocweave.toml"),
            content_sha256: [0xab; 32],
            config,
        };

        let outcome = run(Some(&snapshot));
        assert!(!outcome.output.contains("do-not-print"));
        assert!(!outcome.output.contains("abababab"));
        let value: serde_json::Value = serde_json::from_str(&outcome.output).expect("config JSON");
        assert_eq!(value["analysis"]["attributes"]["token"]["state"], "set");
        assert_eq!(value["analysis"]["attributes"]["hidden"]["state"], "unset");
    }

    #[test]
    fn attribute_names_are_serialized_as_data() {
        let hostile_name = "quote\"\nkey";
        let mut config = adocweave_config::ResolvedProjectConfig::default();
        config.analysis.attributes.insert(
            hostile_name.to_owned(),
            Some("still-do-not-print".to_owned()),
        );
        let snapshot = adocweave_config::ConfigSnapshot {
            path: PathBuf::from("/project/.adocweave.toml"),
            content_sha256: [0; 32],
            config,
        };

        let outcome = run(Some(&snapshot));
        let value: serde_json::Value = serde_json::from_str(&outcome.output).expect("config JSON");

        assert_eq!(
            value["analysis"]["attributes"][hostile_name]["state"],
            "set"
        );
        assert!(!outcome.output.contains("still-do-not-print"));
    }
}
