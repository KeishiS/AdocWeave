//! Pure Semantic Tokens projection over an adopted analysis snapshot.

use adocweave::Analysis;
use adocweave::output::projection::project;
use adocweave::semantic::{Inline, ReferenceTargetKind};
use adocweave::text::{SourceDocument, TextRange};
use async_lsp::lsp_types as lsp;

use crate::cancellation::{QueryCancellation, QueryResult};
use crate::position::{PositionEncoding, range_to_lsp};

pub(crate) fn tokens(
    analysis: &Analysis,
    encoding: PositionEncoding,
    cancellation: &QueryCancellation,
) -> QueryResult<lsp::SemanticTokens> {
    cancellation.check_now()?;
    let source_document = analysis.source_document();
    let mut raw = Vec::<(lsp::Position, u32, u32)>::new();
    for link in project(analysis, &adocweave::resolution::RenderInputs::default()).external_links {
        cancellation.checkpoint()?;
        push_range(
            &mut raw,
            link.target_range,
            0,
            source_document,
            encoding,
            cancellation,
        )?;
    }
    for reference in analysis.references() {
        cancellation.checkpoint()?;
        push_range(
            &mut raw,
            reference.target_range,
            0,
            source_document,
            encoding,
            cancellation,
        )?;
    }
    for anchor in analysis
        .document()
        .anchors()
        .iter()
        .filter(|anchor| anchor.valid)
    {
        cancellation.checkpoint()?;
        push_range(
            &mut raw,
            anchor.id_range,
            1,
            source_document,
            encoding,
            cancellation,
        )?;
    }
    let mut inline_ranges = Vec::new();
    adocweave::semantic::walk(analysis.document(), |node| {
        let adocweave::semantic::SemanticNode::Inline(inline) = node else {
            return;
        };
        match inline {
            Inline::Literal { content_range, .. }
            | Inline::Passthrough { content_range, .. }
            | Inline::Formula(adocweave::semantic::InlineFormula { content_range, .. }) => {
                inline_ranges.push((*content_range, 0))
            }
            Inline::Text(_)
            | Inline::Styled { .. }
            | Inline::AttributeReference { .. }
            | Inline::Link(_)
            | Inline::HardBreak { .. }
            | Inline::Macro(_)
            | Inline::Reference(_) => {}
        }
    });
    for target in analysis
        .reference_targets()
        .iter()
        .filter(|target| target.kind == ReferenceTargetKind::InlineAnchor)
    {
        cancellation.checkpoint()?;
        push_range(
            &mut raw,
            target.id_range,
            1,
            source_document,
            encoding,
            cancellation,
        )?;
    }
    for (range, token_type) in inline_ranges {
        cancellation.checkpoint()?;
        push_range(
            &mut raw,
            range,
            token_type,
            source_document,
            encoding,
            cancellation,
        )?;
    }
    cancellation.check_now()?;
    raw.sort_by_key(|(position, length, token_type)| {
        (position.line, position.character, *length, *token_type)
    });
    raw.dedup();
    cancellation.check_now()?;

    let mut previous = lsp::Position::new(0, 0);
    let data = raw
        .into_iter()
        .map(|(position, length, token_type)| {
            let delta_line = position.line - previous.line;
            let delta_start = if delta_line == 0 {
                position.character - previous.character
            } else {
                position.character
            };
            previous = position;
            lsp::SemanticToken {
                delta_line,
                delta_start,
                length,
                token_type,
                token_modifiers_bitset: 0,
            }
        })
        .collect();
    Ok(lsp::SemanticTokens {
        result_id: None,
        data,
    })
}

pub(crate) fn response(
    analysis: Option<&Analysis>,
    encoding: PositionEncoding,
    cancellation: &QueryCancellation,
) -> QueryResult<lsp::SemanticTokensResult> {
    let tokens = match analysis {
        Some(analysis) => tokens(analysis, encoding, cancellation)?,
        None => lsp::SemanticTokens {
            result_id: None,
            data: Vec::new(),
        },
    };
    Ok(lsp::SemanticTokensResult::Tokens(tokens))
}

fn push_range(
    output: &mut Vec<(lsp::Position, u32, u32)>,
    range: TextRange,
    token_type: u32,
    source_document: &SourceDocument,
    encoding: PositionEncoding,
    cancellation: &QueryCancellation,
) -> QueryResult<()> {
    let range = range_to_lsp(range, source_document, encoding)?;
    for line in range.start.line..=range.end.line {
        cancellation.checkpoint()?;
        let start = if line == range.start.line {
            range.start.character
        } else {
            0
        };
        let end = if line == range.end.line {
            range.end.character
        } else {
            source_document
                .line_length(line, encoding.core())
                .map_err(|error| error.to_string())?
        };
        if end > start {
            output.push((lsp::Position::new(line, start), end - start, token_type));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use adocweave::{Analysis, AnalysisOptions, AnalysisRequest, NeverCancel};
    use async_lsp::lsp_types as lsp;

    use super::{response, tokens};
    use crate::PositionEncoding;

    fn analyze(source: &str) -> Analysis {
        AnalysisRequest::new(None, 1, 1, source, AnalysisOptions::default())
            .analyze(&NeverCancel)
            .expect("analysis")
            .analysis
    }

    fn encoded(source: &str, encoding: PositionEncoding) -> Vec<u32> {
        let cancellation = crate::cancellation::test_cancellation();
        tokens(&analyze(source), encoding, &cancellation)
            .expect("semantic tokens")
            .data
            .into_iter()
            .flat_map(|token| {
                [
                    token.delta_line,
                    token.delta_start,
                    token.length,
                    token.token_type,
                    token.token_modifiers_bitset,
                ]
            })
            .collect()
    }

    #[test]
    fn tokens_are_sorted_deduplicated_and_delta_encoded() {
        let data = encoded(
            "[#target]\n== Target\n\nSee <<target>> and https://example.com.\n",
            PositionEncoding::Utf16,
        );

        assert!(!data.is_empty());
        assert_eq!(data.len() % 5, 0);
        for token in data.chunks_exact(5).skip(1) {
            assert!(token[0] > 0 || token[1] > 0);
        }
    }

    #[test]
    fn multiline_ranges_split_at_crlf_and_respect_unicode_encoding() {
        assert_eq!(
            encoded("``a😀\r\nb``", PositionEncoding::Utf8),
            [0, 2, 5, 0, 0, 1, 0, 1, 0, 0]
        );
        assert_eq!(
            encoded("``a😀\r\nb``", PositionEncoding::Utf16),
            [0, 2, 3, 0, 0, 1, 0, 1, 0, 0]
        );
    }

    #[test]
    fn syntactic_headings_are_left_to_editor_grammars() {
        assert!(encoded("= Document\n\n== Section\n", PositionEncoding::Utf8).is_empty());
    }

    #[test]
    fn missing_snapshot_has_an_empty_semantic_token_response() {
        let lsp::SemanticTokensResult::Tokens(tokens) = response(
            None,
            PositionEncoding::Utf16,
            &crate::cancellation::test_cancellation(),
        )
        .expect("response") else {
            panic!("full semantic tokens");
        };
        assert!(tokens.data.is_empty());
    }
}
