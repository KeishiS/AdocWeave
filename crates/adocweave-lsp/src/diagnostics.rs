//! Stateless conversion of analysis diagnostics and fixes to LSP values.

use std::collections::HashMap;

use adocweave::output::diagnostics::{Applicability, Diagnostic, Severity};
use adocweave::text::{SourceDocument, TextRange};
use async_lsp::lsp_types as lsp;

use crate::position::{PositionEncoding, range_to_lsp, ranges_intersect};

#[derive(Clone, Copy)]
pub(super) struct QuickFixCapabilities {
    pub(super) versioned_document_changes: bool,
    pub(super) is_preferred: bool,
}

pub(super) fn analysis_diagnostic(
    uri: &lsp::Url,
    diagnostic: &Diagnostic,
    source_document: &SourceDocument,
    encoding: PositionEncoding,
) -> Result<lsp::Diagnostic, String> {
    let related_information = diagnostic
        .related
        .iter()
        .map(|related| {
            Ok(lsp::DiagnosticRelatedInformation {
                location: lsp::Location::new(
                    uri.clone(),
                    range_to_lsp(related.range, source_document, encoding)?,
                ),
                message: related.message.clone(),
            })
        })
        .collect::<Result<Vec<_>, String>>()?;
    diagnostic_at(
        DiagnosticFields {
            range: diagnostic.range,
            severity: diagnostic.severity,
            code: diagnostic.code.as_str(),
            source: "adocweave",
            message: &diagnostic.message,
            related_information: (!related_information.is_empty()).then_some(related_information),
        },
        source_document,
        encoding,
    )
}

pub(super) fn projected_diagnostic(
    range: TextRange,
    diagnostic: &Diagnostic,
    source_document: &SourceDocument,
    encoding: PositionEncoding,
) -> Result<lsp::Diagnostic, String> {
    diagnostic_at(
        DiagnosticFields {
            range,
            severity: diagnostic.severity,
            code: diagnostic.code.as_str(),
            source: "adocweave",
            message: &diagnostic.message,
            related_information: None,
        },
        source_document,
        encoding,
    )
}

pub(super) fn project_problem(
    range: TextRange,
    code: &str,
    message: &str,
    source_document: &SourceDocument,
    encoding: PositionEncoding,
) -> Result<lsp::Diagnostic, String> {
    diagnostic_at(
        DiagnosticFields {
            range,
            severity: Severity::Error,
            code,
            source: "adocweave-project",
            message,
            related_information: None,
        },
        source_document,
        encoding,
    )
}

pub(super) fn workspace_error(message: &str) -> lsp::Diagnostic {
    lsp::Diagnostic {
        range: lsp::Range::default(),
        severity: Some(lsp::DiagnosticSeverity::ERROR),
        code: Some(lsp::NumberOrString::String(
            "workspace-resource-error".to_owned(),
        )),
        source: Some("adocweave-project".to_owned()),
        message: message.to_owned(),
        ..lsp::Diagnostic::default()
    }
}

struct DiagnosticFields<'a> {
    range: TextRange,
    severity: Severity,
    code: &'a str,
    source: &'a str,
    message: &'a str,
    related_information: Option<Vec<lsp::DiagnosticRelatedInformation>>,
}

fn diagnostic_at(
    fields: DiagnosticFields<'_>,
    source_document: &SourceDocument,
    encoding: PositionEncoding,
) -> Result<lsp::Diagnostic, String> {
    Ok(lsp::Diagnostic {
        range: range_to_lsp(fields.range, source_document, encoding)?,
        severity: Some(match fields.severity {
            Severity::Error => lsp::DiagnosticSeverity::ERROR,
            Severity::Warning => lsp::DiagnosticSeverity::WARNING,
            Severity::Information => lsp::DiagnosticSeverity::INFORMATION,
            Severity::Hint => lsp::DiagnosticSeverity::HINT,
        }),
        code: Some(lsp::NumberOrString::String(fields.code.to_owned())),
        source: Some(fields.source.to_owned()),
        message: fields.message.to_owned(),
        related_information: fields.related_information,
        ..lsp::Diagnostic::default()
    })
}

pub(super) fn canonicalize(diagnostics: &mut Vec<lsp::Diagnostic>) {
    diagnostics.sort_by(|left, right| {
        (
            left.range.start.line,
            left.range.start.character,
            left.range.end.line,
            left.range.end.character,
            &left.message,
        )
            .cmp(&(
                right.range.start.line,
                right.range.start.character,
                right.range.end.line,
                right.range.end.character,
                &right.message,
            ))
    });
    diagnostics.dedup_by(|left, right| {
        left.range == right.range && left.code == right.code && left.message == right.message
    });
}

pub(super) fn quick_fixes(
    uri: &lsp::Url,
    version: i32,
    analysis: &adocweave::Analysis,
    requested_range: lsp::Range,
    encoding: PositionEncoding,
    capabilities: QuickFixCapabilities,
) -> Result<Vec<lsp::CodeActionOrCommand>, String> {
    let mut actions = Vec::new();
    for diagnostic in analysis.diagnostics() {
        let diagnostic_range =
            range_to_lsp(diagnostic.range, analysis.source_document(), encoding)?;
        if !ranges_intersect(requested_range, diagnostic_range) {
            continue;
        }
        for fix in &diagnostic.fixes {
            let edits = fix
                .edits()
                .iter()
                .map(|edit| {
                    Ok(lsp::OneOf::Left(lsp::TextEdit::new(
                        range_to_lsp(edit.range, analysis.source_document(), encoding)?,
                        edit.replacement.clone(),
                    )))
                })
                .collect::<Result<Vec<_>, String>>()?;
            let edit = if capabilities.versioned_document_changes {
                lsp::WorkspaceEdit {
                    document_changes: Some(lsp::DocumentChanges::Edits(vec![
                        lsp::TextDocumentEdit {
                            text_document: lsp::OptionalVersionedTextDocumentIdentifier {
                                uri: uri.clone(),
                                version: Some(version),
                            },
                            edits,
                        },
                    ])),
                    ..lsp::WorkspaceEdit::default()
                }
            } else {
                lsp::WorkspaceEdit {
                    changes: Some(HashMap::from([(
                        uri.clone(),
                        edits
                            .into_iter()
                            .map(|edit| match edit {
                                lsp::OneOf::Left(edit) => edit,
                                lsp::OneOf::Right(_) => {
                                    unreachable!("AdocWeave emits plain text edits")
                                }
                            })
                            .collect(),
                    )])),
                    ..lsp::WorkspaceEdit::default()
                }
            };
            actions.push(lsp::CodeActionOrCommand::CodeAction(lsp::CodeAction {
                title: fix.title.clone(),
                kind: Some(lsp::CodeActionKind::QUICKFIX),
                edit: Some(edit),
                is_preferred: capabilities
                    .is_preferred
                    .then_some(fix.applicability == Applicability::Always),
                ..lsp::CodeAction::default()
            }));
        }
    }
    Ok(actions)
}

#[cfg(test)]
mod tests {
    use super::*;
    use adocweave::text::{TextRange, TextSize};

    fn range(start: usize, end: usize) -> TextRange {
        TextRange::new(
            TextSize::new(start).expect("test offset"),
            TextSize::new(end).expect("test offset"),
        )
        .expect("ordered range")
    }

    #[test]
    fn conversion_preserves_utf8_utf16_order_and_related_information() {
        let source = "日😀 first\n日😀 second\n";
        let document = SourceDocument::new(source).expect("source document");
        let uri = lsp::Url::parse("file:///diagnostics.adoc").expect("URI");
        let diagnostic = Diagnostic {
            id: adocweave::output::diagnostics::DiagnosticId::new("second"),
            code: adocweave::output::diagnostics::DiagnosticCode::new("duplicate"),
            severity: Severity::Warning,
            message: "second definition".to_owned(),
            range: range(22, 28),
            related: vec![adocweave::output::diagnostics::RelatedInformation {
                message: "first definition".to_owned(),
                range: range(8, 13),
            }],
            fixes: Vec::new(),
        };

        let utf8 = analysis_diagnostic(&uri, &diagnostic, &document, PositionEncoding::Utf8)
            .expect("UTF-8 diagnostic");
        let utf16 = analysis_diagnostic(&uri, &diagnostic, &document, PositionEncoding::Utf16)
            .expect("UTF-16 diagnostic");
        assert_eq!(utf8.range.start, lsp::Position::new(1, 8));
        assert_eq!(utf16.range.start, lsp::Position::new(1, 4));
        let related = utf16.related_information.expect("related information");
        assert_eq!(related[0].location.uri, uri);
        assert_eq!(related[0].location.range.start, lsp::Position::new(0, 4));

        let mut diagnostics = vec![utf8.clone(), workspace_error("workspace"), utf8];
        canonicalize(&mut diagnostics);
        assert_eq!(diagnostics.len(), 2);
        assert_eq!(diagnostics[0].range, lsp::Range::default());
    }

    #[test]
    fn quick_fix_conversion_preserves_ranges_versions_and_preference() {
        let source = "==Title\ntext  \n";
        let analysis = adocweave::Engine::new(adocweave::AnalysisOptions::default())
            .analyze(source)
            .expect("analysis");
        let uri = lsp::Url::parse("file:///fix.adoc").expect("URI");
        let actions = quick_fixes(
            &uri,
            7,
            &analysis,
            lsp::Range::new(lsp::Position::new(0, 0), lsp::Position::new(1, 6)),
            PositionEncoding::Utf16,
            QuickFixCapabilities {
                versioned_document_changes: true,
                is_preferred: true,
            },
        )
        .expect("quick fixes");

        assert_eq!(actions.len(), 2);
        let lsp::CodeActionOrCommand::CodeAction(action) = &actions[0] else {
            panic!("code action");
        };
        assert_eq!(action.is_preferred, Some(true));
        let changes = action
            .edit
            .as_ref()
            .and_then(|edit| edit.document_changes.as_ref())
            .expect("versioned changes");
        let lsp::DocumentChanges::Edits(edits) = changes else {
            panic!("document edits");
        };
        assert_eq!(edits[0].text_document.version, Some(7));
        assert_eq!(edits[0].text_document.uri, uri);
    }
}
