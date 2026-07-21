//! Delimited-block registry and body boundary recognition.

use crate::block_model::{BlockProblem, BlockProblemKind, DelimitedBlockKind};
use crate::source::{PositionError, TextRange, TextSize};
use crate::source_document::SourceDocument;

#[derive(Clone, Copy)]
pub(super) struct DelimiterSpec {
    pub(super) kind: DelimitedBlockKind,
    pub(super) model: DelimitedContentModel,
}

#[derive(Clone, Copy)]
pub(super) enum DelimitedContentModel {
    Compound,
    Verbatim,
    Raw,
    Table,
}

pub(super) struct DelimitedBody {
    pub(super) range_end: TextSize,
    pub(super) content_range: TextRange,
    pub(super) closing_delimiter_range: Option<TextRange>,
    pub(super) next_line: usize,
    pub(super) content_end_line: usize,
    pub(super) problems: Vec<BlockProblem>,
}

pub(super) fn spec(delimiter: &str) -> Option<DelimiterSpec> {
    let (kind, model) = match delimiter {
        "--" => (DelimitedBlockKind::Open, DelimitedContentModel::Compound),
        "|===" => (DelimitedBlockKind::Table, DelimitedContentModel::Table),
        value if crate::table::is_table_delimiter(value) => {
            (DelimitedBlockKind::Table, DelimitedContentModel::Table)
        }
        _ if repeated(delimiter, '/') => {
            (DelimitedBlockKind::Comment, DelimitedContentModel::Verbatim)
        }
        _ if repeated(delimiter, '=') => {
            (DelimitedBlockKind::Example, DelimitedContentModel::Compound)
        }
        _ if repeated(delimiter, '-') => {
            (DelimitedBlockKind::Listing, DelimitedContentModel::Verbatim)
        }
        _ if repeated(delimiter, '.') => {
            (DelimitedBlockKind::Literal, DelimitedContentModel::Verbatim)
        }
        _ if repeated(delimiter, '*') => {
            (DelimitedBlockKind::Sidebar, DelimitedContentModel::Compound)
        }
        _ if repeated(delimiter, '+') => (DelimitedBlockKind::Pass, DelimitedContentModel::Raw),
        _ if repeated(delimiter, '_') => {
            (DelimitedBlockKind::Quote, DelimitedContentModel::Compound)
        }
        _ => return None,
    };
    Some(DelimiterSpec { kind, model })
}

pub(super) fn body(
    source_document: &SourceDocument,
    opener_index: usize,
    delimiter: &str,
    source: &str,
    end_line: usize,
) -> Result<DelimitedBody, PositionError> {
    let opener = source_document.lines()[opener_index];
    let content_start = opener.full_range().end();
    let mut closer_index = None;
    let mut recovery_index = None;
    for (index, line) in source_document
        .lines()
        .iter()
        .enumerate()
        .skip(opener_index + 1)
        .take(end_line.saturating_sub(opener_index + 1))
    {
        let content = source_document
            .text(line.content_range())
            .expect("line content has valid UTF-8 boundaries");
        if content == delimiter {
            closer_index = Some(index);
            break;
        }
        if recovery_index.is_none() && content.starts_with('=') {
            recovery_index = Some(index);
        }
    }
    let (range_end, content_end, closing_delimiter_range, next_line, problems) =
        if let Some(index) = closer_index {
            let closer = source_document.lines()[index];
            (
                closer.full_range().end(),
                closer.full_range().start(),
                Some(closer.content_range()),
                index + 1,
                Vec::new(),
            )
        } else {
            let end = recovery_index
                .map(|index| source_document.lines()[index].full_range().start())
                .unwrap_or_else(|| TextSize::new(source.len()).expect("validated source size"));
            (
                end,
                end,
                None,
                recovery_index.unwrap_or(end_line),
                vec![BlockProblem {
                    kind: BlockProblemKind::UnclosedBlock,
                    range: opener.content_range(),
                }],
            )
        };
    Ok(DelimitedBody {
        range_end,
        content_range: TextRange::new(content_start, content_end)?,
        closing_delimiter_range,
        next_line,
        content_end_line: closer_index.or(recovery_index).unwrap_or(end_line),
        problems,
    })
}

fn repeated(value: &str, marker: char) -> bool {
    value.len() >= 4 && value.chars().all(|character| character == marker)
}
