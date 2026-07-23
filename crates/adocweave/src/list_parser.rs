//! List marker grammar and flat-to-tree construction.

use crate::block_model::{BlockMetadata, ListBlock, ListItem, ListKind};
use crate::budget::ParseBudget;
use crate::parser_support::ParseFailure;
use crate::source::TextRange;

#[derive(Debug)]
pub(super) struct FlatListItem {
    pub(super) depth: usize,
    pub(super) kind: ListKind,
    pub(super) item: ListItem,
}

#[derive(Clone, Copy, Debug)]
pub(super) struct ParsedListMarker {
    pub(super) kind: ListKind,
    pub(super) depth: usize,
    pub(super) marker_start: usize,
    pub(super) marker_end: usize,
    pub(super) text_start: usize,
    pub(super) term_end: Option<usize>,
    pub(super) callout_id: Option<u32>,
}

pub(super) fn marker(content: &str) -> Option<ParsedListMarker> {
    let marker = content.as_bytes().first().copied()?;
    if matches!(marker, b'*' | b'.') {
        let kind = if marker == b'*' {
            ListKind::Unordered
        } else {
            ListKind::Ordered
        };
        let depth = content.bytes().take_while(|byte| *byte == marker).count();
        let separator = *content.as_bytes().get(depth)?;
        return matches!(separator, b' ' | b'\t').then_some(ParsedListMarker {
            kind,
            depth,
            marker_start: 0,
            marker_end: depth,
            text_start: depth + 1,
            term_end: None,
            callout_id: None,
        });
    }
    if marker == b'<' {
        let close = content.find('>')?;
        let raw = &content[1..close];
        let id = (raw == ".").then_some(0).or_else(|| raw.parse().ok())?;
        let separator = *content.as_bytes().get(close + 1)?;
        return matches!(separator, b' ' | b'\t').then_some(ParsedListMarker {
            kind: ListKind::Callout,
            depth: 1,
            marker_start: 0,
            marker_end: close + 1,
            text_start: close + 2,
            term_end: None,
            callout_id: Some(id),
        });
    }
    for (offset, character) in content.char_indices() {
        if !matches!(character, ':' | ';')
            || !content[offset..].starts_with(if character == ':' { "::" } else { ";;" })
        {
            continue;
        }
        let delimiter = &content[offset..];
        let width = if character == ':' && delimiter.starts_with("::::") {
            4
        } else if character == ':' && delimiter.starts_with(":::") {
            3
        } else {
            2
        };
        let after = offset + width;
        if offset == 0 || !matches!(content.as_bytes().get(after), None | Some(b' ' | b'\t')) {
            continue;
        }
        return Some(ParsedListMarker {
            kind: ListKind::Description,
            depth: width.saturating_sub(1),
            marker_start: offset,
            marker_end: after,
            text_start: after
                + usize::from(matches!(content.as_bytes().get(after), Some(b' ' | b'\t'))),
            term_end: Some(offset),
            callout_id: None,
        });
    }
    None
}

pub(super) fn build_tree(
    flat: &mut [FlatListItem],
    cursor: &mut usize,
    depth: usize,
    kind: ListKind,
    budget: &mut ParseBudget,
) -> Result<ListBlock, ParseFailure> {
    let mut items = Vec::new();
    while *cursor < flat.len() && flat[*cursor].depth == depth && flat[*cursor].kind == kind {
        let mut item = flat[*cursor].item.clone();
        *cursor += 1;
        while *cursor < flat.len() && flat[*cursor].depth > depth {
            let child_depth = flat[*cursor].depth;
            let child_kind = flat[*cursor].kind;
            item.children
                .push(build_tree(flat, cursor, child_depth, child_kind, budget)?);
        }
        if let Some(child) = item.children.last() {
            item.range = TextRange::new(item.range.start(), child.range.end())?;
        }
        items.push(item);
    }
    let range = TextRange::new(
        items.first().expect("list has item").range.start(),
        items.last().expect("list has item").range.end(),
    )?;
    budget.consume_node()?;
    Ok(ListBlock {
        metadata: BlockMetadata::default(),
        kind,
        presentation: crate::block_model::OrderedListPresentation::default(),
        range,
        items,
    })
}
