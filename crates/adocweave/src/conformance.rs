//! Stable, backend-neutral products used by cross-runtime conformance tests.

use std::fmt::Write as _;

use crate::Analysis;
use crate::diagnostic::render_json as render_diagnostics_json;
use crate::document::{document_symbols, render_symbols_json};
use crate::html::{RenderPolicy, ResolvedReference, render_with_resolutions};
use crate::projection::project;

pub const CONFORMANCE_CONTRACT_VERSION: u16 = 1;

/// Canonical products derived from exactly one owned analysis snapshot.
///
/// Strings are used at this boundary so native, WASM, and non-Rust hosts compare
/// the same bytes without depending on host object-key ordering.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ConformanceSnapshot {
    pub contract_version: u16,
    pub cst: String,
    pub ast: String,
    pub diagnostics_json: String,
    pub symbols_json: String,
    pub projection_json: String,
    pub html: String,
}

pub fn snapshot(
    analysis: &Analysis,
    policy: &RenderPolicy,
    resolutions: &[ResolvedReference],
) -> ConformanceSnapshot {
    ConformanceSnapshot {
        contract_version: CONFORMANCE_CONTRACT_VERSION,
        cst: canonical_cst(analysis),
        ast: format!("{:#?}", analysis.ast),
        diagnostics_json: render_diagnostics_json(&analysis.diagnostics),
        symbols_json: render_symbols_json(&document_symbols(&analysis.ast)),
        projection_json: project(analysis, resolutions).render_json(),
        html: render_with_resolutions(&analysis.ast, policy, resolutions).html,
    }
}

fn canonical_cst(analysis: &Analysis) -> String {
    let mut output = analysis.cst.snapshot();
    output.push_str("Tokens\n");
    for token in analysis.cst.tokens() {
        writeln!(
            output,
            "  {:?}@{}..{}",
            token.kind,
            token.range.start().to_u32(),
            token.range.end().to_u32()
        )
        .expect("writing to a String cannot fail");
    }
    output
}

#[cfg(test)]
mod tests {
    use crate::{Engine, ParseOptions};

    use super::*;

    #[test]
    fn snapshot_is_deterministic_and_owns_every_product() {
        let analysis = Engine::new(ParseOptions::default())
            .analyze("= Title\n\n[[target]]\n== Section\n\n<<target,Here>>\n")
            .expect("analysis");
        let first = snapshot(&analysis, &RenderPolicy::default(), &[]);
        let second = snapshot(&analysis, &RenderPolicy::default(), &[]);

        assert_eq!(first, second);
        assert_eq!(first.contract_version, CONFORMANCE_CONTRACT_VERSION);
        assert!(first.cst.contains("Document@"));
        assert!(first.ast.contains("ExplicitAnchor"));
        assert!(first.ast.contains("Reference"));
        assert!(first.projection_json.contains("referenceEdges"));
        assert!(first.html.contains("href=\"#target\""));
    }
}
