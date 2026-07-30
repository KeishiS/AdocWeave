use crate::diagnostic::RelatedInformation;

use super::{INVALID_CATALOG, LintContext, LintDiagnosticBody, LintDiagnosticSink};

pub(super) fn lint_catalogs(context: &LintContext<'_>, sink: &mut LintDiagnosticSink<'_>) {
    let document = context.document();
    // A citation without a key names nothing, so it would silently disappear
    // from the output instead of reaching the host's bibliography library.
    for citation in crate::citation::citations(document.resolved.facts().macros()) {
        if sink.should_stop() {
            break;
        }
        if citation.keys.iter().all(|key| key.value.trim().is_empty()) {
            sink.emit(INVALID_CATALOG, citation.range, || {
                LintDiagnosticBody::new("citation names no bibliography key")
            });
        }
    }
    for problem in document.catalogs().problems() {
        if sink.should_stop() {
            break;
        }
        let message = match problem.kind {
            crate::catalog::CatalogProblemKind::MissingFootnoteDefinition => {
                "named footnote definition does not exist"
            }
            crate::catalog::CatalogProblemKind::DuplicateFootnoteDefinition => {
                "duplicate named footnote definition"
            }
            crate::catalog::CatalogProblemKind::DuplicateBibliographyEntry => {
                "duplicate bibliography entry"
            }
            crate::catalog::CatalogProblemKind::EmptyIndexTerm => "index term is empty",
        };
        sink.emit(INVALID_CATALOG, problem.range, || {
            LintDiagnosticBody::new(message).with_related(
                problem
                    .related_range
                    .map(|range| RelatedInformation {
                        message: "first definition is here".to_owned(),
                        range,
                    })
                    .into_iter()
                    .collect(),
            )
        });
    }
}
