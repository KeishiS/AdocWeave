//! Stateless formatting and rename conversion over selected analyses.

use std::collections::HashMap;

use adocweave::Analysis;
use adocweave::output::formatter::{self, FormatConfig};
use adocweave::resolution::ReferenceKey;
use adocweave::semantic::{ReferenceTarget, ReferenceTargetKind, is_valid_anchor_id};
use async_lsp::lsp_types as lsp;

use crate::cancellation::{QueryCancellation, QueryResult};
use crate::position::{PositionEncoding, range_contains_offset, range_to_lsp, request_offset};

pub(crate) fn formatting(
    analysis: &Analysis,
    config: &FormatConfig,
    encoding: PositionEncoding,
    cancellation: &QueryCancellation,
) -> QueryResult<Vec<lsp::TextEdit>> {
    cancellation.check_now()?;
    let formatted = match formatter::format_analysis(analysis, config, cancellation) {
        Ok(formatted) => formatted,
        Err(formatter::FormatError::Cancelled) => {
            cancellation.check_now()?;
            unreachable!("formatter reports cancellation only when the query is cancelled")
        }
        Err(formatter::FormatError::Position(error)) => return Err(error.to_string().into()),
    };
    cancellation.check_now()?;
    let mut edits = Vec::with_capacity(formatted.edits.len());
    for edit in &formatted.edits {
        cancellation.checkpoint()?;
        edits.push(lsp::TextEdit::new(
            range_to_lsp(edit.range, analysis.source_document(), encoding)?,
            edit.replacement.clone(),
        ));
    }
    Ok(edits)
}

/// The anchor definition a rename started at this position would change.
///
/// `prepareRename` and `rename` must agree on which positions can be renamed:
/// an editor that is told a position is renameable and then receives no edit
/// looks broken. Both answers come from here, so they cannot diverge.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct RenameTarget {
    pub(crate) key: ReferenceKey,
    pub(crate) range: lsp::Range,
    pub(crate) placeholder: String,
}

pub(crate) fn renameable_anchor(
    analysis: &Analysis,
    position: lsp::Position,
    encoding: PositionEncoding,
) -> Result<Option<RenameTarget>, String> {
    let offset = request_offset(analysis.source_document(), position, encoding)?;
    let Some(target) = analysis.reference_targets().iter().find(|target| {
        range_contains_offset(target.id_range, offset) && has_explicit_id(analysis, target)
    }) else {
        return Ok(None);
    };
    let range = range_to_lsp(target.id_range, analysis.source_document(), encoding)?;
    Ok(Some(RenameTarget {
        key: ReferenceKey::Local {
            anchor: target.id.clone(),
        },
        range,
        placeholder: target.id.clone(),
    }))
}

fn has_explicit_id(analysis: &Analysis, target: &ReferenceTarget) -> bool {
    target.kind == ReferenceTargetKind::InlineAnchor
        || analysis
            .document()
            .anchors()
            .iter()
            .any(|anchor| anchor.valid && anchor.id_range == target.id_range)
}

pub(crate) fn rename_target(
    analysis: &Analysis,
    position: lsp::Position,
    new_name: &str,
    encoding: PositionEncoding,
) -> Result<Option<RenameTarget>, String> {
    if !is_valid_anchor_id(new_name) {
        return Ok(None);
    }
    let Some(target) = renameable_anchor(analysis, position, encoding)? else {
        return Ok(None);
    };
    if new_name != target.placeholder
        && analysis
            .reference_targets()
            .iter()
            .any(|candidate| candidate.id == new_name)
    {
        return Ok(None);
    }
    Ok(Some(target))
}

pub(crate) fn rename_edit(
    locations: Vec<lsp::Location>,
    new_name: &str,
) -> Option<lsp::WorkspaceEdit> {
    if locations.is_empty() {
        return None;
    }
    let mut changes = HashMap::<lsp::Url, Vec<lsp::TextEdit>>::new();
    for location in locations {
        changes
            .entry(location.uri)
            .or_default()
            .push(lsp::TextEdit::new(location.range, new_name.to_owned()));
    }
    Some(lsp::WorkspaceEdit {
        changes: Some(changes),
        ..lsp::WorkspaceEdit::default()
    })
}

#[cfg(test)]
mod tests {
    use adocweave::{Analysis, AnalysisOptions, AnalysisRequest, NeverCancel};

    use super::*;

    fn analyze(source: &str) -> Analysis {
        AnalysisRequest::new(None, 1, 1, source, AnalysisOptions::default())
            .analyze(&NeverCancel)
            .expect("analysis")
            .analysis
    }

    #[test]
    fn formatting_edits_follow_the_selected_position_encoding() {
        let analysis = analyze("before😀  \n");
        for (encoding, expected_start) in
            [(PositionEncoding::Utf8, 10), (PositionEncoding::Utf16, 8)]
        {
            let cancellation = crate::cancellation::test_cancellation();
            let edits = formatting(&analysis, &FormatConfig::default(), encoding, &cancellation)
                .expect("edits");
            assert_eq!(edits.len(), 1);
            assert_eq!(edits[0].range.start.character, expected_start);
        }
    }

    #[test]
    fn rename_target_rejects_invalid_names_and_builds_grouped_edits() {
        let analysis = analyze("[[target]]\n== Target\n");
        assert!(
            rename_target(
                &analysis,
                lsp::Position::new(0, 3),
                "not valid",
                PositionEncoding::Utf16,
            )
            .expect("invalid name")
            .is_none()
        );
        assert_eq!(
            rename_target(
                &analysis,
                lsp::Position::new(0, 3),
                "renamed",
                PositionEncoding::Utf16,
            )
            .expect("target")
            .expect("renameable target")
            .key,
            ReferenceKey::Local {
                anchor: "target".to_owned()
            }
        );

        for invalid in ["a,b", "a\"b", "a'b", "a&b", "a=b", "a(b", "a)b"] {
            assert!(
                rename_target(
                    &analysis,
                    lsp::Position::new(0, 3),
                    invalid,
                    PositionEncoding::Utf16,
                )
                .expect("invalid anchor name")
                .is_none(),
                "{invalid} must follow the parser's anchor rule",
            );
        }

        let implicit_heading = analyze("== Generated Heading\n");
        assert!(
            renameable_anchor(
                &implicit_heading,
                lsp::Position::new(0, 5),
                PositionEncoding::Utf16,
            )
            .expect("implicit heading lookup")
            .is_none(),
            "a generated heading ID has no authored identifier to rename",
        );

        let uri = lsp::Url::parse("file:///a.adoc").expect("URI");
        let edit = rename_edit(
            vec![lsp::Location::new(uri.clone(), lsp::Range::default())],
            "renamed",
        )
        .expect("workspace edit");
        assert_eq!(edit.changes.expect("changes")[&uri][0].new_text, "renamed");
    }
}
