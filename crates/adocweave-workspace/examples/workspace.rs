use std::collections::BTreeSet;

use adocweave::AnalysisOptions;
use adocweave::preprocess::{PreprocessOptions, ProjectionLimits, SafeMode};
use adocweave_workspace::{NeverCancelled, ResourceId, Revision, Workspace, WorkspaceLimits};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let root = ResourceId::new("file:///manual/index.adoc")?;
    let part = ResourceId::new("file:///manual/part.adoc")?;
    let mut workspace = Workspace::new(WorkspaceLimits::default());
    workspace.upsert_disk(root.clone(), Revision::new(1), "include::part.adoc[]\n")?;
    workspace.upsert_disk(part, Revision::new(1), "Reusable text.\n")?;
    workspace.register_root(root.clone())?;

    let mut allowed_schemes = BTreeSet::new();
    allowed_schemes.insert("file".to_owned());
    let preprocess_options = PreprocessOptions {
        base_uri: Some("file:///manual/".to_owned()),
        safe_mode: SafeMode::Server,
        allowed_schemes,
        ..PreprocessOptions::default()
    };
    let analysis = workspace.snapshot().analyze(
        &root,
        &AnalysisOptions::default(),
        &preprocess_options,
        ProjectionLimits::default(),
        &NeverCancelled,
    )?;
    workspace.accept(&analysis)?;

    println!(
        "{} diagnostic(s)",
        analysis.counts.errors + analysis.counts.warnings
    );
    Ok(())
}
