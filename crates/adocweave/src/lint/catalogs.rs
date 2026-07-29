use crate::diagnostic::RelatedInformation;

use super::{INVALID_CATALOG, LintContext, LintDiagnosticBody, LintDiagnosticSink};

pub(super) fn lint_catalogs(context: &LintContext<'_>, sink: &mut LintDiagnosticSink<'_>) {
    let document = context.document();
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
