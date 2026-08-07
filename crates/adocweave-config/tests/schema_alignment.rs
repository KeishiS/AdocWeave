//! The JSON Schema and the Rust implementation are two hand-written answers to
//! the same configuration questions. These tests keep them agreeing with each
//! other and with the published lint rule catalog.

use std::fs;

use serde::Deserialize;

fn repository_root() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

#[test]
fn project_config_schema_lists_every_lint_rule() {
    let root = repository_root();
    let schema: serde_json::Value = serde_json::from_str(
        &fs::read_to_string(root.join("config/adocweave.schema.json"))
            .expect("project configuration schema"),
    )
    .expect("valid project configuration schema");
    let schema_rules = schema["properties"]["lint"]["properties"]["rules"]["propertyNames"]["enum"]
        .as_array()
        .expect("lint rule name enum")
        .iter()
        .map(|value| value.as_str().expect("lint rule name"))
        .collect::<Vec<_>>();
    let mut catalog_rules = adocweave::output::diagnostics::LINT_RULES
        .iter()
        .map(|rule| rule.id.as_str())
        .collect::<Vec<_>>();
    catalog_rules.sort_unstable();

    assert!(schema_rules.windows(2).all(|pair| pair[0] < pair[1]));
    assert_eq!(schema_rules, catalog_rules);
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ConfigCorpus {
    schema_version: u8,
    cases: Vec<ConfigCorpusCase>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ConfigCorpusCase {
    name: String,
    config: serde_json::Value,
    accepted: bool,
    #[serde(default)]
    schema_only_accepts: bool,
    #[serde(default)]
    reason: Option<String>,
}

#[test]
fn the_configuration_corpus_matches_the_implementation() {
    // `tools/config-schema.test.mjs` asks the schema; this asks the
    // implementation, so a constraint added to one and not the other is
    // reported instead of leaving an editor to accept what the run rejects.
    let root = repository_root();
    let corpus: ConfigCorpus = serde_json::from_str(
        &fs::read_to_string(root.join("fixtures/config/schema-corpus.json"))
            .expect("configuration corpus"),
    )
    .expect("valid configuration corpus");
    assert_eq!(corpus.schema_version, 1);
    assert!(!corpus.cases.is_empty());

    let mut disagreements = Vec::new();
    for case in &corpus.cases {
        let source = toml::to_string(&case.config).unwrap_or_else(|error| {
            panic!("{}: corpusの設定をTOMLへ変換できません: {error}", case.name)
        });
        let accepted = adocweave_config::ResolvedProjectConfig::parse(
            &source,
            std::path::Path::new("/workspace"),
        )
        .is_ok();
        if accepted != case.accepted {
            disagreements.push(format!(
                "{}: 実装={} corpus={}",
                case.name,
                if accepted { "受理" } else { "拒否" },
                if case.accepted { "受理" } else { "拒否" },
            ));
        }
        if case.schema_only_accepts {
            assert!(!case.accepted, "{}", case.name);
            assert!(case.reason.is_some(), "{}: 理由がありません", case.name);
        }
    }
    assert_eq!(disagreements, Vec::<String>::new());
}
