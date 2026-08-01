//! Attribute names written in a preprocessor directive.
//!
//! AsciiDoc attribute names do not distinguish case, and every attribute map is
//! keyed by the folded name. Directives searched with the name as written, so
//! only the lower-case spelling matched: a caller who supplied `Web` and wrote
//! `ifdef::Web[]` got no match, while the body of the same document resolved
//! `{WEB}` to the value. These tests hold the four directives to the rule the
//! body already follows.

use adocweave::preprocess::{PreprocessOptions, ResourceDocument, ResourceSnapshot, preprocess};
use adocweave::{AnalysisOptions, Engine, SourceId};

fn preprocessed(source: &str, name: &str, value: &str) -> String {
    let mut snapshot = ResourceSnapshot::default();
    snapshot.insert(
        "part.adoc",
        ResourceDocument {
            source_id: SourceId::new("part"),
            source: "Included\n".into(),
        },
    );
    let mut options = PreprocessOptions {
        source_id: Some(SourceId::new("root")),
        ..PreprocessOptions::default()
    };
    options
        .attributes
        .insert(name.to_owned(), Some(value.to_owned()));
    preprocess(source, &snapshot, &options)
        .expect("preprocess")
        .source
}

/// Every spelling of the same name selects the same branch.
#[test]
fn ifdef_matches_regardless_of_how_the_name_is_written() {
    for written in ["Web", "WEB", "web", "wEb"] {
        assert_eq!(
            preprocessed(
                &format!("ifdef::{written}[]\nBody\nendif::[]\n"),
                "Web",
                "yes"
            ),
            "Body\n",
            "ifdef::{written}[] with the attribute supplied as Web"
        );
    }
}

#[test]
fn ifndef_treats_a_supplied_attribute_as_present_in_every_spelling() {
    for written in ["Web", "WEB", "web"] {
        assert_eq!(
            preprocessed(
                &format!("ifndef::{written}[]\nBody\nendif::[]\n"),
                "Web",
                "yes"
            ),
            "",
            "ifndef::{written}[] must find the attribute the caller supplied"
        );
    }
}

#[test]
fn ifeval_expands_the_reference_in_every_spelling() {
    for written in ["Web", "WEB", "web"] {
        assert_eq!(
            preprocessed(
                &format!("ifeval::[\"{{{written}}}\" == \"yes\"]\nBody\nendif::[]\n"),
                "Web",
                "yes"
            ),
            "Body\n",
            "ifeval with {{{written}}}"
        );
    }
}

#[test]
fn include_expands_the_target_reference_in_every_spelling() {
    for written in ["Part", "PART", "part"] {
        assert_eq!(
            preprocessed(&format!("include::{{{written}}}.adoc[]\n"), "Part", "part"),
            "Included\n",
            "include::{{{written}}}.adoc[]"
        );
    }
}

/// The directive and the body of one document agree on what a name means.
#[test]
fn a_directive_and_the_body_resolve_the_same_attribute() {
    assert_eq!(
        preprocessed("ifdef::Web[]\nBody\nendif::[]\n", "Web", "yes"),
        "Body\n"
    );

    let mut analysis_options = AnalysisOptions::default();
    analysis_options
        .attributes
        .insert("Web".to_owned(), Some("yes".to_owned()));
    let analysis = Engine::new(analysis_options)
        .analyze("= T\n\nBody {WEB}\n")
        .expect("analysis");
    assert_eq!(
        analysis
            .attribute_environment()
            .final_values()
            .get("web")
            .map(String::as_str),
        Some("yes")
    );
}

/// A name that differs by more than case still names a different attribute.
#[test]
fn folding_case_does_not_merge_distinct_names() {
    assert_eq!(
        preprocessed("ifdef::Website[]\nBody\nendif::[]\n", "Web", "yes"),
        ""
    );
}
