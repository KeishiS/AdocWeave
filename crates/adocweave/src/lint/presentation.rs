use std::ops::ControlFlow;

use crate::parser::AstBlock;

use super::{
    INVALID_ATTRIBUTE, INVALID_LIST_PRESENTATION, LintContext, LintDiagnosticBody,
    LintDiagnosticSink,
};

pub(super) fn lint_list_presentation(context: &LintContext<'_>, sink: &mut LintDiagnosticSink<'_>) {
    lint_list_presentation_with_observer(context.document(), sink, |_| {});
}

pub(super) fn lint_list_presentation_with_observer<'document>(
    document: &'document crate::parser::AstDocument,
    sink: &mut LintDiagnosticSink<'_>,
    mut observe: impl FnMut(crate::walker::SemanticNode<'document>),
) {
    let _: ControlFlow<()> = crate::walker::try_walk_ast(document, |node| {
        observe(node);
        if sink.should_stop() {
            return ControlFlow::Break(());
        }
        let crate::walker::SemanticNode::Block(AstBlock::List(list)) = node else {
            return ControlFlow::Continue(());
        };
        for problem in &list.presentation_problems {
            if sink.should_stop() {
                break;
            }
            let message = match problem.kind {
                crate::parser::ListPresentationProblemKind::InvalidStart => {
                    "ordered list start must be a positive integer"
                }
                crate::parser::ListPresentationProblemKind::InvalidExplicitNumber => {
                    "explicit ordered-list number must be a positive 32-bit integer"
                }
                crate::parser::ListPresentationProblemKind::InconsistentExplicitNumber => {
                    "explicit ordered-list numbers must be sequential"
                }
                crate::parser::ListPresentationProblemKind::UnknownOrderedStyle => {
                    "unsupported ordered list style"
                }
            };
            sink.emit(INVALID_LIST_PRESENTATION, problem.range, || {
                LintDiagnosticBody::new(message)
            });
            if sink.should_stop() {
                return ControlFlow::Break(());
            }
        }
        ControlFlow::Continue(())
    });
}

pub(super) fn lint_document_presentation(
    context: &LintContext<'_>,
    sink: &mut LintDiagnosticSink<'_>,
) {
    let document = context.document();
    if let Some(range) = document.presentation().toc_policy().invalid_level_range {
        sink.emit(INVALID_ATTRIBUTE, range, || {
            LintDiagnosticBody::new("toclevels must be an integer from 1 to 5")
        });
    }
}
