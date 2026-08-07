//! Every tracked AsciiDoc document must produce a textlint plan that spans the
//! whole source, so the repository's own documents keep exercising the plan
//! builder.

use std::fs;
use std::process::Command;

use adocweave::{AnalysisOptions, Engine};

fn repository_root() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn analyze(path: &str) -> adocweave::Analysis {
    let source = fs::read_to_string(repository_root().join(path))
        .unwrap_or_else(|error| panic!("{path}: {error}"));
    Engine::new(AnalysisOptions::default())
        .analyze(&source)
        .unwrap_or_else(|error| panic!("{path}: {error}"))
}

#[test]
fn tracked_adoc_corpus_builds_textlint_plans() {
    let output = Command::new("git")
        .args(["ls-files", "-z", "*.adoc"])
        .current_dir(repository_root())
        .output()
        .expect("git ls-files");
    assert!(output.status.success());
    for path in output
        .stdout
        .split(|byte| *byte == 0)
        .filter(|path| !path.is_empty())
    {
        let path = std::str::from_utf8(path).expect("UTF-8 repository path");
        let analysis = analyze(path);
        let plan = adocweave_textlint::plan(&analysis, adocweave_textlint::PlanLimits::default())
            .unwrap_or_else(|error| panic!("{path}: {error}"));
        assert_eq!(
            plan.range,
            adocweave_textlint::Utf16Range(
                0,
                u32::try_from(analysis.source().encode_utf16().count())
                    .expect("document UTF-16 length")
            ),
            "{path}"
        );
    }
}
