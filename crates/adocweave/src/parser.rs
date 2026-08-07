//! Lossless concrete syntax and HTML-independent semantic syntax.

use std::collections::BTreeMap;
use std::sync::Arc;

use crate::attributes::parse_lines as parse_attribute_lines;
use crate::block_grammar::{
    LineRecognition, is_block_title, parse_block_attributes, parse_explicit_anchor,
    parse_math_attribute, parse_source_attribute, recognize_line, unsupported_reason,
    valid_anchor_id,
};
pub use crate::block_model::*;
use crate::block_sequence::{
    BlockConsumption, BlockContext, BlockCursor, BlockFacts, BlockInput, BlockLocation,
    BlockRecognition, BlockSequenceOutput, ParseDepth, RootBlockSequenceOutput,
};
use crate::budget::{BudgetExceeded, ParseBudget, ParseBudgetCharge};
use crate::delimiter::{DelimitedContentModel, DelimiterSpec};
use crate::document_header::DocumentHeaderState;
use crate::inline::parse_with_budget_impl as parse_inlines;
use crate::inline::{Inline, InlineParseConfig};
use crate::limits::AnalysisLimits;
use crate::list_parser::{FlatListItem, ParsedListMarker};
use crate::parser_support::{ParseFailure, ParseState};
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

    fn push_title(&mut self, value: BlockTitle, line_range: TextRange) {
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

    fn push_interstitial_syntax(&mut self, syntax: SyntaxNode) {
        debug_assert!(!self.is_empty());
        self.syntax.push(syntax);
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

struct BlockParserState {
    cursor: BlockCursor,
    document_header_phase: Option<DocumentHeaderState>,
    pending_metadata: PendingBlockMetadata,
    paragraph_lines: Vec<(SourceLine, String)>,
    saw_content: bool,
    context: BlockContext,
}

impl BlockParserState {
    fn new(input: &BlockInput<'_>, context: BlockContext) -> Self {
        Self {
            cursor: BlockCursor::for_range(&input.lines),
            document_header_phase: context
                .allows_document_header()
                .then(DocumentHeaderState::default),
            pending_metadata: PendingBlockMetadata::default(),
            paragraph_lines: Vec::new(),
            saw_content: false,
            context,
        }
    }

    fn current_line(&self) -> Option<usize> {
        self.cursor.current()
    }

    fn document_attribute_position(
        &self,
        source_document: &SourceDocument,
        line_index: usize,
    ) -> bool {
        self.document_header_phase.as_ref().is_some_and(|header| {
            header.attributes_open
                || body_attribute_has_blank_offset(
                    source_document,
                    line_index,
                    self.saw_content || !header.attributes_open,
                )
        })
    }

    fn document_title_position(&self) -> bool {
        self.context.document_title_position(self.saw_content)
    }

    fn take_author_expectation(&mut self) -> bool {
        let Some(header) = self.document_header_phase.as_mut() else {
            return false;
        };
        let expected = header.expect_author;
        header.expect_author = false;
        expected
    }

    fn record_author(&mut self, author: Author, line_range: TextRange) {
        let header = self
            .document_header_phase
            .as_mut()
            .expect("only root documents recognize authors");
        header.extend_range(line_range);
        header.header.authors.push(author);
        header.expect_revision = true;
    }

    fn take_revision_expectation(&mut self) -> bool {
        let Some(header) = self.document_header_phase.as_mut() else {
            return false;
        };
        let expected = header.expect_revision;
        header.expect_revision = false;
        expected
    }

    fn record_revision(&mut self, revision: Revision, line_range: TextRange) {
        let header = self
            .document_header_phase
            .as_mut()
            .expect("only root documents recognize revisions");
        header.extend_range(line_range);
        header.header.revision = Some(revision);
    }

    fn close_header_attributes(&mut self) {
        self.document_header_phase
            .iter_mut()
            .for_each(DocumentHeaderState::close_attributes);
    }

    fn stop_header_author_revision(&mut self) {
        self.document_header_phase
            .iter_mut()
            .for_each(DocumentHeaderState::stop_author_revision);
    }

    fn close_header_at_blank(&mut self, line_range: TextRange) {
        let Some(header) = self.document_header_phase.as_mut() else {
            return;
        };
        if header.attributes_open && header.header.range.is_some() {
            header.attributes_open = false;
            header.header.end = line_range.start();
        }
    }

    fn open_header_after_title(&mut self, line_range: TextRange) {
        let header = self
            .document_header_phase
            .as_mut()
            .expect("only root documents recognize document titles");
        header.attributes_open = true;
        header.extend_range(line_range);
        header.expect_author = true;
    }

    fn mark_content_seen(&mut self) {
        self.saw_content = true;
    }

    fn mark_body_content(&mut self) {
        self.mark_content_seen();
        self.close_header_attributes();
    }

    fn push_paragraph_line(&mut self, line: SourceLine, content: &str) {
        self.paragraph_lines.push((line, content.to_owned()));
        self.mark_body_content();
    }

    fn flush_paragraph(
        &mut self,
        syntax_blocks: &mut Vec<SyntaxNode>,
        ast_blocks: &mut Vec<AstBlock>,
        config: &ParseConfig,
        budget: &mut ParseBudget,
    ) -> Result<(), ParseFailure> {
        flush_paragraph(
            syntax_blocks,
            ast_blocks,
            &mut self.paragraph_lines,
            config,
            budget,
            &mut self.pending_metadata,
        )
    }

    fn flush_orphan_metadata(
        &mut self,
        syntax_blocks: &mut Vec<SyntaxNode>,
        ast_blocks: &mut Vec<AstBlock>,
        source: &str,
        budget: &mut ParseBudget,
    ) -> Result<(), ParseFailure> {
        Ok(flush_orphan_metadata(
            syntax_blocks,
            ast_blocks,
            &mut self.pending_metadata,
            source,
            budget,
        )?)
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
    pub limits: AnalysisLimits,
}

impl Default for ParseConfig {
    fn default() -> Self {
        let limits = AnalysisLimits {
            max_line_bytes: u32::MAX,
            max_blocks: u32::MAX,
            max_nodes: u32::MAX,
            max_references: u32::MAX,
            max_attributes: u32::MAX,
            ..AnalysisLimits::default()
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
    match parse_shared_cancellable(source, config, &BTreeMap::new(), &crate::core::NeverCancel) {
        Ok(document) => Ok(document),
        Err(ParseFailure::Position(error)) => Err(error),
        Err(
            ParseFailure::Cancelled | ParseFailure::Budget(_) | ParseFailure::InternalInvariant,
        ) => {
            unreachable!("default test parser cannot be cancelled or exhaust its budget")
        }
    }
}

#[derive(Debug)]
struct BlockCommit {
    syntax: SyntaxNode,
    block: AstBlock,
    charge: ParseBudgetCharge,
}

impl BlockCommit {
    fn single(syntax: SyntaxNode, block: AstBlock, attributes: usize) -> Self {
        Self {
            syntax,
            block,
            charge: ParseBudgetCharge {
                blocks: 1,
                nodes: 1,
                attributes,
            },
        }
    }

    fn already_charged(syntax: SyntaxNode, block: AstBlock) -> Self {
        Self {
            syntax,
            block,
            charge: ParseBudgetCharge::default(),
        }
    }
}

fn commit_recognized_block(
    state: &mut BlockParserState,
    recognition: BlockRecognition<BlockCommit>,
    syntax_blocks: &mut Vec<SyntaxNode>,
    ast_blocks: &mut Vec<AstBlock>,
    budget: &mut ParseBudget,
) -> Result<bool, ParseFailure> {
    let Some((consumption, commit)) = recognition.into_commit() else {
        return Ok(false);
    };
    state.cursor.validate(consumption)?;
    budget.charge(commit.charge)?;
    state.cursor.commit(consumption)?;
    syntax_blocks.push(commit.syntax);
    ast_blocks.push(commit.block);
    attach_pending_metadata(syntax_blocks, ast_blocks, &mut state.pending_metadata);
    Ok(true)
}

fn recognize_source_or_math(
    recognition: LineRecognition,
    context: &DelimitedParseContext<'_>,
    line_index: usize,
    end_line: usize,
    content: &str,
    line: SourceLine,
) -> Result<BlockRecognition<BlockCommit>, ParseFailure> {
    match recognition {
        LineRecognition::Source => {
            let (mut source_block, next_line) = parse_source_block(
                context.source_document,
                line_index,
                context.source,
                end_line,
            )?;
            source_block.metadata =
                parse_block_attributes(content, line.content_range().start().to_usize())
                    .unwrap_or_default();
            let attribute_count = metadata_attribute_count(&source_block.metadata);
            source_block.metadata.range = Some(line.full_range());
            let syntax = crate::syntax_builder::source(&source_block);
            let recovered = !source_block.problems.is_empty();
            let commit =
                BlockCommit::single(syntax, AstBlock::Source(source_block), attribute_count);
            Ok(if recovered {
                BlockRecognition::recovered(BlockConsumption::Through(next_line), commit)
            } else {
                BlockRecognition::matched(BlockConsumption::Through(next_line), commit)
            })
        }
        LineRecognition::InvalidSource => Ok(BlockRecognition::recovered(
            BlockConsumption::OneLine,
            BlockCommit::single(
                SyntaxNode::new(
                    SyntaxKind::Unsupported,
                    line.full_range(),
                    vec![SyntaxNode::leaf(SyntaxKind::Unknown, line.full_range())],
                ),
                AstBlock::Unsupported(Unsupported {
                    metadata: BlockMetadata::default(),
                    range: line.full_range(),
                    raw: content.to_owned(),
                    reason: "invalid source block attribute".to_owned(),
                    kind: UnsupportedKind::Syntax,
                }),
                0,
            ),
        )),
        LineRecognition::Math => {
            let (mut math, next_line) = parse_math_block(
                context.source_document,
                line_index,
                context.source,
                context.config,
                end_line,
            )?;
            math.metadata =
                parse_block_attributes(content, line.content_range().start().to_usize())
                    .unwrap_or_default();
            let attribute_count = metadata_attribute_count(&math.metadata);
            math.metadata.range = Some(line.full_range());
            let syntax = crate::syntax_builder::math(&math);
            let recovered = !math.problems.is_empty();
            let commit = BlockCommit::single(syntax, AstBlock::Math(math), attribute_count);
            Ok(if recovered {
                BlockRecognition::recovered(BlockConsumption::Through(next_line), commit)
            } else {
                BlockRecognition::matched(BlockConsumption::Through(next_line), commit)
            })
        }
        _ => Err(ParseFailure::InternalInvariant),
    }
}

fn recognize_simple_block(
    recognition: LineRecognition,
    source_document: &SourceDocument,
    line_index: usize,
    content: &str,
    line: SourceLine,
) -> Result<BlockRecognition<BlockCommit>, ParseFailure> {
    let commit = match recognition {
        LineRecognition::Break => {
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
            BlockCommit::single(
                SyntaxNode::leaf(syntax_kind, line.full_range()),
                AstBlock::Break(BreakBlock {
                    metadata: BlockMetadata::default(),
                    range: line.full_range(),
                    kind,
                }),
                0,
            )
        }
        LineRecognition::LiteralParagraph => {
            let (literal, next_line) = parse_literal_paragraph(source_document, line_index)?;
            return Ok(BlockRecognition::matched(
                BlockConsumption::Through(next_line),
                BlockCommit::single(
                    SyntaxNode::leaf(SyntaxKind::LiteralBlock, literal.range),
                    AstBlock::LiteralParagraph(literal),
                    0,
                ),
            ));
        }
        LineRecognition::PreprocessorDirective => {
            let reason = crate::preprocessor::classify_line(content)
                .map(crate::block_grammar::directive_reason)
                .ok_or(ParseFailure::InternalInvariant)?;
            BlockCommit::single(
                SyntaxNode::new(
                    SyntaxKind::Unsupported,
                    line.full_range(),
                    vec![SyntaxNode::leaf(SyntaxKind::Unknown, line.full_range())],
                ),
                AstBlock::Unsupported(Unsupported {
                    metadata: BlockMetadata::default(),
                    range: line.full_range(),
                    raw: content.to_owned(),
                    reason: reason.to_owned(),
                    kind: UnsupportedKind::UnprocessedDirective,
                }),
                0,
            )
        }
        LineRecognition::Unsupported => {
            let reason = unsupported_reason(content).ok_or(ParseFailure::InternalInvariant)?;
            BlockCommit::single(
                SyntaxNode::new(
                    SyntaxKind::Unsupported,
                    line.full_range(),
                    vec![SyntaxNode::leaf(SyntaxKind::Unknown, line.full_range())],
                ),
                AstBlock::Unsupported(Unsupported {
                    metadata: BlockMetadata::default(),
                    range: line.full_range(),
                    raw: content.to_owned(),
                    reason: reason.to_owned(),
                    kind: UnsupportedKind::Syntax,
                }),
                0,
            )
        }
        _ => return Ok(BlockRecognition::NoMatch),
    };
    Ok(BlockRecognition::matched(BlockConsumption::OneLine, commit))
}

pub(crate) fn parse_shared_cancellable(
    source: Arc<str>,
    config: &ParseConfig,
    external_attributes: &BTreeMap<String, Option<String>>,
    cancellation: &dyn crate::core::CancellationCheck,
) -> Result<ParsedDocument, ParseFailure> {
    let mut budget = ParseBudget::new(config.limits)?;
    let source_document = SourceDocument::from_shared_bounded(
        Arc::clone(&source),
        config.limits.max_line_bytes,
        &|| cancellation.is_cancelled(),
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
        &|| cancellation.is_cancelled(),
        &mut budget,
        BlockContext::root(),
    )?;
    finish_document(
        sequence,
        source_document,
        config,
        external_attributes,
        cancellation,
    )
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
    let mut anchors = Vec::new();
    let mut parser = BlockParserState::new(&input, context);

    let end_line = input.lines.end;
    while let Some(line_index) = parser.current_line() {
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
            .filter(|_| line_index + 1 < end_line)
            .and_then(|next| source_document.text(next.content_range()));

        if !content.trim_matches([' ', '\t']).is_empty()
            && parser.take_author_expectation()
            && !content.chars().any(char::is_control)
            && !content.starts_with([':', '[', '='])
            && crate::delimiter::spec(content).is_none()
            && !content.starts_with("//")
            && let Some(author) = crate::document_header::parse_author(content, line)?
        {
            budget.consume_node()?;
            parser.record_author(author, line.full_range());
            blocks.push(SyntaxNode::leaf(SyntaxKind::AuthorLine, line.full_range()));
            parser.cursor.commit(BlockConsumption::OneLine)?;
            continue;
        }
        if !content.trim_matches([' ', '\t']).is_empty()
            && parser.take_revision_expectation()
            && !content.chars().any(char::is_control)
            && !content.starts_with([':', '[', '='])
            && crate::delimiter::spec(content).is_none()
            && !content.starts_with("//")
        {
            let revision = crate::document_header::parse_revision(content, line)?;
            budget.consume_node()?;
            parser.record_revision(revision, line.full_range());
            blocks.push(SyntaxNode::leaf(
                SyntaxKind::RevisionLine,
                line.full_range(),
            ));
            parser.cursor.commit(BlockConsumption::OneLine)?;
            continue;
        }

        let document_attribute_position =
            parser.document_attribute_position(source_document, line_index);
        let recognition = recognize_line(
            content,
            next_content,
            line.content_range().start().to_usize(),
            line.full_range(),
            document_attribute_position,
        );
        let recognized_block = if matches!(
            recognition,
            LineRecognition::Source
                | LineRecognition::InvalidSource
                | LineRecognition::Math
                | LineRecognition::Break
                | LineRecognition::LiteralParagraph
                | LineRecognition::PreprocessorDirective
                | LineRecognition::Unsupported
        ) {
            parser.flush_paragraph(&mut blocks, &mut ast_blocks, config, budget)?;
            budget.check(ParseBudgetCharge {
                blocks: 1,
                nodes: 1,
                attributes: 0,
            })?;
            if matches!(
                recognition,
                LineRecognition::Source | LineRecognition::InvalidSource | LineRecognition::Math
            ) {
                let recognition_context = DelimitedParseContext {
                    source_document,
                    source,
                    config,
                    is_cancelled,
                };
                recognize_source_or_math(
                    recognition,
                    &recognition_context,
                    line_index,
                    end_line,
                    content,
                    line,
                )?
            } else {
                recognize_simple_block(recognition, source_document, line_index, content, line)?
            }
        } else {
            BlockRecognition::NoMatch
        };
        if commit_recognized_block(
            &mut parser,
            recognized_block,
            &mut blocks,
            &mut ast_blocks,
            budget,
        )? {
            match recognition {
                LineRecognition::Source | LineRecognition::Math => parser.mark_content_seen(),
                LineRecognition::InvalidSource
                | LineRecognition::Break
                | LineRecognition::LiteralParagraph
                | LineRecognition::PreprocessorDirective
                | LineRecognition::Unsupported => parser.mark_body_content(),
                _ => return Err(ParseFailure::InternalInvariant),
            }
            continue;
        }
        if recognition == LineRecognition::Delimited {
            let spec = crate::delimiter::spec(content).expect("recognizer verified delimiter");
            parser.flush_paragraph(&mut blocks, &mut ast_blocks, config, budget)?;
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
                end_line,
                spec,
                &mut state,
                parser.context.depth,
                Some(&parser.pending_metadata.semantic),
            )?;
            let syntax = crate::syntax_builder::delimited(&block, nested_syntax);
            let recognition = if block.problems.is_empty() {
                BlockRecognition::matched(
                    BlockConsumption::Through(next_line),
                    BlockCommit::already_charged(syntax, AstBlock::Delimited(block)),
                )
            } else {
                BlockRecognition::recovered(
                    BlockConsumption::Through(next_line),
                    BlockCommit::already_charged(syntax, AstBlock::Delimited(block)),
                )
            };
            commit_recognized_block(
                &mut parser,
                recognition,
                &mut blocks,
                &mut ast_blocks,
                budget,
            )?;
            parser.mark_body_content();
            continue;
        } else if recognition == LineRecognition::Anchor {
            let anchor = parse_explicit_anchor(
                content,
                line.content_range().start().to_usize(),
                line.full_range(),
            )
            .filter(|_| content.starts_with("[["))
            .expect("recognizer verified anchor");
            parser.flush_paragraph(&mut blocks, &mut ast_blocks, config, budget)?;
            budget.consume_node()?;
            parser.pending_metadata.push_anchor(&anchor);
            anchors.push(anchor);
            parser.mark_body_content();
        } else if recognition == LineRecognition::BlockTitle {
            let title = parse_block_title(
                content,
                line.content_range().start().to_usize(),
                config,
                budget,
            )?
            .expect("recognizer verified block title");
            parser.flush_paragraph(&mut blocks, &mut ast_blocks, config, budget)?;
            budget.consume_node()?;
            budget.consume_attribute()?;
            parser.pending_metadata.push_title(title, line.full_range());
            parser.close_header_attributes();
        } else if recognition == LineRecognition::BlockMetadata {
            let metadata = parse_block_attributes(content, line.content_range().start().to_usize())
                .expect("recognizer verified block metadata");
            parser.flush_paragraph(&mut blocks, &mut ast_blocks, config, budget)?;
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
            parser
                .pending_metadata
                .push_attributes(metadata, line.full_range());
            parser.close_header_attributes();
        } else if recognition == LineRecognition::Comment {
            parser.stop_header_author_revision();
            parser.flush_paragraph(&mut blocks, &mut ast_blocks, config, budget)?;
            let comment = SyntaxNode::leaf(SyntaxKind::CommentLine, line.full_range());
            if parser.pending_metadata.is_empty() {
                blocks.push(comment);
            } else {
                parser.pending_metadata.push_interstitial_syntax(comment);
            }
        } else if recognition == LineRecognition::Blank {
            parser.stop_header_author_revision();
            parser.flush_paragraph(&mut blocks, &mut ast_blocks, config, budget)?;
            parser.flush_orphan_metadata(&mut blocks, &mut ast_blocks, source, budget)?;
            blocks.push(SyntaxNode::leaf(SyntaxKind::BlankLine, line.full_range()));
            parser.close_header_at_blank(line.full_range());
        } else if recognition == LineRecognition::DocumentAttribute {
            let (attribute, problem, last_line) =
                parse_attribute_lines(source_document, line_index, is_cancelled)?
                    .expect("recognizer verified document attribute");
            parser.flush_paragraph(&mut blocks, &mut ast_blocks, config, budget)?;
            parser.flush_orphan_metadata(&mut blocks, &mut ast_blocks, source, budget)?;
            budget.consume_attribute()?;
            budget.consume_node()?;
            let attribute_range = attribute.range;
            blocks.push(SyntaxNode::leaf(
                SyntaxKind::DocumentAttribute,
                attribute_range,
            ));
            let root = parser
                .document_header_phase
                .as_mut()
                .expect("attribute recognition requires root state");
            let in_header = root.attributes_open;
            root.attributes.push(attribute);
            root.attribute_problems.extend(problem);
            if in_header {
                root.extend_range(attribute_range);
            }
            if last_line > line_index {
                parser
                    .cursor
                    .commit(BlockConsumption::Through(last_line + 1))?;
                continue;
            }
        } else if recognition == LineRecognition::Heading {
            parser.flush_paragraph(&mut blocks, &mut ast_blocks, config, budget)?;
            budget.consume_block()?;
            budget.consume_node()?;
            let heading = parse_heading(
                content,
                line,
                parser.document_title_position(),
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
            attach_pending_metadata(&mut blocks, &mut ast_blocks, &mut parser.pending_metadata);
            let opens_header = parser.context.allows_document_header()
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
                parser.open_header_after_title(line.full_range());
            }
            parser.mark_content_seen();
        } else if recognition == LineRecognition::List {
            parser.flush_paragraph(&mut blocks, &mut ast_blocks, config, budget)?;
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
            let (lists, next_line, range) = parse_lists(
                &list_context,
                line_index,
                end_line,
                &mut state,
                parser.context.depth,
            )?;
            blocks.push(crate::syntax_builder::list(range, &lists));
            ast_blocks.extend(lists.into_iter().map(AstBlock::List));
            attach_pending_metadata(&mut blocks, &mut ast_blocks, &mut parser.pending_metadata);
            parser.mark_body_content();
            parser.cursor.commit(BlockConsumption::Through(next_line))?;
            continue;
        } else {
            parser.push_paragraph_line(line, content);
        }
        parser.cursor.commit(BlockConsumption::OneLine)?;
    }
    parser.flush_paragraph(&mut blocks, &mut ast_blocks, config, budget)?;
    parser.flush_orphan_metadata(&mut blocks, &mut ast_blocks, source, budget)?;
    let common = BlockFacts {
        syntax: blocks,
        blocks: ast_blocks,
        anchors,
    };
    Ok(match parser.document_header_phase {
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
    external_attributes: &BTreeMap<String, Option<String>>,
    cancellation: &dyn crate::core::CancellationCheck,
) -> Result<ParsedDocument, ParseFailure> {
    let BlockSequenceOutput::Root(sequence) = sequence else {
        return Err(ParseFailure::InternalInvariant);
    };
    let header_attribute_count = sequence
        .attributes
        .partition_point(|attribute| attribute.range.end() <= sequence.header.end);
    let mut checkpoint = crate::cancellation::CancellationCheckpoint::new(cancellation);
    let mut ast = crate::lowering::lower(
        crate::lowering::ParsedFacts {
            blocks: sequence.common.blocks,
            attributes: sequence.attributes,
            header_attribute_count,
            anchors: sequence.common.anchors,
            header: sequence.header,
            external_attributes,
            attribute_expansion_limits: crate::substitution::AttributeExpansionLimits {
                max_depth: config.limits.max_attribute_expansion_depth,
                max_bytes: config.limits.max_attribute_expansion_bytes,
            },
            processing_limits: config.limits,
        },
        &mut checkpoint,
    )
    .map_err(|error| match error {
        crate::lowering::LoweringFailure::Limit(error) => ParseFailure::Budget(BudgetExceeded {
            resource: error.resource,
            limit: error.limit,
            actual: error.actual,
        }),
        crate::lowering::LoweringFailure::Cancelled => ParseFailure::Cancelled,
    })?;
    let syntax_issues = crate::syntax_diagnostics::collect_and_clear_cancellable(
        &mut ast.blocks,
        &sequence.attribute_problems,
        &mut checkpoint,
    )
    .map_err(|()| ParseFailure::Cancelled)?;

    Ok(ParsedDocument {
        syntax: SyntaxTree::from_blocks_cancellable(
            source_document,
            sequence.common.syntax,
            syntax_issues,
            &mut checkpoint,
        )
        .map_err(|failure| match failure {
            crate::syntax::SyntaxTreeBuildFailure::Invariant(_) => ParseFailure::InternalInvariant,
            crate::syntax::SyntaxTreeBuildFailure::Cancelled => ParseFailure::Cancelled,
        })?,
        ast,
    })
}

fn body_attribute_has_blank_offset(
    document: &SourceDocument,
    line_index: usize,
    saw_content: bool,
) -> bool {
    if !saw_content {
        return false;
    }
    let mut preceding = line_index;
    while preceding > 0 {
        preceding -= 1;
        let line = document.lines()[preceding];
        let content = document
            .text(line.content_range())
            .expect("line content has valid UTF-8 boundaries");
        if content.trim_matches([' ', '\t']).is_empty() {
            return true;
        }
        if content.starts_with("//") {
            return true;
        }
        if crate::attributes::parse_line(
            content,
            line.content_range().start().to_usize(),
            line.full_range(),
        )
        .is_none()
        {
            return false;
        }
    }
    false
}

fn parse_block_title(
    content: &str,
    base: usize,
    config: &ParseConfig,
    budget: &mut ParseBudget,
) -> Result<Option<BlockTitle>, BudgetExceeded> {
    if !is_block_title(content) {
        return Ok(None);
    }
    let value = content.strip_prefix('.').expect("checked title prefix");
    let start = TextSize::new(base + 1).expect("recognized title offset");
    let end = TextSize::new(base + content.len()).expect("recognized title offset");
    let range = TextRange::new(start, end).expect("recognized title range");
    let parsed = parse_inlines(
        value,
        range,
        InlineParseConfig {
            max_depth: config.max_inline_depth,
            max_formula_bytes: config.max_formula_bytes,
        },
        budget,
    )?;
    Ok(Some(BlockTitle {
        value: value.to_owned(),
        range,
        inlines: split_hard_breaks(parsed.inlines),
        inline_problems: parsed.problems,
    }))
}

fn consume_metadata_budget(
    metadata: &BlockMetadata,
    budget: &mut ParseBudget,
) -> Result<(), BudgetExceeded> {
    for _ in 0..metadata_attribute_count(metadata) {
        budget.consume_attribute()?;
    }
    Ok(())
}

fn metadata_attribute_count(metadata: &BlockMetadata) -> usize {
    metadata.attributes.len()
        + metadata.roles.len()
        + metadata.options.len()
        + usize::from(metadata.id.is_some())
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
    let mut pending = std::mem::take(pending);
    let metadata_range = pending
        .semantic
        .range
        .expect("non-empty metadata has a range");
    let trailing_start = pending
        .syntax
        .iter()
        .position(|syntax| metadata_range.end() <= syntax.range().start())
        .unwrap_or(pending.syntax.len());
    let trailing_syntax = pending.syntax.split_off(trailing_start);
    let unknown = SyntaxNode::new(SyntaxKind::Unknown, metadata_range, pending.syntax);
    syntax_blocks.push(SyntaxNode::new(
        SyntaxKind::Unsupported,
        metadata_range,
        vec![unknown],
    ));
    ast_blocks.push(AstBlock::Unsupported(Unsupported {
        metadata: BlockMetadata::default(),
        range: metadata_range,
        raw: source[metadata_range.start().to_usize()..metadata_range.end().to_usize()]
            .trim_end_matches(['\r', '\n'])
            .to_owned(),
        reason: "block metadata is not attached to a block".to_owned(),
        kind: UnsupportedKind::Syntax,
    }));
    syntax_blocks.extend(trailing_syntax);
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

fn parse_math_block(
    source_document: &SourceDocument,
    attribute_index: usize,
    source: &str,
    config: &ParseConfig,
    end_line: usize,
) -> Result<(MathBlock, usize), PositionError> {
    let attribute = source_document.lines()[attribute_index];
    let attribute_text = source_document
        .text(attribute.content_range())
        .expect("valid");
    let language = parse_math_attribute(attribute_text).expect("recognized math attribute");
    let delimiter_index = attribute_index + 1;
    let delimiter = source_document.lines()[delimiter_index];
    let body = crate::delimiter::body(source_document, delimiter_index, "++++", source, end_line)?;
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

fn parse_lists(
    context: &DelimitedParseContext<'_>,
    start: usize,
    end_line: usize,
    state: &mut ParseState<'_>,
    parse_depth: ParseDepth,
) -> Result<(Vec<ListBlock>, usize, TextRange), ParseFailure> {
    let source_document = context.source_document;
    let config = context.config;
    let mut flat = Vec::new();
    let mut index = start;
    let mut previous: Option<(usize, ListKind)> = None;
    let mut kinds_by_depth = Vec::<Option<ListKind>>::new();
    while index < end_line {
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
            explicit_number,
            mut text_start,
            term_end,
            mut callout_id,
        } = marker;
        let (explicit_number, invalid_explicit_number) = explicit_number.public_fields();
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
        let principal_start = absolute + text_start;
        let mut principal_end = line.content_range().end().to_usize();
        let mut principal_end_line = index + 1;
        if matches!(kind, ListKind::Ordered | ListKind::Unordered) {
            while principal_end_line < end_line {
                if (context.is_cancelled)() {
                    return Err(ParseFailure::Cancelled);
                }
                let continuation_line = source_document.lines()[principal_end_line];
                let continuation_content = source_document
                    .text(continuation_line.content_range())
                    .expect("valid principal text line");
                if continuation_content == "+"
                    || crate::list_parser::marker(continuation_content).is_some()
                {
                    break;
                }
                let next_content = source_document
                    .lines()
                    .get(principal_end_line + 1)
                    .and_then(|next| source_document.text(next.content_range()));
                if !matches!(
                    crate::block_grammar::recognize_line(
                        continuation_content,
                        next_content,
                        continuation_line.content_range().start().to_usize(),
                        continuation_line.full_range(),
                        false,
                    ),
                    crate::block_grammar::LineRecognition::Paragraph
                        | crate::block_grammar::LineRecognition::LiteralParagraph
                ) {
                    break;
                }
                principal_end = continuation_line.content_range().end().to_usize();
                principal_end_line += 1;
            }
        }
        let text = context
            .source
            .get(principal_start..principal_end)
            .expect("principal text range is valid UTF-8");
        let item_text_range = text_range(principal_start, principal_end)?;
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
        if text.is_empty() && kind != ListKind::Description {
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
        if let Some((previous_depth, _)) = previous
            && effective_depth > previous_depth + 1
        {
            problems.push(ListProblem {
                kind: ListProblemKind::InvalidNesting,
                range: marker_range,
            });
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
            range: TextRange::new(
                line.full_range().start(),
                source_document.lines()[principal_end_line - 1]
                    .full_range()
                    .end(),
            )?,
            marker_range,
            explicit_number,
            invalid_explicit_number,
            separator_range,
            text_range: item_text_range,
            text: text.to_owned(),
            inlines: split_hard_breaks(parsed.inlines),
            terms,
            checklist,
            callout_id,
            inline_problems: parsed.problems,
            children: Vec::new(),
            continuations: Vec::new(),
            continuation_ranges: Vec::new(),
            problems,
        };
        index = principal_end_line;
        while source_document
            .lines()
            .get(index)
            .is_some_and(|next| source_document.text(next.content_range()) == Some("+"))
        {
            let continuation = source_document.lines()[index];
            state.budget.consume_list_continuation()?;
            let next = index + 1;
            let Some((attached, end)) =
                parse_list_continuation(context, next, end_line, state, parse_depth)?
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
    end_line: usize,
    state: &mut ParseState<'_>,
    depth: ParseDepth,
) -> Result<Option<(Vec<AstBlock>, usize)>, ParseFailure> {
    let source_document = context.source_document;
    let source = context.source;
    let config = context.config;
    if index >= end_line {
        return Ok(None);
    }
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
        let (mut block, end) = parse_source_block(source_document, index, source, end_line)?;
        block.metadata = parse_block_attributes(content, line.content_range().start().to_usize())
            .unwrap_or_default();
        return Ok(Some((vec![AstBlock::Source(block)], end)));
    }
    if let Some(spec) = crate::delimiter::spec(content) {
        let (block, nested_syntax, end) = parse_delimited_block(
            context,
            index,
            end_line,
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
        let (lists, end, _) = parse_lists(context, index, end_line, state, depth)?;
        return Ok(Some((lists.into_iter().map(AstBlock::List).collect(), end)));
    }
    let mut paragraph = Paragraph {
        metadata: BlockMetadata::default(),
        range: line.full_range(),
        content_range: line.content_range(),
        value: content.to_owned(),
        inlines: Vec::new(),
        admonition: None,
        inline_problems: Vec::new(),
    };
    resolve_paragraph_inlines(&mut paragraph, config, state.budget)?;
    Ok(Some((vec![AstBlock::Paragraph(paragraph)], index + 1)))
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
        if let Ok(id) = value[open + 1..close].parse::<u32>()
            && id != 0
        {
            output.push(CalloutMarker {
                id,
                range: text_range(
                    range.start().to_usize() + open,
                    range.start().to_usize() + close + 1,
                )?,
            });
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
            presentation: None,
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
    use crate::table::{TableCellContent, TableCellStyle};

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
    let maximum_columns = config.limits.max_table_columns as usize;
    let configuration = crate::table::ResolvedTableConfiguration::resolve(
        input.delimiter,
        input.delimiter_range,
        input.metadata,
        maximum_columns,
    )
    .map_err(|error| match error {
        crate::table::TableConfigurationError::ColumnCount(actual) => {
            reject("table columns", config.limits.max_table_columns, actual)
        }
        crate::table::TableConfigurationError::ColumnWidth(actual) => {
            reject("table column width", u32::MAX, actual)
        }
    })?;
    let column_styles = configuration.column_styles().collect::<Vec<_>>();
    let scanned = crate::table::scan_with_configuration(
        input.value,
        input.content_range,
        configuration.input(),
        &column_styles,
    );
    let cell_count = scanned.materialized_cell_count();
    if cell_count > u64::from(config.limits.max_table_cells) {
        return Err(reject(
            "table cells",
            config.limits.max_table_cells,
            cell_count,
        ));
    }
    let widest = scanned.inferred_columns as u64;
    if widest > u64::from(config.limits.max_table_columns) {
        return Err(reject(
            "table columns",
            config.limits.max_table_columns,
            widest,
        ));
    }
    if (context.is_cancelled)() {
        return Err(ParseFailure::Cancelled);
    }
    state.budget.consume_nodes(cell_count)?;
    let laid_out = configuration
        .configure(scanned, || {
            if (context.is_cancelled)() {
                Err(ParseFailure::Cancelled)
            } else {
                Ok(())
            }
        })?
        .layout();
    let mut nested_syntax = Vec::new();
    let table = laid_out.lower_content(
        |cell: &crate::table::ConfiguredCell| -> Result<TableCellContent, ParseFailure> {
            match cell.style {
                TableCellStyle::Literal | TableCellStyle::Verse => {
                    Ok(TableCellContent::Verbatim(cell.raw.clone()))
                }
                TableCellStyle::AsciiDoc => {
                    let fragment = if context.source_document.text(cell.content_range)
                        == Some(cell.raw.as_str())
                    {
                        SourceDocument::indexed_view(context.source_document, cell.content_range)?
                    } else {
                        SourceDocument::from_fragment_bounded(
                            Arc::from(cell.raw.as_str()),
                            cell.content_range.start(),
                            config.limits.max_line_bytes,
                            context.is_cancelled,
                        )
                        .map_err(|error| match error {
                            SourceDocumentBuildError::Position(error) => {
                                ParseFailure::Position(error)
                            }
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
                    Ok(TableCellContent::AsciiDoc(nested.blocks))
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
                    Ok(TableCellContent::Inlines(parsed.inlines))
                }
            }
        },
    )?;
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
    end_line: usize,
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
    let mut body =
        crate::delimiter::body(source_document, delimiter_index, "----", source, end_line)?;
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
        admonition: None,
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
    resolve_paragraph_inlines(&mut paragraph, config, budget)?;
    cst_blocks.push(crate::syntax_builder::paragraph(&paragraph));
    ast_blocks.push(AstBlock::Paragraph(paragraph));
    attach_pending_metadata(cst_blocks, ast_blocks, pending_metadata);
    Ok(())
}

fn resolve_paragraph_inlines(
    paragraph: &mut Paragraph,
    config: &ParseConfig,
    budget: &mut ParseBudget,
) -> Result<(), ParseFailure> {
    let admonition = admonition_paragraph(&paragraph.value);
    let body_offset = admonition.map_or(0, |(_, offset)| offset);
    if let Some((kind, _)) = admonition {
        paragraph.admonition = Some(AdmonitionPresentation {
            kind,
            label_range: TextRange::new(
                paragraph.content_range.start(),
                TextSize::new(paragraph.content_range.start().to_usize() + kind.label().len() + 1)
                    .expect("admonition label is in range"),
            )
            .expect("admonition label range is ordered"),
        });
    }
    let inline_output = parse_inlines(
        &paragraph.value[body_offset..],
        TextRange::new(
            TextSize::new(paragraph.content_range.start().to_usize() + body_offset)
                .expect("admonition body is in range"),
            paragraph.content_range.end(),
        )
        .expect("paragraph body range is ordered"),
        InlineParseConfig {
            max_depth: config.max_inline_depth,
            max_formula_bytes: config.max_formula_bytes,
        },
        budget,
    )?;
    paragraph.inlines = split_hard_breaks(inline_output.inlines);
    paragraph.inline_problems = inline_output.problems;
    Ok(())
}

fn admonition_paragraph(value: &str) -> Option<(AdmonitionKind, usize)> {
    let (label, body) = value.split_once(':')?;
    let kind = AdmonitionKind::parse(label)?;
    let whitespace = body.len() - body.trim_start_matches([' ', '\t']).len();
    (whitespace > 0 && !body[whitespace..].trim().is_empty())
        .then_some((kind, label.len() + 1 + whitespace))
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

fn text_range(start: usize, end: usize) -> Result<TextRange, PositionError> {
    TextRange::new(
        crate::source::TextSize::new(start)?,
        crate::source::TextSize::new(end)?,
    )
}

#[cfg(test)]
mod tests;
