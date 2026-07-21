//! Recursive block-sequence input, context, output, and cursor invariants.

use std::ops::Range;

use crate::attributes::{AttributeProblem, DocumentAttribute};
use crate::parser::{AstBlock, DocumentHeader, ExplicitAnchor, ParseFailure};
use crate::source_document::SourceDocument;
use crate::syntax::SyntaxNode;

pub(super) struct BlockFacts {
    pub(super) syntax: Vec<SyntaxNode>,
    pub(super) blocks: Vec<AstBlock>,
    pub(super) anchors: Vec<ExplicitAnchor>,
}

pub(super) struct RootBlockSequenceOutput {
    pub(super) common: BlockFacts,
    pub(super) attributes: Vec<DocumentAttribute>,
    pub(super) attribute_problems: Vec<AttributeProblem>,
    pub(super) header: DocumentHeader,
}

pub(super) enum BlockSequenceOutput {
    Root(RootBlockSequenceOutput),
    Nested(BlockFacts),
}

#[derive(Clone)]
pub(super) struct BlockInput<'source> {
    pub(super) document: &'source SourceDocument,
    pub(super) lines: Range<usize>,
}

impl<'source> BlockInput<'source> {
    pub(super) fn new(
        document: &'source SourceDocument,
        lines: Range<usize>,
    ) -> Result<Self, ParseFailure> {
        if lines.start > lines.end || lines.end > document.lines().len() {
            return Err(ParseFailure::InternalInvariant);
        }
        Ok(Self { document, lines })
    }
}

#[derive(Clone, Copy)]
pub(super) struct ParseDepth {
    pub(super) block: usize,
    pub(super) table: usize,
}

#[derive(Clone, Copy)]
pub(super) enum BlockLocation {
    DocumentRoot,
    Compound,
    AsciiDocCell,
}

#[derive(Clone, Copy)]
pub(super) struct BlockContext {
    pub(super) depth: ParseDepth,
    location: BlockLocation,
}

impl BlockContext {
    pub(super) const fn root() -> Self {
        Self {
            location: BlockLocation::DocumentRoot,
            depth: ParseDepth { block: 1, table: 1 },
        }
    }

    pub(super) const fn nested(location: BlockLocation, depth: ParseDepth) -> Self {
        Self { location, depth }
    }

    pub(super) const fn allows_document_header(self) -> bool {
        matches!(self.location, BlockLocation::DocumentRoot)
    }

    pub(super) const fn document_title_position(self, saw_content: bool) -> bool {
        self.allows_document_header() && !saw_content
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct BlockCursor {
    line: usize,
    line_count: usize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum BlockRecognition {
    OneLine,
    Through(usize),
}

impl BlockCursor {
    pub(super) const fn for_range(lines: &Range<usize>) -> Self {
        Self {
            line: lines.start,
            line_count: lines.end,
        }
    }

    #[cfg(test)]
    pub(super) const fn new(line_count: usize) -> Self {
        Self {
            line: 0,
            line_count,
        }
    }

    pub(super) const fn current(self) -> Option<usize> {
        if self.line < self.line_count {
            Some(self.line)
        } else {
            None
        }
    }

    pub(super) fn commit(&mut self, recognition: BlockRecognition) -> Result<(), ParseFailure> {
        let next = match recognition {
            BlockRecognition::OneLine => self.line.saturating_add(1),
            BlockRecognition::Through(next) => next,
        };
        if next <= self.line || next > self.line_count {
            return Err(ParseFailure::InternalInvariant);
        }
        self.line = next;
        Ok(())
    }
}
