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

/// The search index and the textlint plan derive prose/code from one shared
/// table; this invariant catches a divergence end to end. A textlint code
/// block must never overlap a searchable prose segment, and a textlint prose
/// (`Str`) range must never overlap a searchable code segment. Inline code
/// inside a paragraph stays out of the comparison: both sides deliberately
/// treat it at different granularity (a `Code` node inside prose, one prose
/// search segment).
#[test]
fn tracked_adoc_corpus_classifies_prose_and_code_consistently() {
    use adocweave::output::projection::{SearchTextKind, searchable_text};
    use adocweave_textlint::{TxtAstNode, Utf16Range};

    fn collect(nodes: &[TxtAstNode], code: &mut Vec<Utf16Range>, prose: &mut Vec<Utf16Range>) {
        for node in nodes {
            match node {
                TxtAstNode::CodeBlock { range, .. } => code.push(*range),
                TxtAstNode::Str { range, .. } => prose.push(*range),
                TxtAstNode::Header { children, .. }
                | TxtAstNode::Paragraph { children, .. }
                | TxtAstNode::List { children, .. }
                | TxtAstNode::ListItem { children, .. }
                | TxtAstNode::BlockQuote { children, .. }
                | TxtAstNode::Table { children, .. }
                | TxtAstNode::TableRow { children, .. }
                | TxtAstNode::TableCell { children, .. }
                | TxtAstNode::Strong { children, .. }
                | TxtAstNode::Emphasis { children, .. }
                | TxtAstNode::Link { children, .. } => collect(children, code, prose),
                _ => {}
            }
        }
    }

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
        let mut code = Vec::new();
        let mut prose = Vec::new();
        collect(&plan.children, &mut code, &mut prose);

        // Convert byte offsets of the search segments into UTF-16 offsets so
        // both sides live in the plan's coordinate space.
        let source = analysis.source();
        let mut utf16_at_byte = vec![0_u32; source.len() + 1];
        let mut units = 0_u32;
        for (offset, character) in source.char_indices() {
            utf16_at_byte[offset] = units;
            units += character.len_utf16() as u32;
            utf16_at_byte[offset + 1..offset + character.len_utf8()].fill(units);
        }
        utf16_at_byte[source.len()] = units;

        let overlaps =
            |left: Utf16Range, right: (u32, u32)| -> bool { left.0 < right.1 && right.0 < left.1 };
        let segments = searchable_text(&analysis).segments;
        for segment in &segments {
            let range = (
                utf16_at_byte[segment.source_range.start().to_usize()],
                utf16_at_byte[segment.source_range.end().to_usize()],
            );
            match segment.kind {
                SearchTextKind::Prose => {
                    for code_range in &code {
                        assert!(
                            !overlaps(*code_range, range),
                            "{path}: textlintのコード範囲{code_range:?}が検索のProse区間{range:?}と重なっています"
                        );
                    }
                }
                SearchTextKind::Code => {
                    for prose_range in &prose {
                        assert!(
                            !overlaps(*prose_range, range),
                            "{path}: textlintの本文範囲{prose_range:?}が検索のCode区間{range:?}と重なっています"
                        );
                    }
                }
            }
        }
    }
}
