//! Stateless formatting and rename conversion over selected analyses.

use std::collections::HashMap;

use adocweave::Analysis;
use adocweave::output::formatter::{self, FormatConfig};
use adocweave::resolution::ReferenceKey;
use async_lsp::lsp_types as lsp;

use crate::position::{PositionEncoding, range_contains_offset, range_to_lsp, request_offset};

pub(crate) fn formatting(
    analysis: &Analysis,
    config: &FormatConfig,
    encoding: PositionEncoding,
) -> Result<Vec<lsp::TextEdit>, String> {
    formatter::format_analysis(analysis, config)
        .map_err(|error| error.to_string())?
        .edits
        .iter()
        .map(|edit| {
            Ok(lsp::TextEdit::new(
                range_to_lsp(edit.range, analysis.source_document(), encoding)?,
                edit.replacement.clone(),
            ))
        })
        .collect()
}

pub(crate) fn rename_target(
    analysis: &Analysis,
    position: lsp::Position,
    new_name: &str,
    encoding: PositionEncoding,
) -> Result<Option<ReferenceKey>, String> {
    if !valid_anchor_name(new_name) {
        return Ok(None);
    }
    let offset = request_offset(analysis.source_document(), position, encoding)?;
    Ok(analysis
        .reference_targets()
        .iter()
        .find(|target| range_contains_offset(target.id_range, offset))
        .map(|target| ReferenceKey::Local {
            anchor: target.id.clone(),
        }))
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

fn valid_anchor_name(value: &str) -> bool {
    !value.is_empty()
        && !value.chars().any(|character| {
            character.is_whitespace()
                || character.is_control()
                || matches!(character, '[' | ']' | '<' | '>' | '#')
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
            let edits = formatting(&analysis, &FormatConfig::default(), encoding).expect("edits");
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
            .expect("target"),
            Some(ReferenceKey::Local {
                anchor: "target".to_owned(),
            })
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
