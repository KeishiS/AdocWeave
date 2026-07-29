use std::ops::ControlFlow;

use super::{INVALID_TABLE, LintContext, LintDiagnosticBody, LintDiagnosticSink};

pub(super) fn lint_tables(context: &LintContext<'_>, sink: &mut LintDiagnosticSink<'_>) {
    lint_tables_with_observer(context.document(), sink, |_| {});
}

pub(super) fn lint_tables_with_observer<'document>(
    document: &'document crate::parser::AstDocument,
    sink: &mut LintDiagnosticSink<'_>,
    mut observe: impl FnMut(crate::walker::SemanticNode<'document>),
) {
    let _: ControlFlow<()> = crate::walker::try_walk_ast(document, |node| {
        observe(node);
        if sink.should_stop() {
            return ControlFlow::Break(());
        }
        let crate::walker::SemanticNode::Table(table) = node else {
            return ControlFlow::Continue(());
        };
        for problem in &table.problems {
            if sink.should_stop() {
                break;
            }
            let message = match problem.kind {
                crate::table::TableProblemKind::InvalidFormat => "unsupported table format",
                crate::table::TableProblemKind::InvalidSeparator => {
                    "table separator must be one non-control character and match the delimiter"
                }
                crate::table::TableProblemKind::UnclosedQuotedCell => "unclosed quoted table cell",
                crate::table::TableProblemKind::InvalidPresentation => {
                    "invalid or conflicting table presentation attribute"
                }
            };
            sink.emit(INVALID_TABLE, problem.range, || {
                LintDiagnosticBody::new(message)
            });
            if sink.should_stop() {
                return ControlFlow::Break(());
            }
        }
        ControlFlow::Continue(())
    });
}
