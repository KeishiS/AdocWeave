//! Lossless concrete syntax and HTML-independent semantic syntax.

use std::fmt::Write as _;
use std::sync::Arc;

use crate::attributes::{DocumentAttribute, parse_line as parse_attribute_line};
pub use crate::block_model::*;
use crate::block_sequence::{
    BlockContext, BlockCursor, BlockFacts, BlockInput, BlockLocation, BlockRecognition,
    BlockSequenceOutput, ParseDepth, RootBlockSequenceOutput,
};
use crate::budget::{BudgetExceeded, ParseBudget};
use crate::delimiter::{DelimitedContentModel, DelimiterSpec};
use crate::document_header::DocumentHeaderState;
use crate::inline::{Inline, InlineParseConfig, MathLanguage, parse_with_budget as parse_inlines};
use crate::limits::ProcessingLimits;
use crate::list_parser::{FlatListItem, ParsedListMarker};
use crate::source::{PositionError, TextRange, TextSize};
use crate::source_document::{SourceDocument, SourceDocumentBuildError, SourceLine};
use crate::syntax::{SyntaxKind, SyntaxNode, SyntaxTree};

#[derive(Default)]
struct PendingBlockMetadata {
    semantic: BlockMetadata,
    syntax: Vec<SyntaxNode>,
}

impl PendingBlockMetadata {
    fn is_empty(&self) -> bool {
        self.semantic.range.is_none()
    }

    fn push_title(&mut self, value: MetadataValue, line_range: TextRange) {
        self.extend_range(line_range);
        self.semantic.title = Some(value);
        self.syntax
            .push(SyntaxNode::leaf(SyntaxKind::BlockTitle, line_range));
    }

    fn push_attributes(&mut self, metadata: BlockMetadata, line_range: TextRange) {
        self.extend_range(line_range);
        if metadata.id.is_some() {
            self.semantic.id = metadata.id;
        }
        self.semantic.roles.extend(metadata.roles);
        self.semantic.options.extend(metadata.options);
        self.semantic.attributes.extend(metadata.attributes);
        self.syntax
            .push(SyntaxNode::leaf(SyntaxKind::BlockAttribute, line_range));
    }

    fn push_anchor(&mut self, anchor: &ExplicitAnchor) {
        self.extend_range(anchor.range);
        self.semantic.id = Some(MetadataValue {
            value: anchor.id.clone(),
            range: anchor.id_range,
        });
        self.syntax
            .push(SyntaxNode::leaf(SyntaxKind::BlockAnchor, anchor.range));
    }

    fn extend_range(&mut self, line_range: TextRange) {
        self.semantic.range = Some(match self.semantic.range {
            Some(range) => {
                TextRange::new(range.start(), line_range.end()).expect("ordered metadata")
            }
            None => line_range,
        });
    }
}

impl AstDocument {
    pub(crate) fn new(
        blocks: Vec<AstBlock>,
        attributes: Vec<DocumentAttribute>,
        anchors: Vec<ExplicitAnchor>,
        header: DocumentHeader,
    ) -> Self {
        Self {
            blocks,
            attributes,
            anchors,
            header,
            catalogs: crate::catalog::DocumentCatalogs::default(),
            identifiers: crate::document::DocumentIdentifiers::default(),
            structure: crate::structure::DocumentStructure::default(),
        }
    }

    pub fn blocks(&self) -> &[AstBlock] {
        &self.blocks
    }

    pub fn attributes(&self) -> &[DocumentAttribute] {
        &self.attributes
    }

    pub fn anchors(&self) -> &[ExplicitAnchor] {
        &self.anchors
    }

    pub const fn header(&self) -> &DocumentHeader {
        &self.header
    }

    pub const fn catalogs(&self) -> &crate::catalog::DocumentCatalogs {
        &self.catalogs
    }

    pub const fn identifiers(&self) -> &crate::document::DocumentIdentifiers {
        &self.identifiers
    }

    pub const fn structure(&self) -> &crate::structure::DocumentStructure {
        &self.structure
    }

    pub fn preamble(&self) -> &[AstBlock] {
        let end = self
            .blocks
            .iter()
            .position(|block| {
                matches!(
                    block,
                    AstBlock::Heading(Heading {
                        kind: HeadingKind::Section { .. } | HeadingKind::Part,
                        ..
                    })
                )
            })
            .unwrap_or(self.blocks.len());
        let start = self
            .blocks
            .iter()
            .position(|block| {
                !matches!(
                    block,
                    AstBlock::Heading(Heading {
                        kind: HeadingKind::DocumentTitle,
                        ..
                    })
                )
            })
            .unwrap_or(end);
        &self.blocks[start.min(end)..end]
    }

    pub(crate) fn normalize_heading_kinds(&mut self) {
        let doctype = self.header.doctype;
        self.visit_blocks_mut(|block| {
            let AstBlock::Heading(heading) = block else {
                return;
            };
            let discrete = heading
                .metadata
                .roles
                .iter()
                .any(|value| value.value == "discrete")
                || heading.metadata.attributes.iter().any(|attribute| {
                    attribute.name.is_none()
                        && matches!(attribute.value.as_str(), "discrete" | "float")
                });
            if discrete {
                let level = match heading.kind {
                    HeadingKind::DocumentTitle | HeadingKind::Part => 1,
                    HeadingKind::Section { level } | HeadingKind::Discrete { level } => level,
                };
                heading.kind = HeadingKind::Discrete { level };
                heading
                    .problems
                    .retain(|problem| *problem != HeadingProblem::MisplacedDocumentTitle);
                heading.well_formed = heading.problems.is_empty();
                heading.hierarchy_valid = heading.well_formed;
            } else if doctype == DocumentType::Book
                && heading.kind == HeadingKind::DocumentTitle
                && heading
                    .problems
                    .contains(&HeadingProblem::MisplacedDocumentTitle)
            {
                heading.kind = HeadingKind::Part;
                heading
                    .problems
                    .retain(|problem| *problem != HeadingProblem::MisplacedDocumentTitle);
                heading.well_formed = heading.problems.is_empty();
                heading.hierarchy_valid = heading.well_formed;
            }
        });
    }

    pub(crate) fn visit_blocks_mut(&mut self, mut visitor: impl FnMut(&mut AstBlock)) {
        crate::walker::walk_blocks_mut(&mut self.blocks, &mut visitor);
    }

    pub fn node_count(&self) -> usize {
        let mut count = 1;
        crate::walker::walk(self, |_| count += 1);
        count
    }

    pub fn snapshot(&self) -> String {
        let mut output = String::from("Document\n");
        for block in &self.blocks {
            match block {
                AstBlock::Heading(heading) => {
                    writeln!(
                        output,
                        "  {:?}@{}..{} marker={}..{} text={}..{} {:?} problems={:?}",
                        heading.kind,
                        heading.range.start().to_u32(),
                        heading.range.end().to_u32(),
                        heading.marker_range.start().to_u32(),
                        heading.marker_range.end().to_u32(),
                        heading.text_range.start().to_u32(),
                        heading.text_range.end().to_u32(),
                        heading.text,
                        heading.problems
                    )
                    .expect("writing to a String cannot fail");
                }
                AstBlock::Paragraph(paragraph) => {
                    writeln!(
                        output,
                        "  Paragraph@{}..{}",
                        paragraph.range.start().to_u32(),
                        paragraph.range.end().to_u32()
                    )
                    .expect("writing to a String cannot fail");
                    writeln!(
                        output,
                        "    Text@{}..{} {:?}",
                        paragraph.content_range.start().to_u32(),
                        paragraph.content_range.end().to_u32(),
                        paragraph.value
                    )
                    .expect("writing to a String cannot fail");
                }
                AstBlock::LiteralParagraph(paragraph) => {
                    writeln!(
                        output,
                        "  LiteralParagraph@{}..{} {:?}",
                        paragraph.range.start().to_u32(),
                        paragraph.range.end().to_u32(),
                        paragraph.value
                    )
                    .expect("writing to a String cannot fail");
                }
                AstBlock::Break(block) => {
                    writeln!(
                        output,
                        "  {:?}Break@{}..{}",
                        block.kind,
                        block.range.start().to_u32(),
                        block.range.end().to_u32()
                    )
                    .expect("writing to a String cannot fail");
                }
                AstBlock::Source(source) => {
                    writeln!(
                        output,
                        "  Source@{}..{} language={:?} content={}..{} problems={:?}",
                        source.range.start().to_u32(),
                        source.range.end().to_u32(),
                        source.language,
                        source.content_range.start().to_u32(),
                        source.content_range.end().to_u32(),
                        source.problems
                    )
                    .expect("writing to a String cannot fail");
                }
                AstBlock::List(list) => {
                    writeln!(
                        output,
                        "  {:?}List@{}..{} items={}",
                        list.kind,
                        list.range.start().to_u32(),
                        list.range.end().to_u32(),
                        list.items.len()
                    )
                    .expect("writing to a String cannot fail");
                }
                AstBlock::Math(math) => {
                    writeln!(
                        output,
                        "  Math({:?})@{}..{} content={}..{} {:?} problems={:?}",
                        math.language,
                        math.range.start().to_u32(),
                        math.range.end().to_u32(),
                        math.content_range.start().to_u32(),
                        math.content_range.end().to_u32(),
                        math.value,
                        math.problems
                    )
                    .expect("writing to a String cannot fail");
                }
                AstBlock::Delimited(block) => {
                    writeln!(
                        output,
                        "  {:?}@{}..{} delimiter={:?} content={}..{} {:?} problems={:?}",
                        block.kind,
                        block.range.start().to_u32(),
                        block.range.end().to_u32(),
                        block.delimiter,
                        block.content_range.start().to_u32(),
                        block.content_range.end().to_u32(),
                        block.content,
                        block.problems
                    )
                    .expect("writing to a String cannot fail");
                }
                AstBlock::Unsupported(unsupported) => {
                    writeln!(
                        output,
                        "  Unsupported@{}..{} {:?} ({})",
                        unsupported.range.start().to_u32(),
                        unsupported.range.end().to_u32(),
                        unsupported.raw,
                        unsupported.reason
                    )
                    .expect("writing to a String cannot fail");
                }
            }
        }
        output
    }

    pub(crate) fn visit_inline_sequences_mut(&mut self, mut visitor: impl FnMut(&mut Vec<Inline>)) {
        crate::walker::walk_inline_sequences_mut(&mut self.blocks, &mut visitor);
    }
}

impl AstBlock {
    pub const fn metadata(&self) -> &BlockMetadata {
        match self {
            Self::Heading(value) => &value.metadata,
            Self::Paragraph(value) => &value.metadata,
            Self::LiteralParagraph(value) => &value.metadata,
            Self::Break(value) => &value.metadata,
            Self::Source(value) => &value.metadata,
            Self::List(value) => &value.metadata,
            Self::Math(value) => &value.metadata,
            Self::Delimited(value) => &value.metadata,
            Self::Unsupported(value) => &value.metadata,
        }
    }

    pub(crate) fn metadata_mut(&mut self) -> &mut BlockMetadata {
        match self {
            Self::Heading(value) => &mut value.metadata,
            Self::Paragraph(value) => &mut value.metadata,
            Self::LiteralParagraph(value) => &mut value.metadata,
            Self::Break(value) => &mut value.metadata,
            Self::Source(value) => &mut value.metadata,
            Self::List(value) => &mut value.metadata,
            Self::Math(value) => &mut value.metadata,
            Self::Delimited(value) => &mut value.metadata,
            Self::Unsupported(value) => &mut value.metadata,
        }
    }

    pub const fn range(&self) -> TextRange {
        match self {
            Self::Heading(value) => value.range,
            Self::Paragraph(value) => value.range,
            Self::LiteralParagraph(value) => value.range,
            Self::Break(value) => value.range,
            Self::Source(value) => value.range,
            Self::List(value) => value.range,
            Self::Math(value) => value.range,
            Self::Delimited(value) => value.range,
            Self::Unsupported(value) => value.range,
        }
    }
}

#[derive(Debug)]
pub(crate) struct ParsedDocument {
    pub syntax: SyntaxTree,
    pub ast: AstDocument,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct ParseConfig {
    pub max_inline_depth: usize,
    pub max_list_depth: usize,
    pub max_block_depth: usize,
    pub max_formula_bytes: usize,
    pub limits: ProcessingLimits,
}

impl Default for ParseConfig {
    fn default() -> Self {
        let limits = ProcessingLimits {
            max_line_bytes: u32::MAX,
            max_blocks: u32::MAX,
            max_nodes: u32::MAX,
            max_references: u32::MAX,
            max_attributes: u32::MAX,
            ..ProcessingLimits::default()
        };
        Self {
            max_inline_depth: 32,
            max_list_depth: 8,
            max_block_depth: 32,
            max_formula_bytes: 1024 * 1024,
            limits,
        }
    }
}

#[cfg(test)]
pub(crate) fn parse(source: &str) -> Result<ParsedDocument, PositionError> {
    parse_with_config(source, &ParseConfig::default())
}

#[cfg(test)]
pub(crate) fn parse_with_config(
    source: &str,
    config: &ParseConfig,
) -> Result<ParsedDocument, PositionError> {
    parse_shared(Arc::from(source), config)
}

#[cfg(test)]
pub(crate) fn parse_shared(
    source: Arc<str>,
    config: &ParseConfig,
) -> Result<ParsedDocument, PositionError> {
    match parse_shared_cancellable(source, config, &|| false) {
        Ok(document) => Ok(document),
        Err(ParseFailure::Position(error)) => Err(error),
        Err(
            ParseFailure::Cancelled | ParseFailure::Budget(_) | ParseFailure::InternalInvariant,
        ) => {
            unreachable!("default test parser cannot be cancelled or exhaust its budget")
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ParseFailure {
    Position(PositionError),
    Budget(BudgetExceeded),
    Cancelled,
    InternalInvariant,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum LineRecognition {
    Source,
    InvalidSource,
    Math,
    Delimited,
    Anchor,
    BlockTitle,
    BlockMetadata,
    Blank,
    DocumentAttribute,
    Break,
    LiteralParagraph,
    Heading,
    List,
    Unsupported,
    Paragraph,
}

fn recognize_line(
    content: &str,
    next_content: Option<&str>,
    content_start: usize,
    full_range: TextRange,
    header_attributes_open: bool,
) -> LineRecognition {
    if parse_source_attribute(content).is_some() && next_content == Some("----") {
        LineRecognition::Source
    } else if content.starts_with("[source") && next_content == Some("----") {
        LineRecognition::InvalidSource
    } else if parse_math_attribute(content).is_some() && next_content == Some("++++") {
        LineRecognition::Math
    } else if crate::delimiter::spec(content).is_some() {
        LineRecognition::Delimited
    } else if parse_explicit_anchor(content, content_start, full_range)
        .filter(|_| content.starts_with("[["))
        .is_some()
    {
        LineRecognition::Anchor
    } else if parse_block_title(content, content_start).is_some() {
        LineRecognition::BlockTitle
    } else if parse_block_attributes(content, content_start).is_some() {
        LineRecognition::BlockMetadata
    } else if content.trim_matches([' ', '\t']).is_empty() {
        LineRecognition::Blank
    } else if header_attributes_open
        && parse_attribute_line(content, content_start, full_range).is_some()
    {
        LineRecognition::DocumentAttribute
    } else if matches!(content, "'''" | "<<<") {
        LineRecognition::Break
    } else if content.starts_with([' ', '\t']) {
        LineRecognition::LiteralParagraph
    } else if content.starts_with('=') {
        LineRecognition::Heading
    } else if crate::list_parser::marker(content).is_some() {
        LineRecognition::List
    } else if unsupported_reason(content).is_some() {
        LineRecognition::Unsupported
    } else {
        LineRecognition::Paragraph
    }
}

fn commit_block(
    syntax_blocks: &mut Vec<SyntaxNode>,
    ast_blocks: &mut Vec<AstBlock>,
    pending_metadata: &mut PendingBlockMetadata,
    syntax: SyntaxNode,
    block: AstBlock,
) {
    syntax_blocks.push(syntax);
    ast_blocks.push(block);
    attach_pending_metadata(syntax_blocks, ast_blocks, pending_metadata);
}

impl From<PositionError> for ParseFailure {
    fn from(error: PositionError) -> Self {
        Self::Position(error)
    }
}

impl From<BudgetExceeded> for ParseFailure {
    fn from(error: BudgetExceeded) -> Self {
        Self::Budget(error)
    }
}

pub(crate) fn parse_shared_cancellable(
    source: Arc<str>,
    config: &ParseConfig,
    is_cancelled: &dyn Fn() -> bool,
) -> Result<ParsedDocument, ParseFailure> {
    let mut budget = ParseBudget::new(config.limits)?;
    let source_document = SourceDocument::from_shared_bounded(
        Arc::clone(&source),
        config.limits.max_line_bytes,
        is_cancelled,
    )
    .map_err(|error| match error {
        SourceDocumentBuildError::Position(error) => ParseFailure::Position(error),
        SourceDocumentBuildError::LineLimitExceeded { limit, actual } => {
            ParseFailure::Budget(BudgetExceeded {
                resource: "line bytes",
                limit,
                actual,
            })
        }
        SourceDocumentBuildError::Cancelled => ParseFailure::Cancelled,
    })?;
    let line_count = source_document.lines().len();
    let sequence = parse_block_sequence(
        source.as_ref(),
        BlockInput::new(&source_document, 0..line_count)?,
        config,
        is_cancelled,
        &mut budget,
        BlockContext::root(),
    )?;
    finish_document(sequence, source_document, config)
}

fn parse_block_sequence(
    source: &str,
    input: BlockInput<'_>,
    config: &ParseConfig,
    is_cancelled: &dyn Fn() -> bool,
    budget: &mut ParseBudget,
    context: BlockContext,
) -> Result<BlockSequenceOutput, ParseFailure> {
    let source_document = input.document;
    let mut blocks = Vec::new();
    let mut ast_blocks = Vec::new();
    let mut paragraph_lines = Vec::new();
    let mut saw_content = false;
    let mut root = context
        .allows_document_header()
        .then(DocumentHeaderState::default);
    let mut anchors = Vec::new();
    let mut pending_metadata = PendingBlockMetadata::default();

    let mut cursor = BlockCursor::for_range(&input.lines);
    while let Some(line_index) = cursor.current() {
        if is_cancelled() {
            return Err(ParseFailure::Cancelled);
        }
        let line = source_document.lines()[line_index];
        let content = source_document
            .text(line.content_range())
            .expect("line content has valid UTF-8 boundaries");
        let next_content = source_document
            .lines()
            .get(line_index + 1)
            .and_then(|next| source_document.text(next.content_range()));

        if root.as_ref().is_some_and(|state| state.expect_author)
            && !content.trim_matches([' ', '\t']).is_empty()
        {
            root.as_mut().expect("root state exists").expect_author = false;
            if !content.chars().any(char::is_control)
                && !content.starts_with([':', '[', '='])
                && crate::delimiter::spec(content).is_none()
                && !content.starts_with("//")
            {
                if let Some(author) = crate::document_header::parse_author(content, line)? {
                    budget.consume_node()?;
                    let root = root.as_mut().expect("root state exists");
                    root.extend_range(line.full_range());
                    root.header.authors.push(author);
                    blocks.push(SyntaxNode::leaf(SyntaxKind::AuthorLine, line.full_range()));
                    root.expect_revision = true;
                    cursor.commit(BlockRecognition::OneLine)?;
                    continue;
                }
            }
        }
        if root.as_ref().is_some_and(|state| state.expect_revision)
            && !content.trim_matches([' ', '\t']).is_empty()
        {
            root.as_mut().expect("root state exists").expect_revision = false;
            if !content.chars().any(char::is_control)
                && !content.starts_with([':', '[', '='])
                && crate::delimiter::spec(content).is_none()
                && !content.starts_with("//")
            {
                let revision = crate::document_header::parse_revision(content, line)?;
                budget.consume_node()?;
                let root = root.as_mut().expect("root state exists");
                root.extend_range(line.full_range());
                root.header.revision = Some(revision);
                blocks.push(SyntaxNode::leaf(
                    SyntaxKind::RevisionLine,
                    line.full_range(),
                ));
                cursor.commit(BlockRecognition::OneLine)?;
                continue;
            }
        }

        let recognition = recognize_line(
            content,
            next_content,
            line.content_range().start().to_usize(),
            line.full_range(),
            root.as_ref().is_some_and(|state| state.attributes_open),
        );
        if recognition == LineRecognition::Source {
            flush_paragraph(
                &mut blocks,
                &mut ast_blocks,
                &mut paragraph_lines,
                config,
                budget,
                &mut pending_metadata,
            )?;
            budget.consume_block()?;
            budget.consume_node()?;
            let (mut source_block, next_line) =
                parse_source_block(source_document, line_index, source)?;
            source_block.metadata =
                parse_block_attributes(content, line.content_range().start().to_usize())
                    .unwrap_or_default();
            consume_metadata_budget(&source_block.metadata, budget)?;
            source_block.metadata.range = Some(line.full_range());
            let syntax = crate::syntax_builder::source(&source_block);
            commit_block(
                &mut blocks,
                &mut ast_blocks,
                &mut pending_metadata,
                syntax,
                AstBlock::Source(source_block),
            );
            saw_content = true;
            cursor.commit(BlockRecognition::Through(next_line))?;
            continue;
        } else if recognition == LineRecognition::InvalidSource {
            flush_paragraph(
                &mut blocks,
                &mut ast_blocks,
                &mut paragraph_lines,
                config,
                budget,
                &mut pending_metadata,
            )?;
            budget.consume_block()?;
            budget.consume_node()?;
            blocks.push(SyntaxNode::new(
                SyntaxKind::Unsupported,
                line.full_range(),
                vec![SyntaxNode::leaf(SyntaxKind::Unknown, line.full_range())],
            ));
            ast_blocks.push(AstBlock::Unsupported(Unsupported {
                metadata: BlockMetadata::default(),
                range: line.full_range(),
                raw: content.to_owned(),
                reason: "invalid source block attribute".to_owned(),
            }));
            attach_pending_metadata(&mut blocks, &mut ast_blocks, &mut pending_metadata);
            saw_content = true;
            root.iter_mut()
                .for_each(DocumentHeaderState::close_attributes);
        } else if recognition == LineRecognition::Math {
            flush_paragraph(
                &mut blocks,
                &mut ast_blocks,
                &mut paragraph_lines,
                config,
                budget,
                &mut pending_metadata,
            )?;
            budget.consume_block()?;
            budget.consume_node()?;
            let (mut math, next_line) =
                parse_math_block(source_document, line_index, source, config)?;
            math.metadata =
                parse_block_attributes(content, line.content_range().start().to_usize())
                    .unwrap_or_default();
            consume_metadata_budget(&math.metadata, budget)?;
            math.metadata.range = Some(line.full_range());
            let syntax = crate::syntax_builder::math(&math);
            commit_block(
                &mut blocks,
                &mut ast_blocks,
                &mut pending_metadata,
                syntax,
                AstBlock::Math(math),
            );
            saw_content = true;
            cursor.commit(BlockRecognition::Through(next_line))?;
            continue;
        } else if recognition == LineRecognition::Delimited {
            let spec = crate::delimiter::spec(content).expect("recognizer verified delimiter");
            flush_paragraph(
                &mut blocks,
                &mut ast_blocks,
                &mut paragraph_lines,
                config,
                budget,
                &mut pending_metadata,
            )?;
            budget.consume_block()?;
            budget.consume_node()?;
            let delimited_context = DelimitedParseContext {
                source_document,
                source,
                config,
                is_cancelled,
            };
            let mut state = ParseState {
                budget: &mut *budget,
                anchors: &mut anchors,
            };
            let (block, nested_syntax, next_line) = parse_delimited_block(
                &delimited_context,
                line_index,
                source_document.lines().len(),
                spec,
                &mut state,
                context.depth,
                Some(&pending_metadata.semantic),
            )?;
            let syntax = crate::syntax_builder::delimited(&block, nested_syntax);
            commit_block(
                &mut blocks,
                &mut ast_blocks,
                &mut pending_metadata,
                syntax,
                AstBlock::Delimited(block),
            );
            saw_content = true;
            root.iter_mut()
                .for_each(DocumentHeaderState::close_attributes);
            cursor.commit(BlockRecognition::Through(next_line))?;
            continue;
        } else if recognition == LineRecognition::Anchor {
            let anchor = parse_explicit_anchor(
                content,
                line.content_range().start().to_usize(),
                line.full_range(),
            )
            .filter(|_| content.starts_with("[["))
            .expect("recognizer verified anchor");
            flush_paragraph(
                &mut blocks,
                &mut ast_blocks,
                &mut paragraph_lines,
                config,
                budget,
                &mut pending_metadata,
            )?;
            budget.consume_node()?;
            pending_metadata.push_anchor(&anchor);
            anchors.push(anchor);
            saw_content = true;
            root.iter_mut()
                .for_each(DocumentHeaderState::close_attributes);
        } else if recognition == LineRecognition::BlockTitle {
            let title = parse_block_title(content, line.content_range().start().to_usize())
                .expect("recognizer verified block title");
            flush_paragraph(
                &mut blocks,
                &mut ast_blocks,
                &mut paragraph_lines,
                config,
                budget,
                &mut pending_metadata,
            )?;
            budget.consume_node()?;
            budget.consume_attribute()?;
            pending_metadata.push_title(title, line.full_range());
            root.iter_mut()
                .for_each(DocumentHeaderState::close_attributes);
        } else if recognition == LineRecognition::BlockMetadata {
            let metadata = parse_block_attributes(content, line.content_range().start().to_usize())
                .expect("recognizer verified block metadata");
            flush_paragraph(
                &mut blocks,
                &mut ast_blocks,
                &mut paragraph_lines,
                config,
                budget,
                &mut pending_metadata,
            )?;
            budget.consume_node()?;
            consume_metadata_budget(&metadata, budget)?;
            if let Some(id) = &metadata.id {
                anchors.push(ExplicitAnchor {
                    range: line.full_range(),
                    id_range: id.range,
                    label_range: None,
                    id: id.value.clone(),
                    label: None,
                    target_range: None,
                    valid: valid_anchor_id(&id.value),
                });
            }
            pending_metadata.push_attributes(metadata, line.full_range());
            root.iter_mut()
                .for_each(DocumentHeaderState::close_attributes);
        } else if recognition == LineRecognition::Blank {
            root.iter_mut()
                .for_each(DocumentHeaderState::stop_author_revision);
            flush_paragraph(
                &mut blocks,
                &mut ast_blocks,
                &mut paragraph_lines,
                config,
                budget,
                &mut pending_metadata,
            )?;
            flush_orphan_metadata(
                &mut blocks,
                &mut ast_blocks,
                &mut pending_metadata,
                source,
                budget,
            )?;
            blocks.push(SyntaxNode::leaf(SyntaxKind::BlankLine, line.full_range()));
            if root.as_ref().is_some_and(|state| state.attributes_open) {
                let root = root.as_mut().expect("root state exists");
                root.attributes_open = false;
                root.header.end = line.full_range().start();
            }
        } else if recognition == LineRecognition::DocumentAttribute {
            let (attribute, problem) = parse_attribute_line(
                content,
                line.content_range().start().to_usize(),
                line.full_range(),
            )
            .expect("recognizer verified document attribute");
            flush_paragraph(
                &mut blocks,
                &mut ast_blocks,
                &mut paragraph_lines,
                config,
                budget,
                &mut pending_metadata,
            )?;
            budget.consume_attribute()?;
            budget.consume_node()?;
            blocks.push(SyntaxNode::leaf(
                SyntaxKind::DocumentAttribute,
                line.full_range(),
            ));
            let root = root
                .as_mut()
                .expect("attribute recognition requires root state");
            root.attributes.push(attribute);
            root.attribute_problems.extend(problem);
            root.extend_range(line.full_range());
        } else if recognition == LineRecognition::Break {
            flush_paragraph(
                &mut blocks,
                &mut ast_blocks,
                &mut paragraph_lines,
                config,
                budget,
                &mut pending_metadata,
            )?;
            budget.consume_block()?;
            budget.consume_node()?;
            let kind = if content == "'''" {
                BreakKind::Thematic
            } else {
                BreakKind::Page
            };
            let syntax_kind = if kind == BreakKind::Thematic {
                SyntaxKind::ThematicBreak
            } else {
                SyntaxKind::PageBreak
            };
            blocks.push(SyntaxNode::leaf(syntax_kind, line.full_range()));
            ast_blocks.push(AstBlock::Break(BreakBlock {
                metadata: BlockMetadata::default(),
                range: line.full_range(),
                kind,
            }));
            attach_pending_metadata(&mut blocks, &mut ast_blocks, &mut pending_metadata);
            saw_content = true;
            root.iter_mut()
                .for_each(DocumentHeaderState::close_attributes);
        } else if recognition == LineRecognition::LiteralParagraph {
            flush_paragraph(
                &mut blocks,
                &mut ast_blocks,
                &mut paragraph_lines,
                config,
                budget,
                &mut pending_metadata,
            )?;
            budget.consume_block()?;
            budget.consume_node()?;
            let (literal, next_line) = parse_literal_paragraph(source_document, line_index)?;
            blocks.push(SyntaxNode::leaf(SyntaxKind::LiteralBlock, literal.range));
            ast_blocks.push(AstBlock::LiteralParagraph(literal));
            attach_pending_metadata(&mut blocks, &mut ast_blocks, &mut pending_metadata);
            saw_content = true;
            root.iter_mut()
                .for_each(DocumentHeaderState::close_attributes);
            cursor.commit(BlockRecognition::Through(next_line))?;
            continue;
        } else if recognition == LineRecognition::Heading {
            flush_paragraph(
                &mut blocks,
                &mut ast_blocks,
                &mut paragraph_lines,
                config,
                budget,
                &mut pending_metadata,
            )?;
            budget.consume_block()?;
            budget.consume_node()?;
            let heading = parse_heading(
                content,
                line,
                context.document_title_position(saw_content),
                config,
                budget,
            )?;
            let syntax_kind = if heading.problems.is_empty() {
                match heading.kind {
                    HeadingKind::DocumentTitle => SyntaxKind::DocumentTitle,
                    HeadingKind::Part
                    | HeadingKind::Section { .. }
                    | HeadingKind::Discrete { .. } => SyntaxKind::Heading,
                }
            } else {
                SyntaxKind::MalformedHeading
            };
            blocks.push(crate::syntax_builder::heading(&heading, syntax_kind));
            ast_blocks.push(AstBlock::Heading(heading));
            attach_pending_metadata(&mut blocks, &mut ast_blocks, &mut pending_metadata);
            let opens_header = context.allows_document_header()
                && matches!(
                    ast_blocks.last(),
                    Some(AstBlock::Heading(Heading {
                        kind: HeadingKind::DocumentTitle,
                        well_formed: true,
                        hierarchy_valid: true,
                        ..
                    }))
                );
            if opens_header {
                let root = root.as_mut().expect("document title requires root state");
                root.attributes_open = true;
                root.extend_range(line.full_range());
                root.expect_author = true;
            }
            saw_content = true;
        } else if recognition == LineRecognition::List {
            flush_paragraph(
                &mut blocks,
                &mut ast_blocks,
                &mut paragraph_lines,
                config,
                budget,
                &mut pending_metadata,
            )?;
            let list_context = DelimitedParseContext {
                source_document,
                source,
                config,
                is_cancelled,
            };
            let mut state = ParseState {
                budget: &mut *budget,
                anchors: &mut anchors,
            };
            let (lists, next_line, range) =
                parse_lists(&list_context, line_index, &mut state, context.depth)?;
            blocks.push(crate::syntax_builder::list(range, &lists));
            ast_blocks.extend(lists.into_iter().map(AstBlock::List));
            attach_pending_metadata(&mut blocks, &mut ast_blocks, &mut pending_metadata);
            saw_content = true;
            root.iter_mut()
                .for_each(DocumentHeaderState::close_attributes);
            cursor.commit(BlockRecognition::Through(next_line))?;
            continue;
        } else if recognition == LineRecognition::Unsupported {
            let reason = unsupported_reason(content).expect("recognizer verified unsupported line");
            flush_paragraph(
                &mut blocks,
                &mut ast_blocks,
                &mut paragraph_lines,
                config,
                budget,
                &mut pending_metadata,
            )?;
            budget.consume_block()?;
            budget.consume_node()?;
            blocks.push(SyntaxNode::new(
                SyntaxKind::Unsupported,
                line.full_range(),
                vec![SyntaxNode::leaf(SyntaxKind::Unknown, line.full_range())],
            ));
            ast_blocks.push(AstBlock::Unsupported(Unsupported {
                metadata: BlockMetadata::default(),
                range: line.full_range(),
                raw: content.to_owned(),
                reason: reason.to_owned(),
            }));
            attach_pending_metadata(&mut blocks, &mut ast_blocks, &mut pending_metadata);
            saw_content = true;
        } else {
            paragraph_lines.push((line, content.to_owned()));
            saw_content = true;
        }
        cursor.commit(BlockRecognition::OneLine)?;
    }
    flush_paragraph(
        &mut blocks,
        &mut ast_blocks,
        &mut paragraph_lines,
        config,
        budget,
        &mut pending_metadata,
    )?;
    flush_orphan_metadata(
        &mut blocks,
        &mut ast_blocks,
        &mut pending_metadata,
        source,
        budget,
    )?;
    let common = BlockFacts {
        syntax: blocks,
        blocks: ast_blocks,
        anchors,
    };
    Ok(match root {
        Some(root) => BlockSequenceOutput::Root(RootBlockSequenceOutput {
            common,
            attributes: root.attributes,
            attribute_problems: root.attribute_problems,
            header: root.header,
        }),
        None => BlockSequenceOutput::Nested(common),
    })
}

fn finish_document(
    sequence: BlockSequenceOutput,
    source_document: SourceDocument,
    config: &ParseConfig,
) -> Result<ParsedDocument, ParseFailure> {
    let BlockSequenceOutput::Root(sequence) = sequence else {
        return Err(ParseFailure::InternalInvariant);
    };
    let mut ast = crate::lowering::lower(crate::lowering::ParsedFacts {
        blocks: sequence.common.blocks,
        attributes: sequence.attributes,
        anchors: sequence.common.anchors,
        header: sequence.header,
        attribute_expansion_limits: crate::substitution::AttributeExpansionLimits {
            max_depth: config.limits.max_attribute_expansion_depth,
            max_bytes: config.limits.max_attribute_expansion_bytes,
        },
    });
    ast.catalogs = crate::catalog::build(&ast, config.limits).map_err(|error| {
        ParseFailure::Budget(BudgetExceeded {
            resource: error.resource,
            limit: error.limit,
            actual: error.actual,
        })
    })?;
    let syntax_issues =
        crate::syntax_diagnostics::collect_and_clear(&mut ast.blocks, &sequence.attribute_problems);

    Ok(ParsedDocument {
        syntax: SyntaxTree::from_blocks(source_document, sequence.common.syntax, syntax_issues),
        ast,
    })
}

fn parse_explicit_anchor(
    content: &str,
    absolute_start: usize,
    full_range: TextRange,
) -> Option<ExplicitAnchor> {
    let (inner, prefix_len) = if let Some(inner) = content
        .strip_prefix("[[")
        .and_then(|value| value.strip_suffix("]]"))
    {
        (inner, 2)
    } else if let Some(inner) = content
        .strip_prefix("[#")
        .and_then(|value| value.strip_suffix(']'))
    {
        (inner, 2)
    } else {
        return None;
    };
    let (id, label) = inner
        .split_once(',')
        .map_or((inner, None), |(id, label)| (id, Some(label)));
    let id_range = text_range(
        absolute_start + prefix_len,
        absolute_start + prefix_len + id.len(),
    )
    .expect("anchor range fits");
    let label_range = label.map(|label| {
        text_range(
            absolute_start + prefix_len + id.len() + 1,
            absolute_start + prefix_len + id.len() + 1 + label.len(),
        )
        .expect("anchor label range fits")
    });
    Some(ExplicitAnchor {
        range: full_range,
        id_range,
        label_range,
        id: id.to_owned(),
        label: label.map(str::to_owned),
        target_range: None,
        valid: valid_anchor_id(id),
    })
}

fn parse_block_title(content: &str, base: usize) -> Option<MetadataValue> {
    let value = content.strip_prefix('.')?;
    if value.is_empty() || value.starts_with([' ', '\t', '.']) {
        return None;
    }
    let start = TextSize::new(base + 1).ok()?;
    let end = TextSize::new(base + content.len()).ok()?;
    Some(MetadataValue {
        value: value.to_owned(),
        range: TextRange::new(start, end).ok()?,
    })
}

fn parse_block_attributes(content: &str, base: usize) -> Option<BlockMetadata> {
    let inner = content.strip_prefix('[')?.strip_suffix(']')?;
    if inner.starts_with('[') || inner.ends_with(']') {
        return None;
    }
    let mut metadata = BlockMetadata::default();
    let mut field_start = 0;
    let mut quoted = false;
    for field_end in inner
        .char_indices()
        .filter_map(|(index, character)| {
            if character == '"' {
                quoted = !quoted;
            }
            (character == ',' && !quoted).then_some(index)
        })
        .chain(std::iter::once(inner.len()))
    {
        let raw = &inner[field_start..field_end];
        let leading = raw.len() - raw.trim_start().len();
        let value = raw.trim();
        let absolute_start = base + 1 + field_start + leading;
        let range = TextRange::new(
            TextSize::new(absolute_start).ok()?,
            TextSize::new(absolute_start + value.len()).ok()?,
        )
        .ok()?;
        if !value.is_empty() {
            parse_element_attribute(value, range, &mut metadata);
        }
        field_start = field_end.saturating_add(1);
    }
    Some(metadata)
}

fn parse_element_attribute(value: &str, range: TextRange, metadata: &mut BlockMetadata) {
    if let Some((name, raw_value)) = value.split_once('=') {
        let name = name.trim();
        let raw_value = raw_value.trim();
        metadata.attributes.push(ElementAttribute {
            name: (!name.is_empty()).then(|| name.to_owned()),
            value: unquote(raw_value).to_owned(),
            range,
        });
        return;
    }

    let mut shorthand = value;
    let mut consumed_shorthand = false;
    while let Some(marker) = shorthand
        .chars()
        .next()
        .filter(|value| matches!(value, '#' | '.' | '%'))
    {
        let tail = &shorthand[marker.len_utf8()..];
        let end = tail.find(['#', '.', '%']).unwrap_or(tail.len());
        let item = &tail[..end];
        if item.is_empty() {
            break;
        }
        let offset = value.len() - shorthand.len() + marker.len_utf8();
        let item_range = TextRange::new(
            TextSize::new(range.start().to_usize() + offset).expect("attribute offset is bounded"),
            TextSize::new(range.start().to_usize() + offset + item.len())
                .expect("attribute offset is bounded"),
        )
        .expect("ordered shorthand range");
        let item = MetadataValue {
            value: item.to_owned(),
            range: item_range,
        };
        match marker {
            '#' => metadata.id = Some(item),
            '.' => metadata.roles.push(item),
            '%' => metadata.options.push(item),
            _ => unreachable!(),
        }
        consumed_shorthand = true;
        shorthand = &tail[end..];
    }
    if !consumed_shorthand || !shorthand.is_empty() {
        metadata.attributes.push(ElementAttribute {
            name: None,
            value: unquote(value).to_owned(),
            range,
        });
    }
}

fn unquote(value: &str) -> &str {
    value
        .strip_prefix('"')
        .and_then(|value| value.strip_suffix('"'))
        .unwrap_or(value)
}

fn consume_metadata_budget(
    metadata: &BlockMetadata,
    budget: &mut ParseBudget,
) -> Result<(), BudgetExceeded> {
    let count = metadata.attributes.len()
        + metadata.roles.len()
        + metadata.options.len()
        + usize::from(metadata.id.is_some());
    for _ in 0..count {
        budget.consume_attribute()?;
    }
    Ok(())
}

fn attach_pending_metadata(
    syntax_blocks: &mut [SyntaxNode],
    ast_blocks: &mut [AstBlock],
    pending: &mut PendingBlockMetadata,
) {
    if pending.is_empty() {
        return;
    }
    let metadata = std::mem::take(pending);
    let Some(block) = ast_blocks.last_mut() else {
        return;
    };
    let existing = std::mem::take(block.metadata_mut());
    *block.metadata_mut() = merge_block_metadata(metadata.semantic, existing);
    let syntax = syntax_blocks
        .last_mut()
        .expect("semantic and syntax blocks are appended together");
    let start = block.metadata().range.expect("metadata range").start();
    syntax.prepend_annotations(start, metadata.syntax);
}

fn flush_orphan_metadata(
    syntax_blocks: &mut Vec<SyntaxNode>,
    ast_blocks: &mut Vec<AstBlock>,
    pending: &mut PendingBlockMetadata,
    source: &str,
    budget: &mut ParseBudget,
) -> Result<(), BudgetExceeded> {
    if pending.is_empty() {
        return Ok(());
    }
    budget.consume_block()?;
    budget.consume_node()?;
    let pending = std::mem::take(pending);
    let range = pending
        .semantic
        .range
        .expect("non-empty metadata has a range");
    let unknown = SyntaxNode::new(SyntaxKind::Unknown, range, pending.syntax);
    syntax_blocks.push(SyntaxNode::new(
        SyntaxKind::Unsupported,
        range,
        vec![unknown],
    ));
    ast_blocks.push(AstBlock::Unsupported(Unsupported {
        metadata: BlockMetadata::default(),
        range,
        raw: source[range.start().to_usize()..range.end().to_usize()]
            .trim_end_matches(['\r', '\n'])
            .to_owned(),
        reason: "block metadata is not attached to a block".to_owned(),
    }));
    Ok(())
}

fn merge_block_metadata(mut leading: BlockMetadata, trailing: BlockMetadata) -> BlockMetadata {
    leading.range = match (leading.range, trailing.range) {
        (Some(first), Some(last)) => {
            Some(TextRange::new(first.start(), last.end()).expect("metadata ranges are ordered"))
        }
        (range @ Some(_), None) | (None, range @ Some(_)) => range,
        (None, None) => None,
    };
    if trailing.title.is_some() {
        leading.title = trailing.title;
    }
    if trailing.id.is_some() {
        leading.id = trailing.id;
    }
    leading.roles.extend(trailing.roles);
    leading.options.extend(trailing.options);
    leading.attributes.extend(trailing.attributes);
    leading
}

fn valid_anchor_id(id: &str) -> bool {
    !id.is_empty()
        && id.chars().all(|character| {
            !character.is_control()
                && !character.is_whitespace()
                && !matches!(character, '[' | ']' | '<' | '>' | ',' | '#')
        })
}

fn parse_math_block(
    source_document: &SourceDocument,
    attribute_index: usize,
    source: &str,
    config: &ParseConfig,
) -> Result<(MathBlock, usize), PositionError> {
    let attribute = source_document.lines()[attribute_index];
    let attribute_text = source_document
        .text(attribute.content_range())
        .expect("valid");
    let language = parse_math_attribute(attribute_text).expect("recognized math attribute");
    let delimiter_index = attribute_index + 1;
    let delimiter = source_document.lines()[delimiter_index];
    let body = crate::delimiter::body(
        source_document,
        delimiter_index,
        "++++",
        source,
        source_document.lines().len(),
    )?;
    let value = source
        .get(body.content_range.start().to_usize()..body.content_range.end().to_usize())
        .expect("valid math content")
        .to_owned();
    let mut problems = Vec::new();
    if body
        .problems
        .iter()
        .any(|problem| problem.kind == BlockProblemKind::UnclosedBlock)
    {
        problems.push(MathProblem {
            kind: MathProblemKind::Unclosed,
            range: delimiter.content_range(),
        });
    }
    if value.is_empty() {
        problems.push(MathProblem {
            kind: MathProblemKind::Empty,
            range: body.content_range,
        });
    }
    if value.len() > config.max_formula_bytes {
        problems.push(MathProblem {
            kind: MathProblemKind::SizeLimitExceeded,
            range: body.content_range,
        });
    }
    Ok((
        MathBlock {
            metadata: BlockMetadata::default(),
            range: TextRange::new(attribute.full_range().start(), body.range_end)?,
            attribute_range: attribute.content_range(),
            delimiter_range: delimiter.content_range(),
            content_range: body.content_range,
            language,
            value,
            problems,
        },
        body.next_line,
    ))
}

fn parse_math_attribute(text: &str) -> Option<MathLanguage> {
    match text {
        "[stem]" | "[latexmath]" => Some(MathLanguage::Latex),
        _ => None,
    }
}

fn parse_lists(
    context: &DelimitedParseContext<'_>,
    start: usize,
    state: &mut ParseState<'_>,
    parse_depth: ParseDepth,
) -> Result<(Vec<ListBlock>, usize, TextRange), ParseFailure> {
    let source_document = context.source_document;
    let config = context.config;
    let mut flat = Vec::new();
    let mut index = start;
    let mut previous: Option<(usize, ListKind)> = None;
    let mut kinds_by_depth = Vec::<Option<ListKind>>::new();
    while index < source_document.lines().len() {
        let line = source_document.lines()[index];
        let content = source_document
            .text(line.content_range())
            .expect("valid line");
        let Some(marker) = crate::list_parser::marker(content) else {
            break;
        };
        let ParsedListMarker {
            kind,
            depth,
            marker_start,
            marker_end,
            mut text_start,
            term_end,
            mut callout_id,
        } = marker;
        let effective_depth = depth.min(config.max_list_depth.max(1));
        let absolute = line.content_range().start().to_usize();
        let marker_range = text_range(absolute + marker_start, absolute + marker_end)?;
        let separator_range = text_range(absolute + marker_end, absolute + text_start)?;
        let mut checklist = None;
        if kind == ListKind::Unordered {
            let rest = &content[text_start..];
            if rest.len() >= 4
                && rest.as_bytes()[0] == b'['
                && rest.as_bytes()[2] == b']'
                && matches!(rest.as_bytes()[3], b' ' | b'\t')
            {
                checklist = match rest.as_bytes()[1] {
                    b' ' => Some(ChecklistState::Unchecked),
                    b'x' | b'X' | b'*' => Some(ChecklistState::Checked),
                    _ => None,
                };
                if checklist.is_some() {
                    text_start += 4;
                }
            }
        }
        if kind == ListKind::Callout && callout_id == Some(0) {
            callout_id = Some(
                flat.iter()
                    .filter(|item: &&FlatListItem| item.kind == ListKind::Callout)
                    .count() as u32
                    + 1,
            );
        }
        let text = &content[text_start..];
        let item_text_range = text_range(absolute + text_start, absolute + content.len())?;
        let parsed = parse_inlines(
            text,
            item_text_range,
            InlineParseConfig {
                max_depth: config.max_inline_depth,
                max_formula_bytes: config.max_formula_bytes,
            },
            state.budget,
        )?;
        let mut problems = Vec::new();
        if text.is_empty() {
            problems.push(ListProblem {
                kind: ListProblemKind::EmptyItem,
                range: item_text_range,
            });
        }
        if content.as_bytes().get(marker_end) == Some(&b'\t') {
            problems.push(ListProblem {
                kind: ListProblemKind::NonCanonicalSeparator,
                range: separator_range,
            });
        }
        if depth > config.max_list_depth {
            problems.push(ListProblem {
                kind: ListProblemKind::DepthLimitExceeded,
                range: marker_range,
            });
        }
        if let Some((previous_depth, _)) = previous {
            if effective_depth > previous_depth + 1 {
                problems.push(ListProblem {
                    kind: ListProblemKind::InvalidNesting,
                    range: marker_range,
                });
            }
        }
        if kinds_by_depth
            .get(effective_depth)
            .and_then(|kind| *kind)
            .is_some_and(|established| established != kind)
        {
            problems.push(ListProblem {
                kind: ListProblemKind::InconsistentMarker,
                range: marker_range,
            });
        }
        kinds_by_depth.resize(kinds_by_depth.len().max(effective_depth + 1), None);
        kinds_by_depth[effective_depth] = Some(kind);
        state.budget.consume_node()?;
        let terms = if let Some(term_end) = term_end {
            let term_range = text_range(absolute, absolute + term_end)?;
            let term = &content[..term_end];
            let parsed_term = parse_inlines(
                term,
                term_range,
                InlineParseConfig {
                    max_depth: config.max_inline_depth,
                    max_formula_bytes: config.max_formula_bytes,
                },
                state.budget,
            )?;
            vec![DescriptionTerm {
                range: term_range,
                text: term.to_owned(),
                inlines: parsed_term.inlines,
                inline_problems: parsed_term.problems,
            }]
        } else {
            Vec::new()
        };
        let mut item = ListItem {
            range: line.full_range(),
            marker_range,
            separator_range,
            text_range: item_text_range,
            text: text.to_owned(),
            inlines: parsed.inlines,
            terms,
            checklist,
            callout_id,
            inline_problems: parsed.problems,
            children: Vec::new(),
            continuations: Vec::new(),
            continuation_ranges: Vec::new(),
            problems,
        };
        index += 1;
        while source_document
            .lines()
            .get(index)
            .is_some_and(|next| source_document.text(next.content_range()) == Some("+"))
        {
            let continuation = source_document.lines()[index];
            state.budget.consume_list_continuation()?;
            let next = index + 1;
            let Some((attached, end)) = parse_list_continuation(context, next, state, parse_depth)?
            else {
                break;
            };
            item.continuation_ranges.push(continuation.full_range());
            let attached_end = attached
                .last()
                .expect("a parsed continuation has a block")
                .range()
                .end();
            item.range = TextRange::new(item.range.start(), attached_end)?;
            item.continuations.extend(attached);
            index = end;
        }
        previous = Some((effective_depth, kind));
        flat.push(FlatListItem {
            depth: effective_depth,
            kind,
            item,
        });
    }
    let mut item_index = 0;
    while item_index + 1 < flat.len() {
        let combines_with_next = flat[item_index].kind == ListKind::Description
            && flat[item_index].item.text.is_empty()
            && flat[item_index + 1].kind == ListKind::Description
            && flat[item_index + 1].depth == flat[item_index].depth;
        if combines_with_next {
            let preceding = flat.remove(item_index);
            let following = &mut flat[item_index].item;
            let mut terms = preceding.item.terms;
            terms.append(&mut following.terms);
            following.terms = terms;
            following.range = TextRange::new(preceding.item.range.start(), following.range.end())?;
        } else {
            item_index += 1;
        }
    }
    let end = flat
        .last()
        .map_or(source_document.lines()[start].full_range().end(), |item| {
            item.item.range.end()
        });
    let range = TextRange::new(source_document.lines()[start].full_range().start(), end)?;
    let mut cursor = 0;
    let mut roots = Vec::new();
    while cursor < flat.len() {
        let depth = flat[cursor].depth;
        let kind = flat[cursor].kind;
        state.budget.consume_block()?;
        roots.push(crate::list_parser::build_tree(
            &mut flat,
            &mut cursor,
            depth,
            kind,
            state.budget,
        )?);
    }
    Ok((roots, index, range))
}

fn parse_list_continuation(
    context: &DelimitedParseContext<'_>,
    index: usize,
    state: &mut ParseState<'_>,
    depth: ParseDepth,
) -> Result<Option<(Vec<AstBlock>, usize)>, ParseFailure> {
    let source_document = context.source_document;
    let source = context.source;
    let config = context.config;
    let Some(line) = source_document.lines().get(index).copied() else {
        return Ok(None);
    };
    let content = source_document
        .text(line.content_range())
        .expect("valid continuation line");
    if content.trim_matches([' ', '\t']).is_empty() {
        return Ok(None);
    }
    state.budget.consume_block()?;
    state.budget.consume_node()?;
    if parse_source_attribute(content).is_some()
        && source_document
            .lines()
            .get(index + 1)
            .and_then(|line| source_document.text(line.content_range()))
            == Some("----")
    {
        let (mut block, end) = parse_source_block(source_document, index, source)?;
        block.metadata = parse_block_attributes(content, line.content_range().start().to_usize())
            .unwrap_or_default();
        return Ok(Some((vec![AstBlock::Source(block)], end)));
    }
    if let Some(spec) = crate::delimiter::spec(content) {
        let (block, nested_syntax, end) = parse_delimited_block(
            context,
            index,
            source_document.lines().len(),
            spec,
            state,
            ParseDepth {
                block: depth.block + 1,
                table: depth.table,
            },
            None,
        )?;
        let _ = nested_syntax;
        return Ok(Some((vec![AstBlock::Delimited(block)], end)));
    }
    if crate::list_parser::marker(content).is_some() {
        let (lists, end, _) = parse_lists(context, index, state, depth)?;
        return Ok(Some((lists.into_iter().map(AstBlock::List).collect(), end)));
    }
    let parsed = parse_inlines(
        content,
        line.content_range(),
        InlineParseConfig {
            max_depth: config.max_inline_depth,
            max_formula_bytes: config.max_formula_bytes,
        },
        state.budget,
    )?;
    Ok(Some((
        vec![AstBlock::Paragraph(Paragraph {
            metadata: BlockMetadata::default(),
            range: line.full_range(),
            content_range: line.content_range(),
            value: content.to_owned(),
            inlines: parsed.inlines,
            inline_problems: parsed.problems,
        })],
        index + 1,
    )))
}

fn scan_callout_markers(
    value: &str,
    range: TextRange,
) -> Result<Vec<CalloutMarker>, PositionError> {
    let mut output = Vec::new();
    let mut cursor = 0;
    while let Some(relative) = value[cursor..].find('<') {
        let open = cursor + relative;
        if open > 0 && value.as_bytes()[open - 1] == b'\\' {
            cursor = open + 1;
            continue;
        }
        let Some(close_relative) = value[open + 1..].find('>') else {
            break;
        };
        let close = open + 1 + close_relative;
        if let Ok(id) = value[open + 1..close].parse::<u32>() {
            if id != 0 {
                output.push(CalloutMarker {
                    id,
                    range: text_range(
                        range.start().to_usize() + open,
                        range.start().to_usize() + close + 1,
                    )?,
                });
            }
        }
        cursor = close + 1;
    }
    Ok(output)
}

struct DelimitedParseContext<'source> {
    source_document: &'source SourceDocument,
    source: &'source str,
    config: &'source ParseConfig,
    is_cancelled: &'source dyn Fn() -> bool,
}

struct ParseState<'state> {
    budget: &'state mut ParseBudget,
    anchors: &'state mut Vec<ExplicitAnchor>,
}

pub(crate) fn trailing_whitespace_is_structural(content: &str) -> bool {
    let trimmed = content.trim_end_matches([' ', '\t']);
    trimmed.len() != content.len()
        && (crate::delimiter::spec(trimmed).is_some()
            || parse_block_attributes(trimmed, 0).is_some()
            || parse_source_attribute(trimmed).is_some()
            || parse_math_attribute(trimmed).is_some()
            || parse_explicit_anchor(
                trimmed,
                0,
                text_range(0, trimmed.len()).expect("trimmed line range is bounded"),
            )
            .is_some())
}

fn parse_delimited_block(
    context: &DelimitedParseContext<'_>,
    opener_index: usize,
    end_line: usize,
    spec: DelimiterSpec,
    state: &mut ParseState<'_>,
    depth: ParseDepth,
    metadata: Option<&BlockMetadata>,
) -> Result<(DelimitedBlock, Vec<SyntaxNode>, usize), ParseFailure> {
    let source_document = context.source_document;
    let source = context.source;
    let opener = source_document.lines()[opener_index];
    let delimiter = source_document
        .text(opener.content_range())
        .expect("delimiter range is valid");
    let body = crate::delimiter::body(source_document, opener_index, delimiter, source, end_line)?;
    let value = source
        .get(body.content_range.start().to_usize()..body.content_range.end().to_usize())
        .expect("delimited content range is valid")
        .to_owned();
    let mut nested_syntax = Vec::new();
    let content = match spec.model {
        DelimitedContentModel::Compound => {
            let nested = parse_nested_blocks(
                source_document,
                opener_index + 1,
                body.content_end_line,
                context,
                state,
                ParseDepth {
                    block: depth.block + 1,
                    table: depth.table,
                },
                BlockLocation::Compound,
            )?;
            nested_syntax = nested.syntax;
            DelimitedContent::Compound(nested.blocks)
        }
        DelimitedContentModel::Verbatim => DelimitedContent::Verbatim(value),
        DelimitedContentModel::Raw => DelimitedContent::Passthrough(value),
        DelimitedContentModel::Table => {
            let (table, syntax) = parse_table(
                TableSyntaxInput {
                    value: &value,
                    content_range: body.content_range,
                    delimiter,
                    delimiter_range: opener.content_range(),
                    metadata: metadata.unwrap_or(&BlockMetadata::default()),
                },
                context,
                state,
                depth,
            )?;
            nested_syntax = syntax;
            DelimitedContent::Table(table)
        }
    };
    Ok((
        DelimitedBlock {
            metadata: BlockMetadata::default(),
            kind: spec.kind,
            range: TextRange::new(opener.full_range().start(), body.range_end)?,
            opening_delimiter_range: opener.content_range(),
            closing_delimiter_range: body.closing_delimiter_range,
            content_range: body.content_range,
            delimiter: delimiter.to_owned(),
            content,
            problems: body.problems,
        },
        nested_syntax,
        body.next_line,
    ))
}

struct TableSyntaxInput<'source> {
    value: &'source str,
    content_range: TextRange,
    delimiter: &'source str,
    delimiter_range: TextRange,
    metadata: &'source BlockMetadata,
}

fn parse_table(
    input: TableSyntaxInput<'_>,
    context: &DelimitedParseContext<'_>,
    state: &mut ParseState<'_>,
    depth: ParseDepth,
) -> Result<(crate::table::Table, Vec<SyntaxNode>), ParseFailure> {
    use crate::table::{
        HorizontalAlignment, TableCell, TableCellContent, TableCellStyle, TableColumn, TableRow,
        TableSection, VerticalAlignment,
    };

    let config = context.config;
    let reject = |resource, limit, actual| {
        ParseFailure::Budget(BudgetExceeded {
            resource,
            limit,
            actual,
        })
    };
    if input.value.len() as u64 > u64::from(config.limits.max_table_bytes) {
        return Err(reject(
            "table bytes",
            config.limits.max_table_bytes,
            input.value.len() as u64,
        ));
    }
    if depth.table as u64 > u64::from(config.limits.max_table_depth) {
        return Err(reject(
            "table nesting depth",
            config.limits.max_table_depth,
            depth.table as u64,
        ));
    }
    let (input_spec, mut table_problems) = crate::table::TableInputSpec::resolve(
        input.delimiter,
        input.delimiter_range,
        input.metadata,
    );
    let raw = crate::table::scan(input.value, input.content_range, input_spec);
    table_problems.extend(raw.problems.iter().copied());
    let cell_count = raw
        .cells
        .iter()
        .map(|cell| u64::from(cell.duplication))
        .sum::<u64>();
    if cell_count > u64::from(config.limits.max_table_cells) {
        return Err(reject(
            "table cells",
            config.limits.max_table_cells,
            cell_count,
        ));
    }
    let widest = raw.inferred_columns as u64;
    if widest > u64::from(config.limits.max_table_columns) {
        return Err(reject(
            "table columns",
            config.limits.max_table_columns,
            widest,
        ));
    }
    let column_count = raw
        .inferred_columns
        .min(config.limits.max_table_columns as usize);
    let columns = (0..column_count)
        .map(|index| TableColumn {
            index: index as u32,
            width: None,
            horizontal_alignment: HorizontalAlignment::Left,
            vertical_alignment: VerticalAlignment::Top,
            style: TableCellStyle::Default,
        })
        .collect();
    let mut cells = Vec::with_capacity(raw.cells.len());
    for cell in raw.cells {
        for _ in 0..cell.duplication {
            state.budget.consume_node()?;
            let content = match cell.style {
                TableCellStyle::Literal | TableCellStyle::Verse => {
                    TableCellContent::Verbatim(cell.raw.clone())
                }
                _ => {
                    let parsed = parse_inlines(
                        &cell.raw,
                        cell.content_range,
                        InlineParseConfig {
                            max_depth: config.max_inline_depth,
                            max_formula_bytes: config.max_formula_bytes,
                        },
                        state.budget,
                    )?;
                    TableCellContent::Inlines(parsed.inlines)
                }
            };
            cells.push(TableCell {
                range: cell.range,
                marker_range: cell.marker_range,
                content_range: cell.content_range,
                raw: cell.raw.clone(),
                column_index: 0,
                column_span: cell.column_span,
                row_span: cell.row_span,
                horizontal_alignment: cell.horizontal_alignment,
                vertical_alignment: cell.vertical_alignment,
                style: cell.style,
                style_is_explicit: cell.style_is_explicit,
                content,
            });
        }
    }
    let mut table = crate::table::Table {
        format: raw.format,
        separator: raw.separator,
        content_range: raw.content_range,
        columns,
        rows: if cells.is_empty() {
            Vec::new()
        } else {
            vec![TableRow {
                range: TextRange::new(
                    cells.first().expect("non-empty cells").range.start(),
                    cells.last().expect("non-empty cells").range.end(),
                )
                .expect("table cell range is ordered"),
                section: TableSection::Body,
                cells,
            }]
        },
        problems: table_problems,
    };
    crate::table::layout_rows(&mut table);
    crate::table::configure(&mut table, input.metadata);
    let mut nested_syntax = Vec::new();
    for row in &mut table.rows {
        for cell in &mut row.cells {
            if cell.style != TableCellStyle::AsciiDoc {
                continue;
            }
            let fragment =
                if context.source_document.text(cell.content_range) == Some(cell.raw.as_str()) {
                    SourceDocument::indexed_view(context.source_document, cell.content_range)?
                } else {
                    SourceDocument::from_fragment_bounded(
                        Arc::from(cell.raw.as_str()),
                        cell.content_range.start(),
                        config.limits.max_line_bytes,
                        context.is_cancelled,
                    )
                    .map_err(|error| match error {
                        SourceDocumentBuildError::Position(error) => ParseFailure::Position(error),
                        SourceDocumentBuildError::LineLimitExceeded { limit, actual } => {
                            ParseFailure::Budget(BudgetExceeded {
                                resource: "line bytes",
                                limit,
                                actual,
                            })
                        }
                        SourceDocumentBuildError::Cancelled => ParseFailure::Cancelled,
                    })?
                };
            let nested = parse_nested_blocks(
                &fragment,
                0,
                fragment.lines().len(),
                context,
                state,
                ParseDepth {
                    block: depth.block + 1,
                    table: depth.table + 1,
                },
                BlockLocation::AsciiDocCell,
            )?;
            nested_syntax.extend(nested.syntax);
            cell.content = TableCellContent::AsciiDoc(nested.blocks);
        }
    }
    Ok((table, nested_syntax))
}

struct NestedBlocks {
    blocks: Vec<AstBlock>,
    syntax: Vec<SyntaxNode>,
}

fn parse_nested_blocks(
    source_document: &SourceDocument,
    start_line: usize,
    end_line: usize,
    context: &DelimitedParseContext<'_>,
    state: &mut ParseState<'_>,
    depth: ParseDepth,
    location: BlockLocation,
) -> Result<NestedBlocks, ParseFailure> {
    let config = context.config;
    let is_cancelled = context.is_cancelled;
    if depth.block > config.max_block_depth.max(1) {
        return Err(ParseFailure::Budget(BudgetExceeded {
            resource: "block nesting depth",
            limit: u32::try_from(config.max_block_depth).unwrap_or(u32::MAX),
            actual: u64::try_from(depth.block).unwrap_or(u64::MAX),
        }));
    }
    if start_line == end_line {
        return Ok(NestedBlocks {
            blocks: Vec::new(),
            syntax: Vec::new(),
        });
    }
    let sequence = parse_block_sequence(
        context.source,
        BlockInput::new(source_document, start_line..end_line)?,
        config,
        is_cancelled,
        state.budget,
        BlockContext::nested(location, depth),
    )?;
    let BlockSequenceOutput::Nested(sequence) = sequence else {
        return Err(ParseFailure::InternalInvariant);
    };
    state.anchors.extend(sequence.anchors);
    Ok(NestedBlocks {
        blocks: sequence.blocks,
        syntax: sequence.syntax,
    })
}

fn parse_source_block(
    source_document: &SourceDocument,
    attribute_index: usize,
    source: &str,
) -> Result<(SourceBlock, usize), PositionError> {
    let attribute = source_document.lines()[attribute_index];
    let attribute_text = source_document
        .text(attribute.content_range())
        .expect("attribute range is valid");
    let language_relative =
        parse_source_attribute(attribute_text).expect("caller recognized source attribute");
    let language_range = language_relative
        .map(|(start, end)| {
            text_range(
                attribute.content_range().start().to_usize() + start,
                attribute.content_range().start().to_usize() + end,
            )
        })
        .transpose()?;
    let language = language_relative.map(|(start, end)| attribute_text[start..end].to_owned());
    let delimiter_index = attribute_index + 1;
    let delimiter = source_document.lines()[delimiter_index];
    let mut body = crate::delimiter::body(
        source_document,
        delimiter_index,
        "----",
        source,
        source_document.lines().len(),
    )?;
    if language.is_none() {
        body.problems.push(BlockProblem {
            kind: BlockProblemKind::MissingSourceLanguage,
            range: attribute.content_range(),
        });
    }
    let value = source
        .get(body.content_range.start().to_usize()..body.content_range.end().to_usize())
        .expect("source block content range is valid")
        .to_owned();
    let callouts = scan_callout_markers(&value, body.content_range)?;

    Ok((
        SourceBlock {
            metadata: BlockMetadata::default(),
            range: TextRange::new(attribute.full_range().start(), body.range_end)?,
            attribute_range: attribute.content_range(),
            language_range,
            language,
            delimiter_range: delimiter.content_range(),
            content_range: body.content_range,
            value,
            callouts,
            problems: body.problems,
        },
        body.next_line,
    ))
}

fn parse_source_attribute(text: &str) -> Option<Option<(usize, usize)>> {
    let inner = text.strip_prefix("[source")?.strip_suffix(']')?;
    if inner.is_empty() {
        return Some(None);
    }
    let language = inner.strip_prefix(',')?;
    let leading = language.len() - language.trim_start_matches([' ', '\t']).len();
    let trimmed = language.trim_matches([' ', '\t']);
    if trimmed.is_empty() {
        return Some(None);
    }
    if trimmed.contains([',', ']']) {
        return None;
    }
    let start = "[source,".len() + leading;
    Some(Some((start, start + trimmed.len())))
}

fn parse_heading(
    content: &str,
    line: SourceLine,
    document_title_position: bool,
    config: &ParseConfig,
    budget: &mut ParseBudget,
) -> Result<Heading, ParseFailure> {
    let marker_len = content.bytes().take_while(|byte| *byte == b'=').count();
    let content_start = line.content_range().start().to_usize();
    let marker_range = text_range(content_start, content_start + marker_len)?;
    let has_space = content.as_bytes().get(marker_len) == Some(&b' ');
    let text_start_relative = marker_len + usize::from(has_space);
    let text = content
        .get(text_start_relative..)
        .unwrap_or_default()
        .trim_end_matches([' ', '\t']);
    let text_start = content_start + text_start_relative.min(content.len());
    let separator_range = if has_space {
        text_range(content_start + marker_len, content_start + marker_len + 1)?
    } else {
        text_range(content_start + marker_len, content_start + marker_len)?
    };
    let text_range = text_range(text_start, text_start + text.len())?;
    let mut problems = Vec::new();
    if !has_space {
        problems.push(HeadingProblem::MissingSpace);
    }
    if text.is_empty() {
        problems.push(HeadingProblem::EmptyText);
    }
    let kind = match marker_len {
        1 if document_title_position => HeadingKind::DocumentTitle,
        1 => {
            problems.push(HeadingProblem::MisplacedDocumentTitle);
            HeadingKind::DocumentTitle
        }
        2..=6 => HeadingKind::Section {
            level: (marker_len - 1) as u8,
        },
        _ => {
            problems.push(HeadingProblem::LevelTooDeep);
            HeadingKind::Section { level: 6 }
        }
    };

    let inline_output = parse_inlines(
        text,
        text_range,
        InlineParseConfig {
            max_depth: config.max_inline_depth,
            max_formula_bytes: config.max_formula_bytes,
        },
        budget,
    )?;
    Ok(Heading {
        metadata: BlockMetadata::default(),
        range: line.full_range(),
        marker_range,
        separator_range,
        text_range,
        kind,
        well_formed: problems.is_empty(),
        hierarchy_valid: !problems.iter().any(|problem| {
            matches!(
                problem,
                HeadingProblem::LevelTooDeep | HeadingProblem::MisplacedDocumentTitle
            )
        }),
        text: text.to_owned(),
        inlines: inline_output.inlines,
        inline_problems: inline_output.problems,
        problems,
    })
}

fn parse_literal_paragraph(
    source: &SourceDocument,
    start_line: usize,
) -> Result<(LiteralParagraph, usize), PositionError> {
    let first = source.lines()[start_line];
    let mut end_line = start_line;
    let mut value = String::new();
    while let Some(line) = source.lines().get(end_line).copied() {
        let content = source
            .text(line.content_range())
            .expect("line content is valid UTF-8");
        if !content.starts_with([' ', '\t']) {
            break;
        }
        if end_line > start_line {
            value.push('\n');
        }
        value.push_str(&content[1..]);
        end_line += 1;
    }
    let last = source.lines()[end_line - 1];
    let content_start = first.content_range().start().to_usize() + 1;
    Ok((
        LiteralParagraph {
            metadata: BlockMetadata::default(),
            range: TextRange::new(first.full_range().start(), last.full_range().end())?,
            content_range: text_range(content_start, last.content_range().end().to_usize())?,
            value,
        },
        end_line,
    ))
}

fn flush_paragraph(
    cst_blocks: &mut Vec<SyntaxNode>,
    ast_blocks: &mut Vec<AstBlock>,
    lines: &mut Vec<(SourceLine, String)>,
    config: &ParseConfig,
    budget: &mut ParseBudget,
    pending_metadata: &mut PendingBlockMetadata,
) -> Result<(), ParseFailure> {
    let (Some((first, _)), Some((last, _))) = (lines.first(), lines.last()) else {
        return Ok(());
    };
    budget.consume_block()?;
    budget.consume_node()?;
    let range = TextRange::new(first.full_range().start(), last.full_range().end())
        .expect("ordered source lines form an ordered paragraph");
    let mut paragraph = Paragraph {
        metadata: BlockMetadata::default(),
        range,
        content_range: {
            TextRange::new(first.content_range().start(), last.content_range().end())
                .expect("paragraph content range is ordered")
        },
        value: String::new(),
        inlines: Vec::new(),
        inline_problems: Vec::new(),
    };
    for (line, value) in lines.drain(..) {
        paragraph.value.push_str(&value);
        if line.full_range().end() < paragraph.content_range.end() {
            paragraph.value.push_str(match line.ending() {
                crate::source_document::LineEnding::Lf => "\n",
                crate::source_document::LineEnding::CrLf => "\r\n",
                crate::source_document::LineEnding::None => "",
            });
        }
    }
    let inline_output = parse_inlines(
        &paragraph.value,
        paragraph.content_range,
        InlineParseConfig {
            max_depth: config.max_inline_depth,
            max_formula_bytes: config.max_formula_bytes,
        },
        budget,
    )?;
    paragraph.inlines = split_hard_breaks(inline_output.inlines);
    paragraph.inline_problems = inline_output.problems;
    cst_blocks.push(crate::syntax_builder::paragraph(&paragraph));
    ast_blocks.push(AstBlock::Paragraph(paragraph));
    attach_pending_metadata(cst_blocks, ast_blocks, pending_metadata);
    Ok(())
}

fn split_hard_breaks(inlines: Vec<Inline>) -> Vec<Inline> {
    let mut output = Vec::new();
    for inline in inlines {
        match inline {
            Inline::Text(text) => split_hard_break_text(text, &mut output),
            Inline::Styled {
                style,
                range,
                content_range,
                children,
            } => output.push(Inline::Styled {
                style,
                range,
                content_range,
                children: split_hard_breaks(children),
            }),
            Inline::Link(mut link) => {
                link.label = split_hard_breaks(link.label);
                output.push(Inline::Link(link));
            }
            Inline::Reference(mut reference) => {
                reference.label = split_hard_breaks(reference.label);
                output.push(Inline::Reference(reference));
            }
            other => output.push(other),
        }
    }
    output
}

fn split_hard_break_text(text: crate::inline::InlineText, output: &mut Vec<Inline>) {
    let bytes = text.value.as_bytes();
    let mut cursor = 0;
    for (newline, _) in text.value.match_indices('\n') {
        let marker_end = if newline > 0 && bytes[newline - 1] == b'\r' {
            newline - 1
        } else {
            newline
        };
        if marker_end < 2 || &bytes[marker_end - 2..marker_end] != b" +" {
            continue;
        }
        let marker_start = marker_end - 2;
        if cursor < marker_start {
            output.push(Inline::Text(crate::inline::InlineText {
                range: relative_range(text.range, cursor, marker_start),
                value: text.value[cursor..marker_start].to_owned(),
            }));
        }
        let newline_end = newline + 1;
        output.push(Inline::HardBreak {
            range: relative_range(text.range, marker_start, newline_end),
        });
        cursor = newline_end;
    }
    if cursor < text.value.len() {
        output.push(Inline::Text(crate::inline::InlineText {
            range: relative_range(text.range, cursor, text.value.len()),
            value: text.value[cursor..].to_owned(),
        }));
    }
}

fn relative_range(parent: TextRange, start: usize, end: usize) -> TextRange {
    TextRange::new(
        TextSize::new(parent.start().to_usize() + start).expect("inline offset is bounded"),
        TextSize::new(parent.start().to_usize() + end).expect("inline offset is bounded"),
    )
    .expect("relative inline range is ordered")
}

fn unsupported_reason(content: &str) -> Option<&'static str> {
    let trimmed = content.trim_start_matches([' ', '\t']);
    if trimmed.starts_with('[') {
        Some("block attributes are not implemented")
    } else if is_delimiter(trimmed) {
        Some("delimited blocks are not implemented")
    } else if trimmed.starts_with("* ") || trimmed.starts_with(". ") {
        Some("list syntax is not implemented")
    } else {
        None
    }
}

fn text_range(start: usize, end: usize) -> Result<TextRange, PositionError> {
    TextRange::new(
        crate::source::TextSize::new(start)?,
        crate::source::TextSize::new(end)?,
    )
}

fn is_delimiter(text: &str) -> bool {
    let mut characters = text.chars();
    let Some(first) = characters.next() else {
        return false;
    };
    matches!(first, '-' | '.' | '=' | '_')
        && text.chars().count() >= 4
        && characters.all(|character| character == first)
}

#[cfg(test)]
mod tests {
    use super::{
        AstBlock, BreakBlock, BreakKind, ChecklistState, DelimitedBlock, DelimitedBlockKind,
        DelimitedContent, DocumentType, Heading, HeadingKind, ListKind, SyntaxKind, parse,
    };

    #[test]
    fn block_cursor_rejects_non_progress_and_out_of_bounds_commits() {
        let mut cursor = super::BlockCursor::new(2);
        assert_eq!(cursor.current(), Some(0));
        assert!(cursor.commit(super::BlockRecognition::Through(0)).is_err());
        assert!(cursor.commit(super::BlockRecognition::Through(3)).is_err());
        cursor
            .commit(super::BlockRecognition::OneLine)
            .expect("first line");
        cursor
            .commit(super::BlockRecognition::Through(2))
            .expect("second line");
        assert_eq!(cursor.current(), None);
    }

    #[test]
    fn nested_compound_blocks_share_the_root_source_index() {
        crate::source_document::SourceDocument::reset_construction_count();
        let source = "====\n.Outer\n--\n.Sidebar\n****\nparagraph\n****\n--\n====\n";

        let parsed = parse(source).expect("nested compound blocks");

        assert_eq!(
            crate::source_document::SourceDocument::construction_count(),
            1,
            "compound recursion must not rebuild SourceDocument"
        );
        assert_eq!(parsed.syntax.reconstruct(), source);
        let AstBlock::Delimited(outer) = &parsed.ast.blocks()[0] else {
            panic!("outer example")
        };
        let DelimitedContent::Compound(outer_children) = &outer.content else {
            panic!("outer compound content")
        };
        assert_eq!(outer_children[0].range().start().to_usize(), 12);
        let AstBlock::Delimited(open) = &outer_children[0] else {
            panic!("open block")
        };
        let DelimitedContent::Compound(open_children) = &open.content else {
            panic!("open content")
        };
        assert_eq!(open_children[0].range().start().to_usize(), 24);
    }
    use crate::attributes::AttributeOperation;
    use crate::inline::{Inline, MathLanguage};

    #[test]
    fn paragraph_parser_handles_empty_input() {
        let parsed = parse("").expect("valid source");

        assert!(parsed.ast.blocks().is_empty());
        assert_eq!(parsed.syntax.blocks().len(), 1);
        assert_eq!(parsed.syntax.blocks()[0].kind(), SyntaxKind::BlankLine);
        assert_eq!(parsed.syntax.reconstruct(), "");
    }

    #[test]
    fn misplaced_document_title_metadata_keeps_control_character_lines_lossless() {
        let source = "= S]]\n= Seedeed= See2\n\u{c}\n\0[\u{6}\0\0\n[\0\0\0\0\n== S\n[[\u{6}\0\0\0\0\0\0\0\n== Sectioection\n\n*stRtn\n\n";
        let parsed = parse(source).expect("parse");
        assert_eq!(parsed.syntax.reconstruct(), source);
    }

    #[test]
    fn document_attributes_preserve_cst_and_produce_generic_ast() {
        let source = concat!(
            "= Note\n",
            ":note-id: 123E4567-E89B-12D3-A456-426614174000\n",
            ":created-at: 2026-07-20T12:34:56Z\n",
            ":tags: rust, AsciiDoc\n",
            ":stem: latexmath\n",
            ":draft!:\n",
            "body {note-id}\n",
        );
        let parsed = parse(source).expect("valid source");

        assert_eq!(parsed.syntax.reconstruct(), source);
        assert_eq!(parsed.ast.attributes.len(), 5);
        assert_eq!(parsed.ast.attributes[0].operation, AttributeOperation::Set);
        assert_eq!(
            parsed.ast.attributes[0].raw_value,
            "123E4567-E89B-12D3-A456-426614174000"
        );
        assert_eq!(parsed.ast.attributes[2].raw_value, "rust, AsciiDoc");
        assert_eq!(parsed.ast.attributes[3].raw_value, "latexmath");
        assert_eq!(
            parsed.ast.attributes[4].operation,
            AttributeOperation::Unset
        );
        assert!(parsed.syntax.issues().is_empty());
    }

    #[test]
    fn empty_generic_attribute_values_are_preserved_without_host_semantics() {
        let parsed = parse("= Note\n:note-id:\n:tags:\n\nbody\n").expect("recover");
        assert_eq!(parsed.ast.attributes.len(), 2);
        assert!(parsed.syntax.issues().is_empty());
        assert!(matches!(
            parsed.ast.blocks().last(),
            Some(AstBlock::Paragraph(_))
        ));
    }

    #[test]
    fn paragraph_parser_groups_lines_and_splits_on_blank_lines() {
        let source = "\nfirst line\nsecond line\n \t\nlast";
        let parsed = parse(source).expect("valid source");

        assert_eq!(parsed.ast.blocks().len(), 2);
        let AstBlock::Paragraph(first) = &parsed.ast.blocks()[0] else {
            panic!("expected paragraph");
        };
        assert_eq!(first.value, "first line\nsecond line");
        assert_eq!(parsed.syntax.reconstruct().as_bytes(), source.as_bytes());
    }

    #[test]
    fn paragraph_inlines_span_lf_crlf_unicode_and_macro_labels() {
        let source =
            "before *strong\n日本語* and ``mono\r\ncode`` https://example.org[label\n続き]";
        let parsed = parse(source).expect("valid source");
        let AstBlock::Paragraph(paragraph) = &parsed.ast.blocks()[0] else {
            panic!("paragraph");
        };

        assert_eq!(paragraph.content_range.start().to_usize(), 0);
        assert_eq!(paragraph.content_range.end().to_usize(), source.len());
        assert_eq!(paragraph.value, source);
        assert!(paragraph.inline_problems.is_empty());
        assert!(paragraph.inlines.iter().any(|inline| matches!(
            inline,
            Inline::Styled {
                style: crate::inline::InlineStyle::Strong,
                children,
                ..
            } if matches!(&children[..], [Inline::Text(text)] if text.value == "strong\n日本語")
        )));
        assert!(paragraph.inlines.iter().any(|inline| matches!(
            inline,
            Inline::Literal { value, .. } if value == "mono\r\ncode"
        )));
        assert!(paragraph.inlines.iter().any(|inline| matches!(
            inline,
            Inline::Link(link)
                if matches!(&link.label[..], [Inline::Text(text)] if text.value == "label\n続き")
        )));
    }

    #[test]
    fn paragraph_parser_keeps_unsupported_syntax_explicit() {
        let source = "before\n\n[role=test]\n\nafter";
        let parsed = parse(source).expect("valid source");

        assert_eq!(parsed.ast.blocks().len(), 3);
        let AstBlock::Unsupported(unsupported) = &parsed.ast.blocks()[1] else {
            panic!("expected unsupported node");
        };
        assert_eq!(unsupported.raw, "[role=test]");
        assert_eq!(
            unsupported.reason,
            "block metadata is not attached to a block"
        );
        assert_eq!(parsed.syntax.reconstruct(), source);
    }

    #[test]
    fn common_block_metadata_attaches_to_the_adjacent_block() {
        let source = ".Visible title\n[#item.role-a.role-b%collapsible,kind=\"demo\",positional]\nParagraph\n";
        let parsed = parse(source).expect("parse");
        assert_eq!(parsed.syntax.reconstruct(), source);
        assert_eq!(parsed.syntax.nodes(SyntaxKind::BlockTitle).count(), 1);
        assert_eq!(parsed.syntax.nodes(SyntaxKind::BlockAttribute).count(), 1);
        let block = &parsed.ast.blocks()[0];
        let metadata = block.metadata();
        assert_eq!(
            metadata.range.expect("metadata range").end().to_usize(),
            source.find("Paragraph").expect("paragraph")
        );
        assert_eq!(
            metadata.title.as_ref().map(|value| value.value.as_str()),
            Some("Visible title")
        );
        assert_eq!(
            metadata.id.as_ref().map(|value| value.value.as_str()),
            Some("item")
        );
        assert_eq!(
            metadata
                .roles
                .iter()
                .map(|value| value.value.as_str())
                .collect::<Vec<_>>(),
            ["role-a", "role-b"]
        );
        assert_eq!(
            metadata
                .options
                .iter()
                .map(|value| value.value.as_str())
                .collect::<Vec<_>>(),
            ["collapsible"]
        );
        assert_eq!(metadata.attributes.len(), 2);
        assert_eq!(metadata.attributes[0].name.as_deref(), Some("kind"));
        assert_eq!(metadata.attributes[0].value, "demo");
        assert_eq!(metadata.attributes[1].name, None);
        assert_eq!(metadata.attributes[1].value, "positional");
    }

    #[test]
    fn metadata_is_shared_by_heading_literal_list_source_and_math_blocks() {
        let parsed = parse(
            "[.heading]\n== H\n\n.Title\n....\nbody\n....\n\n[#list]\n* item\n\n[source,rust]\n----\nfn main() {}\n----\n\n[stem]\n++++\nx\n++++\n",
        )
        .expect("parse");
        let blocks = parsed.ast.blocks();
        assert_eq!(blocks[0].metadata().roles[0].value, "heading");
        assert_eq!(
            blocks[1].metadata().title.as_ref().expect("title").value,
            "Title"
        );
        assert_eq!(blocks[2].metadata().id.as_ref().expect("id").value, "list");
        assert_eq!(blocks[3].metadata().attributes[0].value, "source");
        assert_eq!(blocks[3].metadata().attributes[1].value, "rust");
        assert_eq!(blocks[4].metadata().attributes[0].value, "stem");
    }

    #[test]
    fn literal_block_preserves_empty_and_multiline_contents() {
        let source = "....\n<tag>\n*not strong*\n....\n\n....\n....\n";
        let parsed = parse(source).expect("valid source");
        let literals = parsed
            .ast
            .blocks()
            .iter()
            .filter_map(|block| match block {
                AstBlock::Delimited(block) if block.kind == DelimitedBlockKind::Literal => {
                    Some(block)
                }
                _ => None,
            })
            .collect::<Vec<_>>();

        assert_eq!(literals.len(), 2);
        assert!(matches!(
            &literals[0].content,
            DelimitedContent::Verbatim(value) if value == "<tag>\n*not strong*\n"
        ));
        assert!(matches!(
            &literals[1].content,
            DelimitedContent::Verbatim(value) if value.is_empty()
        ));
        assert!(literals.iter().all(|literal| literal.problems.is_empty()));
        assert_eq!(parsed.syntax.reconstruct(), source);
    }

    #[test]
    fn literal_block_recovers_at_heading_when_unclosed() {
        let source = "....\ncontent\n== Next\nparagraph";
        let parsed = parse(source).expect("valid source");
        let AstBlock::Delimited(literal) = &parsed.ast.blocks()[0] else {
            panic!("expected literal");
        };

        assert!(matches!(
            &literal.content,
            DelimitedContent::Verbatim(value) if value == "content\n"
        ));
        assert!(literal.problems.is_empty());
        assert!(
            parsed
                .syntax
                .issues()
                .iter()
                .any(|issue| issue.class == crate::syntax::SyntaxIssueClass::UnclosedBlock)
        );
        assert!(matches!(parsed.ast.blocks()[1], AstBlock::Heading(_)));
        assert!(matches!(parsed.ast.blocks()[2], AstBlock::Paragraph(_)));
    }

    #[test]
    fn delimited_containers_have_typed_content_models() {
        let source = "////\ncomment\n////\n\n----\nlisting\n----\n\n++++\n<b>raw</b>\n++++\n\n|===\na |b\n|===\n\n====\nparagraph\n====\n";
        let parsed = parse(source).expect("containers");
        let containers = parsed
            .ast
            .blocks()
            .iter()
            .filter_map(|block| match block {
                AstBlock::Delimited(block) => Some(block),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(containers.len(), 5);
        assert!(matches!(
            containers[0].content,
            DelimitedContent::Verbatim(_)
        ));
        assert!(matches!(
            containers[1].content,
            DelimitedContent::Verbatim(_)
        ));
        assert!(matches!(
            containers[2].content,
            DelimitedContent::Passthrough(_)
        ));
        assert!(matches!(containers[3].content, DelimitedContent::Table(_)));
        assert!(matches!(
            &containers[4].content,
            DelimitedContent::Compound(children)
                if matches!(&children[..], [AstBlock::Paragraph(_)])
        ));
        assert_eq!(parsed.syntax.nodes(SyntaxKind::Paragraph).count(), 1);
        assert_eq!(parsed.syntax.reconstruct(), source);
    }

    #[test]
    fn every_standard_container_delimiter_has_one_kind() {
        for (delimiter, expected) in [
            ("////", DelimitedBlockKind::Comment),
            ("====", DelimitedBlockKind::Example),
            ("----", DelimitedBlockKind::Listing),
            ("....", DelimitedBlockKind::Literal),
            ("--", DelimitedBlockKind::Open),
            ("****", DelimitedBlockKind::Sidebar),
            ("++++", DelimitedBlockKind::Pass),
            ("____", DelimitedBlockKind::Quote),
            ("|===", DelimitedBlockKind::Table),
        ] {
            let source = format!("{delimiter}\nbody\n{delimiter}\n");
            let parsed = parse(&source).expect("container");
            let AstBlock::Delimited(block) = &parsed.ast.blocks()[0] else {
                panic!("{delimiter} must create a container");
            };
            assert_eq!(block.kind, expected, "{delimiter}");
        }
    }

    #[test]
    fn compound_containers_nest_when_delimiter_lengths_differ() {
        let source = "=====\nouter\n======\ninner\n======\n=====\n";
        let parsed = parse(source).expect("nested containers");
        let AstBlock::Delimited(outer) = &parsed.ast.blocks()[0] else {
            panic!("outer container");
        };
        let DelimitedContent::Compound(children) = &outer.content else {
            panic!("compound outer");
        };
        assert!(matches!(children[0], AstBlock::Paragraph(_)));
        let AstBlock::Delimited(inner) = &children[1] else {
            panic!("inner container");
        };
        assert_eq!(inner.delimiter, "======");
        assert!(matches!(
            &inner.content,
            DelimitedContent::Compound(inner) if matches!(&inner[..], [AstBlock::Paragraph(_)])
        ));
    }

    #[test]
    fn container_styles_are_preserved_as_metadata_without_host_semantics() {
        let source = "[verse]\n____\nline\n____\n\n[NOTE]\n====\nwarning\n====\n";
        let parsed = parse(source).expect("styled containers");
        let AstBlock::Delimited(verse) = &parsed.ast.blocks()[0] else {
            panic!("verse container");
        };
        let AstBlock::Delimited(admonition) = &parsed.ast.blocks()[1] else {
            panic!("admonition container");
        };
        assert_eq!(verse.kind, DelimitedBlockKind::Quote);
        assert_eq!(verse.metadata.attributes[0].value, "verse");
        assert_eq!(admonition.kind, DelimitedBlockKind::Example);
        assert_eq!(admonition.metadata.attributes[0].value, "NOTE");
    }

    #[test]
    fn source_block_keeps_language_code_and_ranges() {
        let source = "[source, rust]\n----\nfn main() {}\n----\n";
        let parsed = parse(source).expect("valid source");
        let AstBlock::Source(block) = &parsed.ast.blocks()[0] else {
            panic!("expected source block");
        };

        assert_eq!(block.language.as_deref(), Some("rust"));
        let language_range = block.language_range.expect("language range");
        assert_eq!(
            &source[language_range.start().to_usize()..language_range.end().to_usize()],
            "rust"
        );
        assert_eq!(block.value, "fn main() {}\n");
        assert!(block.problems.is_empty());
        assert_eq!(parsed.syntax.reconstruct(), source);
    }

    #[test]
    fn source_block_handles_missing_language_empty_and_unclosed() {
        let parsed = parse("[source]\n----\n== Next\n").expect("valid source");
        let AstBlock::Source(block) = &parsed.ast.blocks()[0] else {
            panic!("expected source block");
        };

        assert!(block.language.is_none());
        assert_eq!(block.value, "");
        assert!(block.problems.is_empty());
        assert!(parsed.syntax.issues().iter().any(|issue| {
            issue.class == crate::syntax::SyntaxIssueClass::MissingSourceLanguage
        }));
        assert!(
            parsed
                .syntax
                .issues()
                .iter()
                .any(|issue| issue.class == crate::syntax::SyntaxIssueClass::UnclosedBlock)
        );
        assert!(matches!(parsed.ast.blocks()[1], AstBlock::Heading(_)));
    }

    #[test]
    fn heading_parser_distinguishes_title_and_levels_one_to_five() {
        let source = "= Title\n\n== One\n=== Two\n==== Three\n===== Four\n====== Five\n";
        let parsed = parse(source).expect("valid source");
        let headings = parsed
            .ast
            .blocks()
            .iter()
            .filter_map(|block| match block {
                AstBlock::Heading(heading) => Some(heading),
                _ => None,
            })
            .collect::<Vec<_>>();

        assert_eq!(headings.len(), 6);
        assert_eq!(headings[0].kind, HeadingKind::DocumentTitle);
        for (index, heading) in headings[1..].iter().enumerate() {
            assert_eq!(
                heading.kind,
                HeadingKind::Section {
                    level: (index + 1) as u8,
                }
            );
            assert!(heading.problems.is_empty());
        }
    }

    #[test]
    fn heading_parser_keeps_marker_separator_and_text_ranges() {
        let parsed = parse("== 日本語").expect("valid source");
        let AstBlock::Heading(heading) = &parsed.ast.blocks()[0] else {
            panic!("expected heading");
        };

        assert_eq!(heading.marker_range.start().to_u32(), 0);
        assert_eq!(heading.marker_range.end().to_u32(), 2);
        assert_eq!(heading.separator_range.start().to_u32(), 2);
        assert_eq!(heading.separator_range.end().to_u32(), 3);
        assert_eq!(heading.text_range.start().to_u32(), 3);
        assert_eq!(heading.text_range.end().to_u32(), 12);
    }

    #[test]
    fn heading_parser_preserves_malformed_headings_and_recovers() {
        let parsed = parse("==Missing\n\n======= Too deep\n\nafter").expect("valid source");
        let AstBlock::Heading(first) = &parsed.ast.blocks()[0] else {
            panic!("expected malformed heading");
        };
        assert!(first.problems.is_empty());
        let AstBlock::Heading(second) = &parsed.ast.blocks()[1] else {
            panic!("expected malformed heading");
        };
        assert!(second.problems.is_empty());
        assert!(
            parsed.syntax.issues().iter().any(|issue| {
                issue.class == crate::syntax::SyntaxIssueClass::HeadingMarkerSpace
            })
        );
        assert!(
            parsed.syntax.issues().iter().any(|issue| {
                issue.class == crate::syntax::SyntaxIssueClass::InvalidHeadingLevel
            })
        );
        assert!(matches!(parsed.ast.blocks()[2], AstBlock::Paragraph(_)));
    }

    #[test]
    fn paragraph_parser_matches_cst_and_ast_fixtures() {
        let source = include_str!("../../../fixtures/paragraph/basic.adoc");
        let parsed = parse(source).expect("valid source");

        assert_eq!(
            parsed.syntax.snapshot(),
            include_str!("../../../fixtures/paragraph/basic.syntax")
        );
        assert_eq!(
            parsed.ast.snapshot(),
            include_str!("../../../fixtures/paragraph/basic.ast")
        );
    }

    #[test]
    fn lists_build_recursive_semantic_nodes() {
        let parsed = parse("* one\n** nested\n* two\n. ordered\n").expect("parse");
        let AstBlock::List(unordered) = &parsed.ast.blocks()[0] else {
            panic!("unordered list");
        };
        assert_eq!(unordered.items.len(), 2);
        assert_eq!(unordered.items[0].children[0].items[0].text, "nested");
        assert!(matches!(parsed.ast.blocks()[1], AstBlock::List(_)));
    }

    #[test]
    fn list_continuation_attaches_literal_and_source_blocks() {
        let source =
            "* item\n+\n....\nliteral\n....\n* code\n+\n[source,rust]\n----\nfn main() {}\n----\n";
        let parsed = parse(source).expect("parse");
        let AstBlock::List(list) = &parsed.ast.blocks()[0] else {
            panic!("list");
        };
        assert!(matches!(
            list.items[0].continuations[0],
            AstBlock::Delimited(DelimitedBlock {
                kind: DelimitedBlockKind::Literal,
                ..
            })
        ));
        assert!(matches!(
            list.items[1].continuations[0],
            AstBlock::Source(_)
        ));
    }

    #[test]
    fn standard_list_forms_have_typed_terms_checklists_callouts_and_mixed_continuations() {
        let source = "Alias::\nTerm:: *definition*\n* [ ] todo\n* [x] done\n[source,rust]\n----\nlet value = 1; // <1>\n----\n<1> binding\n* compound\n+\nattached paragraph\n+\n====\ninside\n====\n";
        let parsed = parse(source).expect("parse");
        assert_eq!(parsed.syntax.reconstruct(), source);

        let AstBlock::List(description) = &parsed.ast.blocks()[0] else {
            panic!("description list");
        };
        assert_eq!(description.kind, ListKind::Description);
        assert_eq!(description.items[0].terms.len(), 2);
        assert_eq!(description.items[0].terms[0].text, "Alias");
        assert_eq!(description.items[0].terms[1].text, "Term");

        let AstBlock::List(checklist) = &parsed.ast.blocks()[1] else {
            panic!("checklist");
        };
        assert_eq!(
            checklist.items[0].checklist,
            Some(ChecklistState::Unchecked)
        );
        assert_eq!(checklist.items[1].checklist, Some(ChecklistState::Checked));

        let AstBlock::Source(source_block) = &parsed.ast.blocks()[2] else {
            panic!("source block");
        };
        assert_eq!(source_block.callouts[0].id, 1);
        let AstBlock::List(callouts) = &parsed.ast.blocks()[3] else {
            panic!("callout list");
        };
        assert_eq!(callouts.kind, ListKind::Callout);
        assert_eq!(callouts.items[0].callout_id, Some(1));

        let AstBlock::List(compound) = &parsed.ast.blocks()[4] else {
            panic!("compound list");
        };
        assert!(matches!(
            compound.items[0].continuations[0],
            AstBlock::Paragraph(_)
        ));
        assert!(matches!(
            compound.items[0].continuations[1],
            AstBlock::Delimited(_)
        ));
    }

    #[test]
    fn stem_builds_opaque_inline_and_block_nodes() {
        let parsed =
            parse(include_str!("../../../fixtures/stem/substitutions.adoc")).expect("parse");
        let AstBlock::Paragraph(paragraph) = &parsed.ast.blocks()[1] else {
            panic!("paragraph");
        };
        assert!(paragraph.inlines.iter().any(|inline| {
            matches!(
                inline,
                Inline::Formula(formula)
                    if formula.value == "{x} * y < z"
                        && formula.language == MathLanguage::Latex
            )
        }));
        let AstBlock::Math(math) = &parsed.ast.blocks()[2] else {
            panic!("math block");
        };
        assert!(math.value.contains("{x} * y < z"));
    }

    #[test]
    fn stem_recovery_keeps_unclosed_block_before_heading() {
        let parsed = parse("stem:[inline open\n\n[stem]\n++++\nx + y\n== Next\n").expect("parse");
        let AstBlock::Paragraph(paragraph) = &parsed.ast.blocks()[0] else {
            panic!("paragraph");
        };
        assert!(matches!(
            paragraph.inlines[0],
            Inline::Formula(ref formula) if !formula.closed && formula.value == "inline open"
        ));
        let AstBlock::Math(math) = &parsed.ast.blocks()[1] else {
            panic!("math");
        };
        assert!(math.problems.is_empty());
        assert!(
            parsed
                .syntax
                .issues()
                .iter()
                .any(|issue| issue.class == crate::syntax::SyntaxIssueClass::InvalidStem)
        );
        assert!(matches!(parsed.ast.blocks()[2], AstBlock::Heading(_)));
    }

    #[test]
    fn stem_language_boundary_keeps_latex_distinct_from_future_typst() {
        assert_ne!(MathLanguage::Latex, MathLanguage::Typst);
        let parsed = parse("stem:[x]").expect("parse");
        let AstBlock::Paragraph(paragraph) = &parsed.ast.blocks()[0] else {
            panic!("paragraph");
        };
        assert!(matches!(
            paragraph.inlines[0],
            Inline::Formula(ref formula) if formula.language == MathLanguage::Latex
        ));
    }

    #[test]
    fn document_header_preserves_author_revision_doctype_and_preamble() {
        let parsed = parse("= Title\nJane Doe <jane@example.org>\nv2.1, 2026-07-21: Stable\n:doctype: book\n\nIntro.\n\n= Part One\n\n== Chapter\n").expect("parse");
        let header = parsed.ast.header();
        assert_eq!(header.doctype, DocumentType::Book);
        assert_eq!(header.authors[0].name, "Jane Doe");
        assert_eq!(header.authors[0].email.as_deref(), Some("jane@example.org"));
        assert_eq!(
            header
                .revision
                .as_ref()
                .and_then(|revision| revision.number.as_ref())
                .map(|value| value.value.as_str()),
            Some("v2.1")
        );
        assert!(matches!(
            parsed.ast.blocks()[2],
            AstBlock::Heading(Heading {
                kind: HeadingKind::Part,
                ..
            })
        ));
        assert_eq!(parsed.ast.preamble().len(), 1);
    }

    #[test]
    fn discrete_headings_do_not_become_sections() {
        let parsed = parse("= Title\n\n[discrete]\n== Aside\n").expect("parse");
        assert!(matches!(
            parsed.ast.blocks()[1],
            AstBlock::Heading(Heading {
                kind: HeadingKind::Discrete { level: 1 },
                ..
            })
        ));
        assert_eq!(crate::document::document_symbols(&parsed.ast).len(), 1);
    }

    #[test]
    fn paragraph_forms_and_breaks_have_typed_nodes() {
        let parsed =
            parse("line one +\nline two\n\n literal <text>\n next\n\n'''\n\n<<<\n").expect("parse");
        let AstBlock::Paragraph(paragraph) = &parsed.ast.blocks()[0] else {
            panic!("paragraph")
        };
        assert!(
            paragraph
                .inlines
                .iter()
                .any(|inline| matches!(inline, Inline::HardBreak { .. }))
        );
        assert!(
            matches!(&parsed.ast.blocks()[1], AstBlock::LiteralParagraph(node) if node.value == "literal <text>\nnext")
        );
        assert!(matches!(
            &parsed.ast.blocks()[2],
            AstBlock::Break(BreakBlock {
                kind: BreakKind::Thematic,
                ..
            })
        ));
        assert!(matches!(
            &parsed.ast.blocks()[3],
            AstBlock::Break(BreakBlock {
                kind: BreakKind::Page,
                ..
            })
        ));
    }

    #[test]
    fn psv_tables_build_typed_rows_cells_spans_and_multiline_content() {
        let source = "[cols=\"1,^2s\",options=\"header,footer\"]\n|===\n|Name |Value\n\n|first\ncontinued\n|second\n\n2+|wide\n\n|Foot |Done\n|===\n";
        let parsed = parse(source).expect("parse");
        let AstBlock::Delimited(block) = &parsed.ast.blocks()[0] else {
            panic!("table block")
        };
        let DelimitedContent::Table(table) = &block.content else {
            panic!("typed table")
        };
        assert_eq!(table.columns.len(), 2);
        assert_eq!(table.rows.len(), 4);
        assert_eq!(table.rows[0].section, crate::table::TableSection::Header);
        assert_eq!(table.rows[3].section, crate::table::TableSection::Footer);
        assert_eq!(table.rows[1].cells[0].raw, "first\ncontinued");
        assert_eq!(table.rows[2].cells[0].column_span, 2);
        assert_eq!(table.rows[2].cells[0].column_index, 0);
        assert_eq!(parsed.syntax.reconstruct(), source);
    }

    #[test]
    fn separated_table_formats_and_duplication_share_the_table_model() {
        let source = "[format=csv,options=header]\n|===\nname,description\nalpha,\"one, two\"\nbeta,\"line one\nline two\"\n|===\n\n[format=tsv]\n|===\na\tb\n|===\n\n|===\n3*|same\n|===\n";
        let parsed = parse(source).expect("parse");
        let tables = parsed
            .ast
            .blocks()
            .iter()
            .filter_map(|block| match block {
                AstBlock::Delimited(crate::parser::DelimitedBlock {
                    content: DelimitedContent::Table(table),
                    ..
                }) => Some(table),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(tables.len(), 3);
        assert_eq!(tables[0].format, crate::table::TableFormat::Csv);
        assert_eq!(tables[0].rows.len(), 3);
        assert_eq!(tables[0].rows[1].cells[1].raw, "one, two");
        assert_eq!(tables[0].rows[2].cells[1].raw, "line one\nline two");
        assert_eq!(tables[1].format, crate::table::TableFormat::Tsv);
        assert_eq!(tables[1].separator, '\t');
        assert_eq!(tables[2].rows[0].cells.len(), 3);
    }

    #[test]
    fn asciidoc_table_cells_are_parsed_as_nested_blocks() {
        crate::source_document::SourceDocument::reset_construction_count();
        let source = "[cols=a]\n|===\n|A paragraph.\n\n* one\n* two\n|===\n";
        let parsed = parse(source).expect("parse");
        assert_eq!(
            crate::source_document::SourceDocument::construction_count(),
            1,
            "source-backed PSV cells must not rebuild the line index"
        );
        assert_eq!(
            crate::source_document::SourceDocument::indexed_view_count(),
            1
        );
        let AstBlock::Delimited(crate::parser::DelimitedBlock {
            content: DelimitedContent::Table(table),
            ..
        }) = &parsed.ast.blocks()[0]
        else {
            panic!("table")
        };
        let crate::table::TableCellContent::AsciiDoc(blocks) = &table.rows[0].cells[0].content
        else {
            panic!("AsciiDoc cell")
        };
        assert!(matches!(blocks[0], AstBlock::Paragraph(_)));
        assert!(matches!(blocks[1], AstBlock::List(_)));
        assert_eq!(parsed.syntax.reconstruct(), source);
    }

    #[test]
    fn asciidoc_cells_use_the_complete_block_dispatch_and_document_anchor_index() {
        let source = "[cols=a]\n|===\n|[[cell-target]]\n[discrete]\n== Cell heading\n\n literal\n\n'''\n\n<<<\n\n.Cell source\n[source,rust]\n----\nfn main() {}\n----\n\n[stem]\n++++\nx + y\n++++\n\n!===\n!nested\n!===\n|===\n";
        let parsed = parse(source).expect("parse");
        let AstBlock::Delimited(crate::parser::DelimitedBlock {
            content: DelimitedContent::Table(table),
            ..
        }) = &parsed.ast.blocks()[0]
        else {
            panic!("table")
        };
        let crate::table::TableCellContent::AsciiDoc(blocks) = &table.rows[0].cells[0].content
        else {
            panic!("AsciiDoc cell")
        };
        assert!(matches!(
            blocks[0],
            AstBlock::Heading(Heading {
                kind: HeadingKind::Discrete { level: 1 },
                ..
            })
        ));
        assert!(matches!(blocks[1], AstBlock::LiteralParagraph(_)));
        assert!(matches!(
            blocks[2],
            AstBlock::Break(BreakBlock {
                kind: BreakKind::Thematic,
                ..
            })
        ));
        assert!(matches!(
            blocks[3],
            AstBlock::Break(BreakBlock {
                kind: BreakKind::Page,
                ..
            })
        ));
        assert!(matches!(
            &blocks[4],
            AstBlock::Source(block)
                if block.language.as_deref() == Some("rust")
                    && block.metadata.title.as_ref().map(|title| title.value.as_str())
                        == Some("Cell source")
        ));
        assert!(matches!(blocks[5], AstBlock::Math(_)));
        let AstBlock::Delimited(crate::parser::DelimitedBlock {
            content: DelimitedContent::Table(nested),
            ..
        }) = &blocks[6]
        else {
            panic!("nested table")
        };
        assert_eq!(nested.separator, '!');
        assert_eq!(nested.rows[0].cells[0].raw, "nested");
        assert!(parsed.ast.anchors().iter().any(|anchor| {
            anchor.id == "cell-target"
                && anchor.valid
                && anchor.target_range == Some(blocks[0].range())
        }));
        assert!(
            crate::document::reference_targets(&parsed.ast)
                .iter()
                .any(|target| {
                    target.id == "cell-target" && target.target_range == blocks[0].range()
                })
        );
        assert!(
            crate::html::render(&parsed.ast, &crate::html::RenderPolicy::default())
                .html
                .contains("<h1 id=\"cell-target\">Cell heading</h1>")
        );
        assert_eq!(parsed.syntax.reconstruct(), source);
    }

    #[test]
    fn asciidoc_cell_syntax_problems_join_the_root_diagnostic_stream() {
        let parsed = parse("[cols=a]\n|===\n|[source]\n----\ncode\n----\n|===\n").expect("parse");
        assert!(parsed.syntax.issues().iter().any(|issue| {
            issue.class == crate::syntax::SyntaxIssueClass::MissingSourceLanguage
        }));
    }

    #[test]
    fn asciidoc_cell_context_policy_covers_every_block_variant() {
        #[derive(Clone, Copy, Debug)]
        enum Expected {
            Heading,
            Paragraph,
            LiteralParagraph,
            Break,
            Literal,
            Source,
            List,
            Math,
            Delimited,
            Unsupported,
        }
        let cases = [
            ("== heading\n", Expected::Heading),
            ("paragraph\n", Expected::Paragraph),
            ("first\n\n literal\n", Expected::LiteralParagraph),
            ("'''\n", Expected::Break),
            ("* item\n+\n....\nliteral\n....\n", Expected::Literal),
            (
                "[source,rust]\n----\nfn main() {}\n----\n",
                Expected::Source,
            ),
            ("* item\n", Expected::List),
            ("[stem]\n++++\nx\n++++\n", Expected::Math),
            ("====\ninside\n====\n", Expected::Delimited),
            (
                "[source,rust,extra]\n----\ninside\n----\n",
                Expected::Unsupported,
            ),
        ];
        for (cell_source, expected) in cases {
            let source = format!("[cols=a]\n|===\n|{cell_source}|===\n");
            let parsed = parse(&source).expect("parse cell case");
            let AstBlock::Delimited(crate::parser::DelimitedBlock {
                content: DelimitedContent::Table(table),
                ..
            }) = &parsed.ast.blocks()[0]
            else {
                panic!("table for {expected:?}")
            };
            let crate::table::TableCellContent::AsciiDoc(blocks) = &table.rows[0].cells[0].content
            else {
                panic!("AsciiDoc cell for {expected:?}")
            };
            let mut found = false;
            crate::walker::walk_block_slice(blocks, |node| {
                let crate::walker::SemanticNode::Block(block) = node else {
                    return;
                };
                found |= matches!(
                    (expected, block),
                    (Expected::Heading, AstBlock::Heading(_))
                        | (Expected::Paragraph, AstBlock::Paragraph(_))
                        | (Expected::LiteralParagraph, AstBlock::LiteralParagraph(_))
                        | (Expected::Break, AstBlock::Break(_))
                        | (
                            Expected::Literal,
                            AstBlock::Delimited(DelimitedBlock {
                                kind: DelimitedBlockKind::Literal,
                                ..
                            })
                        )
                        | (Expected::Source, AstBlock::Source(_))
                        | (Expected::List, AstBlock::List(_))
                        | (Expected::Math, AstBlock::Math(_))
                        | (Expected::Delimited, AstBlock::Delimited(_))
                        | (Expected::Unsupported, AstBlock::Unsupported(_))
                );
            });
            assert!(found, "missing {expected:?}: {blocks:?}");
            assert_eq!(parsed.syntax.reconstruct(), source);
        }
    }

    #[test]
    fn shorthand_anchor_never_overlaps_recovered_block_metadata() {
        let source = "= Seed\n\n[[target]]\n[source,r(TM)\n----\nfn,rut]\n-------reference>>\n\n* item\n+\n[source,rust]\n--.-\nfn main() {}\n----\n";
        let parsed = parse(source).expect("parse");
        assert_eq!(parsed.syntax.reconstruct(), source);
    }
}
