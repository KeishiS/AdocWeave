use std::collections::BTreeMap;
use std::sync::Arc;

use adocweave::output::diagnostics::PROTECTED_ATTRIBUTE;
use adocweave::preprocess::{
    EffectiveProcessingOptions, PreprocessErrorKind, PreprocessOptions, ProcessingOptionsError,
    ResourceDocument, ResourceSnapshot, preprocess, preprocess_and_analyze,
    preprocess_and_analyze_with_options,
};
use adocweave::{AnalysisOptions, Engine, SourceId};

fn matching_options() -> (AnalysisOptions, PreprocessOptions) {
    let attributes = BTreeMap::from([("selected".to_owned(), Some("part".to_owned()))]);
    let mut analysis = AnalysisOptions::default();
    analysis.attributes.clone_from(&attributes);
    let preprocess = PreprocessOptions {
        attributes,
        max_attribute_expansion_depth: analysis.syntax.limits.max_attribute_expansion_depth,
        max_attribute_expansion_bytes: analysis.syntax.limits.max_attribute_expansion_bytes,
        ..PreprocessOptions::default()
    };
    (analysis, preprocess)
}

fn snapshot() -> ResourceSnapshot {
    [(
        "part.adoc".to_owned(),
        ResourceDocument {
            source_id: SourceId::new("part"),
            source: Arc::from(":selected: other\nincluded {selected}\n"),
        },
    )]
    .into_iter()
    .collect()
}

#[test]
fn one_effective_contract_drives_conditionals_includes_analysis_and_protected_attributes() {
    let (analysis, preprocess) = matching_options();
    let options = EffectiveProcessingOptions::new(analysis, preprocess).expect("matching settings");
    let result = preprocess_and_analyze_with_options(
        "ifdef::selected[]\ninclude::{selected}.adoc[]\nendif::[]\n",
        &snapshot(),
        &options,
    )
    .expect("processing");

    assert!(result.document.source.contains("included {selected}"));
    assert_eq!(
        result
            .analysis
            .attribute_environment()
            .final_values()
            .get("selected")
            .map(String::as_str),
        Some("part")
    );
    assert!(
        result
            .analysis
            .diagnostics()
            .iter()
            .any(|diagnostic| { diagnostic.code.as_str() == PROTECTED_ATTRIBUTE.as_str() })
    );
}

#[test]
fn compatibility_entry_rejects_every_shared_setting_mismatch_before_preprocessing() {
    for (expected, change) in [
        (ProcessingOptionsError::ExternalAttributes, 0_u8),
        (ProcessingOptionsError::AttributeExpansionDepth, 1_u8),
        (ProcessingOptionsError::AttributeExpansionBytes, 2_u8),
    ] {
        let (analysis, mut preprocess) = matching_options();
        match change {
            0 => {
                preprocess.attributes.clear();
            }
            1 => {
                preprocess.max_attribute_expansion_depth += 1;
            }
            2 => {
                preprocess.max_attribute_expansion_bytes += 1;
            }
            _ => unreachable!(),
        }
        let error = preprocess_and_analyze(
            &Engine::new(analysis),
            "include::missing.adoc[]\n",
            &ResourceSnapshot::default(),
            &preprocess,
        )
        .expect_err("mismatched settings");
        assert!(matches!(
            error,
            adocweave::preprocess::PreprocessedAnalysisError::Options(actual)
                if actual == expected
        ));
    }
}

#[test]
fn preprocessing_uses_the_caller_attribute_expansion_boundaries() {
    let source = ":base: 12345\n:expanded: {base}\ninclude::{expanded}.adoc[]\n";
    let resources = [(
        "12345.adoc".to_owned(),
        ResourceDocument {
            source_id: SourceId::new("included"),
            source: Arc::from("included\n"),
        },
    )]
    .into_iter()
    .collect::<ResourceSnapshot>();
    for (depth, bytes, expected) in [
        (1, 5, Ok(())),
        (0, 5, Err(PreprocessErrorKind::MissingResource)),
        (1, 4, Err(PreprocessErrorKind::MissingResource)),
    ] {
        let options = PreprocessOptions {
            max_attribute_expansion_depth: depth,
            max_attribute_expansion_bytes: bytes,
            ..PreprocessOptions::default()
        };
        match expected {
            Ok(()) => {
                preprocess(source, &resources, &options).expect("accepted boundaries");
            }
            Err(kind) => {
                assert_eq!(
                    preprocess(source, &resources, &options)
                        .expect_err("attribute expansion boundary")
                        .kind,
                    kind
                );
            }
        }
    }
}
