use std::collections::BTreeMap;

use crate::diagnostic::RelatedInformation;
use crate::document::heading_id_base;
use crate::parser::{AstBlock, HeadingKind};
use crate::source::TextRange;

use super::{
    DUPLICATE_HEADING_ID, INVALID_DOCUMENT_STRUCTURE, INVALID_HEADING_LEVEL, LintDiagnosticBody,
    LintDiagnosticSink,
};

pub(super) fn lint_headings(
    document: &crate::parser::AstDocument,
    sink: &mut LintDiagnosticSink<'_>,
) {
    let mut previous_level = None;
    let mut ids = BTreeMap::<String, TextRange>::new();

    for block in document.blocks() {
        if sink.is_full() {
            break;
        }
        let AstBlock::Heading(heading) = block else {
            continue;
        };

        let structurally_invalid = !heading.hierarchy_valid;
        match heading.kind {
            HeadingKind::DocumentTitle => {
                previous_level = None;
            }
            HeadingKind::Part => previous_level = None,
            HeadingKind::Discrete { .. } => {}
            HeadingKind::Section { level } => {
                let hierarchy_invalid =
                    previous_level.map_or(level > 1, |previous| level > previous + 1);
                if !structurally_invalid && hierarchy_invalid {
                    sink.emit(INVALID_HEADING_LEVEL, heading.marker_range, || {
                        LintDiagnosticBody::new("heading level skips the expected hierarchy")
                    });
                }
                previous_level = Some(level);
            }
        }

        let base = heading_id_base(&heading.text);
        if let Some(first_range) = ids.get(&base).copied() {
            sink.emit(DUPLICATE_HEADING_ID, heading.text_range, || {
                LintDiagnosticBody::new(format!("duplicate generated heading ID `{base}`"))
                    .with_related(vec![RelatedInformation {
                        message: "first heading with this ID".to_owned(),
                        range: first_range,
                    }])
            });
        } else {
            ids.insert(base, heading.text_range);
        }
    }
}

pub(super) fn lint_document_structure(
    document: &crate::parser::AstDocument,
    sink: &mut LintDiagnosticSink<'_>,
) {
    for problem in document.structure().problems() {
        if sink.is_full() {
            break;
        }
        let message = match problem.kind {
            crate::structure::StructureProblemKind::AppendixLevel => {
                "appendix must be a level-one section"
            }
            crate::structure::StructureProblemKind::AppendixDoctype => {
                "appendix is only valid for article or book documents"
            }
            crate::structure::StructureProblemKind::BibliographyNotSection => {
                "bibliography must be a section, not a document title or discrete heading"
            }
            crate::structure::StructureProblemKind::BibliographyScope => {
                "whole-book bibliography must be a level-zero section in a multipart book"
            }
            crate::structure::StructureProblemKind::BibliographyDoctype => {
                "bibliography is only valid for article or book documents"
            }
            crate::structure::StructureProblemKind::MissingManpageTitle => {
                "manpage document title is missing"
            }
            crate::structure::StructureProblemKind::InvalidManpageTitle => {
                "manpage title must use name(section)"
            }
            crate::structure::StructureProblemKind::MissingManpageNameSection => {
                "manpage NAME section is missing"
            }
            crate::structure::StructureProblemKind::InvalidManpagePurpose => {
                "manpage NAME paragraph must use name - purpose"
            }
        };
        sink.emit(INVALID_DOCUMENT_STRUCTURE, problem.range, || {
            LintDiagnosticBody::new(message)
        });
    }
}
