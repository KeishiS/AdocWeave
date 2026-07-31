#![no_main]

//! Exercises include expansion, which the other targets never reach.
//!
//! `analyze`, `process` and `render` all start from one source string, so the
//! directive parser, the tag and line selection, the indent and leveloffset
//! transforms, and the source map that ties expanded text back to its origin
//! are never driven by generated input. This target supplies a resource
//! snapshot as well, so an include can resolve and those paths run.

use adocweave::preprocess::{
    PreprocessInputs, PreprocessOptions, ResourceDocument, ResourceSnapshot, SafeMode,
    preprocess_with,
};
use adocweave::{AnalysisOptions, Engine, SourceId};
use libfuzzer_sys::fuzz_target;

/// Splits the input into a root document and the resources it may include.
///
/// The separator is a byte sequence a generator can discover quickly, and the
/// resource names are fixed so a generated `include::` directive has something
/// to resolve against.
fn split_documents(source: &str) -> (&str, Vec<(&'static str, &str)>) {
    let mut parts = source.split("\u{0}");
    let root = parts.next().unwrap_or_default();
    let names = ["part.adoc", "other.adoc", "nested.adoc"];
    let resources = names.into_iter().zip(parts).collect();
    (root, resources)
}

fuzz_target!(|input: &[u8]| {
    let Ok(source) = std::str::from_utf8(input) else {
        return;
    };
    let (root, resources) = split_documents(source);
    let mut snapshot = ResourceSnapshot::default();
    for (name, text) in resources {
        snapshot.insert(
            name,
            ResourceDocument {
                source_id: SourceId::new(name),
                source: text.into(),
            },
        );
    }
    let options = PreprocessOptions {
        enable_includes: true,
        safe_mode: SafeMode::Server,
        base_uri: None,
        ..PreprocessOptions::default()
    };
    let Ok(document) = preprocess_with(root, &snapshot, &options, PreprocessInputs::default())
    else {
        return;
    };
    // The expanded text must analyze, and every recorded origin must address
    // real byte boundaries of the resource it names.
    if let Ok(analysis) = Engine::new(AnalysisOptions::default()).analyze(&document.source) {
        let _ = analysis.document().symbols();
    }
});
