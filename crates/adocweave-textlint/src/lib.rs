//! Pure Rust conversion from AdocWeave's semantic model to a typed TxtAST plan.
//!
//! The plan contains only source-backed text. A JavaScript adapter may add
//! `raw`, `value`, and `loc` without deciding which AsciiDoc constructs are
//! prose.

use std::collections::{BTreeMap, BTreeSet};

use adocweave::Analysis;
use adocweave::semantic::{
    Block, BlockMetadata, DelimitedBlockKind, DelimitedContent, HeadingKind, Inline, InlineStyle,
    ListBlock, ListItem, ListKind, MacroAttribute, StandardMacro, StandardMacroKind, Table,
    TableCellContent, VerbatimKind, is_plain_inline_text,
};
use adocweave::text::{SyntaxKind, TextRange, TextSize};
use serde::Serialize;

/// Limits applied while constructing a plan.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PlanLimits {
    /// Maximum number of TxtAST nodes, including the document root.
    pub max_nodes: usize,
}

impl Default for PlanLimits {
    fn default() -> Self {
        Self {
            max_nodes: 1_000_000,
        }
    }
}

/// A stable failure produced by the pure planning backend.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PlanError {
    NodeLimitExceeded { max_nodes: usize },
    InvalidSourceRange,
    OverlappingSiblings,
    InvalidNodeHierarchy,
}

impl std::fmt::Display for PlanError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NodeLimitExceeded { max_nodes } => {
                write!(formatter, "TxtAST plan exceeds the {max_nodes} node limit")
            }
            Self::InvalidSourceRange => formatter.write_str("semantic model has an invalid range"),
            Self::OverlappingSiblings => {
                formatter.write_str("TxtAST plan contains overlapping siblings")
            }
            Self::InvalidNodeHierarchy => {
                formatter.write_str("TxtAST plan contains an invalid parent-child relationship")
            }
        }
    }
}

impl std::error::Error for PlanError {}

/// A UTF-16 half-open range used directly by JavaScript and TxtAST.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub struct Utf16Range(pub u32, pub u32);

/// A complete, typed TxtAST plan.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct TxtAstPlan {
    #[serde(rename = "type")]
    pub node_type: DocumentType,
    pub range: Utf16Range,
    pub children: Vec<TxtAstNode>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
pub enum DocumentType {
    Document,
}

/// TxtAST nodes emitted by this backend.
///
/// Each variant carries exactly the properties required by that node type.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(tag = "type")]
pub enum TxtAstNode {
    Header {
        range: Utf16Range,
        depth: u8,
        children: Vec<TxtAstNode>,
    },
    Paragraph {
        range: Utf16Range,
        children: Vec<TxtAstNode>,
    },
    List {
        range: Utf16Range,
        ordered: bool,
        children: Vec<TxtAstNode>,
    },
    ListItem {
        range: Utf16Range,
        children: Vec<TxtAstNode>,
    },
    BlockQuote {
        range: Utf16Range,
        children: Vec<TxtAstNode>,
    },
    Table {
        range: Utf16Range,
        children: Vec<TxtAstNode>,
    },
    TableRow {
        range: Utf16Range,
        children: Vec<TxtAstNode>,
    },
    TableCell {
        range: Utf16Range,
        children: Vec<TxtAstNode>,
    },
    CodeBlock {
        range: Utf16Range,
        #[serde(rename = "valueRange")]
        value_range: Utf16Range,
        lang: Option<String>,
    },
    Comment {
        range: Utf16Range,
        #[serde(rename = "valueRange")]
        value_range: Utf16Range,
    },
    Str {
        range: Utf16Range,
        #[serde(rename = "valueRange")]
        value_range: Utf16Range,
    },
    Code {
        range: Utf16Range,
        #[serde(rename = "valueRange")]
        value_range: Utf16Range,
    },
    Strong {
        range: Utf16Range,
        children: Vec<TxtAstNode>,
    },
    Emphasis {
        range: Utf16Range,
        children: Vec<TxtAstNode>,
    },
    Link {
        range: Utf16Range,
        url: String,
        children: Vec<TxtAstNode>,
    },
    Break {
        range: Utf16Range,
    },
}

/// Builds a TxtAST plan from one immutable analysis snapshot.
pub fn plan(analysis: &Analysis, limits: PlanLimits) -> Result<TxtAstPlan, PlanError> {
    Builder::new(analysis, limits).build()
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ByteRange(TextRange);

impl ByteRange {
    fn new(start: usize, end: usize) -> Result<Self, PlanError> {
        Ok(Self(
            TextRange::new(
                TextSize::new(start).map_err(|_| PlanError::InvalidSourceRange)?,
                TextSize::new(end).map_err(|_| PlanError::InvalidSourceRange)?,
            )
            .map_err(|_| PlanError::InvalidSourceRange)?,
        ))
    }

    const fn start(self) -> usize {
        self.0.start().to_usize()
    }

    const fn end(self) -> usize {
        self.0.end().to_usize()
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum ByteNode {
    Header {
        range: ByteRange,
        depth: u8,
        children: Vec<Self>,
    },
    Paragraph {
        range: ByteRange,
        children: Vec<Self>,
    },
    List {
        range: ByteRange,
        ordered: bool,
        children: Vec<Self>,
    },
    ListItem {
        range: ByteRange,
        children: Vec<Self>,
    },
    BlockQuote {
        range: ByteRange,
        children: Vec<Self>,
    },
    Table {
        range: ByteRange,
        children: Vec<Self>,
    },
    TableRow {
        range: ByteRange,
        children: Vec<Self>,
    },
    TableCell {
        range: ByteRange,
        children: Vec<Self>,
    },
    CodeBlock {
        range: ByteRange,
        value_range: ByteRange,
        lang: Option<String>,
    },
    Comment {
        range: ByteRange,
        value_range: ByteRange,
    },
    Str {
        range: ByteRange,
        value_range: ByteRange,
    },
    Code {
        range: ByteRange,
        value_range: ByteRange,
    },
    Strong {
        range: ByteRange,
        children: Vec<Self>,
    },
    Emphasis {
        range: ByteRange,
        children: Vec<Self>,
    },
    Link {
        range: ByteRange,
        url: String,
        children: Vec<Self>,
    },
    Break {
        range: ByteRange,
    },
}

impl ByteNode {
    const fn range(&self) -> ByteRange {
        match self {
            Self::Header { range, .. }
            | Self::Paragraph { range, .. }
            | Self::List { range, .. }
            | Self::ListItem { range, .. }
            | Self::BlockQuote { range, .. }
            | Self::Table { range, .. }
            | Self::TableRow { range, .. }
            | Self::TableCell { range, .. }
            | Self::CodeBlock { range, .. }
            | Self::Comment { range, .. }
            | Self::Str { range, .. }
            | Self::Code { range, .. }
            | Self::Strong { range, .. }
            | Self::Emphasis { range, .. }
            | Self::Link { range, .. }
            | Self::Break { range } => *range,
        }
    }

    fn children_mut(&mut self) -> Option<&mut Vec<Self>> {
        match self {
            Self::Header { children, .. }
            | Self::Paragraph { children, .. }
            | Self::List { children, .. }
            | Self::ListItem { children, .. }
            | Self::BlockQuote { children, .. }
            | Self::Table { children, .. }
            | Self::TableRow { children, .. }
            | Self::TableCell { children, .. }
            | Self::Strong { children, .. }
            | Self::Emphasis { children, .. }
            | Self::Link { children, .. } => Some(children),
            Self::CodeBlock { .. }
            | Self::Comment { .. }
            | Self::Str { .. }
            | Self::Code { .. }
            | Self::Break { .. } => None,
        }
    }

    fn accepts_comment_block(&self) -> bool {
        matches!(
            self,
            Self::BlockQuote { .. } | Self::ListItem { .. } | Self::TableCell { .. }
        )
    }
}

struct Budget {
    used: usize,
    max: usize,
}

impl Budget {
    fn claim(&mut self) -> Result<(), PlanError> {
        if self.used >= self.max {
            return Err(PlanError::NodeLimitExceeded {
                max_nodes: self.max,
            });
        }
        self.used += 1;
        Ok(())
    }
}

struct Builder<'analysis> {
    analysis: &'analysis Analysis,
    source: &'analysis str,
    budget: Budget,
}

impl<'analysis> Builder<'analysis> {
    fn new(analysis: &'analysis Analysis, limits: PlanLimits) -> Self {
        Self {
            analysis,
            source: analysis.source(),
            budget: Budget {
                used: 0,
                max: limits.max_nodes,
            },
        }
    }

    fn build(mut self) -> Result<TxtAstPlan, PlanError> {
        self.budget.claim()?;
        let root_range = ByteRange(self.analysis.syntax().root().range());
        let mut children = Vec::new();
        for block in self.analysis.document().blocks() {
            children.extend(self.block(block)?);
        }
        sort_and_check(&mut children)?;
        self.insert_line_comments(&mut children)?;
        sort_and_check(&mut children)?;
        validate_hierarchy(ParentType::Document, &children)?;
        let offsets = Utf16Offsets::new(self.source, root_range, &children)?;
        Ok(TxtAstPlan {
            node_type: DocumentType::Document,
            range: offsets.range(root_range)?,
            children: children
                .into_iter()
                .map(|node| offsets.node(node))
                .collect::<Result<_, _>>()?,
        })
    }

    fn block(&mut self, block: &Block) -> Result<Vec<ByteNode>, PlanError> {
        let mut output = self.block_title(block.metadata())?;
        match block {
            Block::Heading(heading) => {
                let depth = match heading.kind {
                    HeadingKind::DocumentTitle | HeadingKind::Part => 1,
                    HeadingKind::Section { level } | HeadingKind::Discrete { level } => level,
                }
                .clamp(1, 6);
                output.extend(self.headers(heading.range, depth, &heading.inlines)?);
            }
            Block::Paragraph(paragraph) => {
                output.extend(self.paragraphs(paragraph.range, &paragraph.inlines)?)
            }
            Block::LiteralParagraph(literal) => {
                self.budget.claim()?;
                output.push(ByteNode::CodeBlock {
                    range: ByteRange(literal.range),
                    value_range: ByteRange(literal.content_range),
                    lang: None,
                });
            }
            Block::Break(_) | Block::Math(_) | Block::Unsupported(_) => {}
            Block::Source(source) => {
                self.budget.claim()?;
                output.push(ByteNode::CodeBlock {
                    range: ByteRange(source.range),
                    value_range: ByteRange(source.content_range),
                    lang: source.language.clone(),
                });
            }
            Block::Verbatim(verbatim) => {
                self.budget.claim()?;
                output.push(ByteNode::CodeBlock {
                    range: ByteRange(verbatim.range),
                    value_range: ByteRange(verbatim.content_range),
                    lang: match &verbatim.kind {
                        VerbatimKind::Source(source) => source.language.clone(),
                        VerbatimKind::Listing | VerbatimKind::Literal => None,
                    },
                });
            }
            Block::List(list) => output.push(self.list(list)?),
            Block::Delimited(block) => match block.kind {
                DelimitedBlockKind::Comment => {
                    self.budget.claim()?;
                    output.push(ByteNode::Paragraph {
                        range: ByteRange(block.range),
                        children: vec![
                            self.comment(ByteRange(block.range), ByteRange(block.content_range))?,
                        ],
                    });
                }
                DelimitedBlockKind::Listing | DelimitedBlockKind::Literal => {
                    self.budget.claim()?;
                    output.push(ByteNode::CodeBlock {
                        range: ByteRange(block.range),
                        value_range: ByteRange(block.content_range),
                        lang: None,
                    });
                }
                DelimitedBlockKind::Pass => {}
                DelimitedBlockKind::Quote => {
                    let mut children = self.compound(&block.content)?;
                    sort_and_check(&mut children)?;
                    self.budget.claim()?;
                    output.push(ByteNode::BlockQuote {
                        range: ByteRange(block.range),
                        children,
                    });
                }
                DelimitedBlockKind::Table => {
                    if let DelimitedContent::Table(table) = &block.content {
                        output.push(self.table(ByteRange(block.range), table)?);
                    }
                }
                DelimitedBlockKind::Example
                | DelimitedBlockKind::Open
                | DelimitedBlockKind::Sidebar => {
                    output.extend(self.compound(&block.content)?);
                }
            },
        }
        sort_and_check(&mut output)?;
        Ok(output)
    }

    fn compound(&mut self, content: &DelimitedContent) -> Result<Vec<ByteNode>, PlanError> {
        let DelimitedContent::Compound(blocks) = content else {
            return Ok(Vec::new());
        };
        let mut output: Vec<ByteNode> = Vec::new();
        for block in blocks {
            output.extend(self.block(block)?);
        }
        sort_and_check(&mut output)?;
        Ok(output)
    }

    fn block_title(&mut self, metadata: &BlockMetadata) -> Result<Vec<ByteNode>, PlanError> {
        let Some(title) = &metadata.title else {
            return Ok(Vec::new());
        };
        self.headers(title.range, 1, &title.inlines)
    }

    fn headers(
        &mut self,
        full_range: TextRange,
        depth: u8,
        inlines: &[Inline],
    ) -> Result<Vec<ByteNode>, PlanError> {
        let children = self.inline_nodes(inlines)?;
        self.budget.claim()?;
        Ok(vec![ByteNode::Header {
            range: ByteRange(full_range),
            depth,
            children,
        }])
    }

    fn paragraphs(
        &mut self,
        full_range: TextRange,
        inlines: &[Inline],
    ) -> Result<Vec<ByteNode>, PlanError> {
        let children = self.inline_nodes(inlines)?;
        if children.is_empty() {
            return Ok(Vec::new());
        }
        self.budget.claim()?;
        Ok(vec![ByteNode::Paragraph {
            range: ByteRange(full_range),
            children,
        }])
    }

    fn list(&mut self, list: &ListBlock) -> Result<ByteNode, PlanError> {
        let mut children = Vec::new();
        for item in &list.items {
            children.push(self.list_item(item)?);
        }
        sort_and_check(&mut children)?;
        self.budget.claim()?;
        Ok(ByteNode::List {
            range: ByteRange(list.range),
            ordered: matches!(list.kind, ListKind::Ordered),
            children,
        })
    }

    fn list_item(&mut self, item: &ListItem) -> Result<ByteNode, PlanError> {
        let mut children = Vec::new();
        for term in &item.terms {
            children.extend(self.paragraphs(term.range, &term.inlines)?);
        }
        children.extend(self.paragraphs(item.text_range, &item.inlines)?);
        for list in &item.children {
            children.push(self.list(list)?);
        }
        for block in &item.continuations {
            children.extend(self.block(block)?);
        }
        sort_and_check(&mut children)?;
        self.budget.claim()?;
        Ok(ByteNode::ListItem {
            range: ByteRange(item.range),
            children,
        })
    }

    fn table(&mut self, range: ByteRange, table: &Table) -> Result<ByteNode, PlanError> {
        let mut rows = Vec::new();
        for row in &table.rows {
            let mut cells = Vec::new();
            for cell in &row.cells {
                let children = match &cell.content {
                    TableCellContent::Inlines(inlines) => self.inline_nodes(inlines)?,
                    TableCellContent::AsciiDoc(blocks) => {
                        let mut runs = Vec::new();
                        self.table_cell_block_runs(blocks, &mut runs)?;
                        self.join_cell_runs(runs)?
                    }
                    TableCellContent::Verbatim(_) => Vec::new(),
                };
                self.budget.claim()?;
                cells.push(ByteNode::TableCell {
                    range: ByteRange(cell.range),
                    children,
                });
            }
            sort_and_check(&mut cells)?;
            self.budget.claim()?;
            rows.push(ByteNode::TableRow {
                range: ByteRange(row.range),
                children: cells,
            });
        }
        sort_and_check(&mut rows)?;
        self.budget.claim()?;
        Ok(ByteNode::Table {
            range,
            children: rows,
        })
    }

    fn table_cell_block_runs(
        &mut self,
        blocks: &[Block],
        output: &mut Vec<Vec<ByteNode>>,
    ) -> Result<(), PlanError> {
        for block in blocks {
            if let Some(title) = &block.metadata().title {
                output.push(self.inline_nodes(&title.inlines)?);
            }
            match block {
                Block::Heading(heading) => output.push(self.inline_nodes(&heading.inlines)?),
                Block::Paragraph(paragraph) => {
                    output.push(self.inline_nodes(&paragraph.inlines)?);
                }
                Block::List(list) => self.table_cell_list_runs(list, output)?,
                Block::Delimited(delimited) => {
                    if delimited.kind == DelimitedBlockKind::Comment {
                        output.push(vec![self.comment(
                            ByteRange(delimited.range),
                            ByteRange(delimited.content_range),
                        )?]);
                    } else if let DelimitedContent::Compound(blocks) = &delimited.content {
                        self.table_cell_block_runs(blocks, output)?;
                    }
                }
                Block::LiteralParagraph(_)
                | Block::Break(_)
                | Block::Source(_)
                | Block::Verbatim(_)
                | Block::Math(_)
                | Block::Unsupported(_) => {}
            }
        }
        Ok(())
    }

    fn table_cell_list_runs(
        &mut self,
        list: &ListBlock,
        output: &mut Vec<Vec<ByteNode>>,
    ) -> Result<(), PlanError> {
        for item in &list.items {
            for term in &item.terms {
                output.push(self.inline_nodes(&term.inlines)?);
            }
            output.push(self.inline_nodes(&item.inlines)?);
            for child in &item.children {
                self.table_cell_list_runs(child, output)?;
            }
            self.table_cell_block_runs(&item.continuations, output)?;
        }
        Ok(())
    }

    fn join_cell_runs(&mut self, runs: Vec<Vec<ByteNode>>) -> Result<Vec<ByteNode>, PlanError> {
        let mut output: Vec<ByteNode> = Vec::new();
        for run in runs {
            if let (Some(previous), Some(next)) = (output.last(), run.first())
                && let Some(separator) =
                    newline_range(self.source, previous.range().end(), next.range().start())?
            {
                output.push(self.str_node(separator)?);
            }
            output.extend(run);
        }
        Ok(output)
    }

    fn inline_nodes(&mut self, inlines: &[Inline]) -> Result<Vec<ByteNode>, PlanError> {
        let mut output = Vec::new();
        for inline in inlines {
            output.extend(self.inline(inline)?);
        }
        Ok(output)
    }

    fn inline(&mut self, inline: &Inline) -> Result<Vec<ByteNode>, PlanError> {
        match inline {
            Inline::Text(text) => Ok(vec![self.str_node(ByteRange(text.range))?]),
            Inline::Literal {
                range,
                content_range,
                ..
            } => {
                self.budget.claim()?;
                Ok(vec![ByteNode::Code {
                    range: ByteRange(*range),
                    value_range: ByteRange(*content_range),
                }])
            }
            Inline::Styled {
                style,
                range,
                children,
                ..
            } => {
                let children = self.inline_nodes(children)?;
                match style {
                    InlineStyle::Strong => {
                        self.budget.claim()?;
                        Ok(vec![ByteNode::Strong {
                            range: ByteRange(*range),
                            children,
                        }])
                    }
                    InlineStyle::Emphasis => {
                        self.budget.claim()?;
                        Ok(vec![ByteNode::Emphasis {
                            range: ByteRange(*range),
                            children,
                        }])
                    }
                    InlineStyle::Highlight
                    | InlineStyle::Subscript
                    | InlineStyle::Superscript
                    | InlineStyle::CurvedDoubleQuote
                    | InlineStyle::CurvedSingleQuote => Ok(children),
                }
            }
            Inline::AttributeReference { range, .. } => self.opaque_inline(*range, *range),
            Inline::Formula(formula) => self.opaque_inline(formula.range, formula.content_range),
            Inline::Passthrough {
                range,
                content_range,
                ..
            } => self.opaque_inline(*range, *content_range),
            Inline::HardBreak { range } => {
                self.budget.claim()?;
                Ok(vec![ByteNode::Break {
                    range: ByteRange(*range),
                }])
            }
            Inline::Link(link) => {
                if link.label_range.is_none_or(TextRange::is_empty) {
                    self.opaque_inline(link.range, link.range)
                } else {
                    self.link(link.range, link.target.clone(), &link.label)
                }
            }
            Inline::Reference(reference) => {
                if reference.label_range.is_none_or(TextRange::is_empty) {
                    self.opaque_inline(reference.range, reference.range)
                } else {
                    self.link(
                        reference.range,
                        reference.expanded_target.clone(),
                        &reference.label,
                    )
                }
            }
            Inline::Macro(macro_node) => self.macro_pieces(macro_node),
        }
    }

    fn link(
        &mut self,
        range: TextRange,
        url: String,
        label: &[Inline],
    ) -> Result<Vec<ByteNode>, PlanError> {
        let children = self.inline_nodes(label)?;
        if children.is_empty() {
            return self.opaque_inline(range, range);
        }
        self.budget.claim()?;
        Ok(vec![ByteNode::Link {
            range: ByteRange(range),
            url,
            children,
        }])
    }

    fn macro_pieces(&mut self, node: &StandardMacro) -> Result<Vec<ByteNode>, PlanError> {
        use StandardMacroKind as Kind;
        let first = node.attributes.first();
        let named = |name: &str| {
            node.attributes.iter().find(|attribute| {
                attribute
                    .name
                    .as_deref()
                    .is_some_and(|candidate| candidate.eq_ignore_ascii_case(name))
            })
        };
        let text = match node.kind {
            Kind::Footnote => MacroText::Prose(first.into_iter().collect()),
            Kind::Keyboard => MacroText::UiToken(
                node.attributes
                    .iter()
                    .map(|value| value.value_range)
                    .collect(),
            ),
            Kind::Button => {
                MacroText::UiToken(first.into_iter().map(|value| value.value_range).collect())
            }
            Kind::Menu => MacroText::UiToken(
                std::iter::once(node.target_range)
                    .chain(node.attributes.iter().map(|value| value.value_range))
                    .collect(),
            ),
            Kind::Image => MacroText::Prose(
                named("alt")
                    .or_else(|| node.attributes.iter().find(|value| value.name.is_none()))
                    .into_iter()
                    .collect(),
            ),
            Kind::Icon => MacroText::Prose(
                named("alt")
                    .or_else(|| named("title"))
                    .into_iter()
                    .collect(),
            ),
            Kind::Audio | Kind::Video => MacroText::Prose(named("title").into_iter().collect()),
            Kind::Email
            | Kind::Anchor
            | Kind::BibliographyAnchor
            | Kind::Citation
            | Kind::IndexTerm => MacroText::Opaque,
        };
        match text {
            MacroText::Prose(attributes)
                if !attributes.is_empty()
                    && attributes
                        .iter()
                        .all(|attribute| plain_macro_prose(self.source, attribute)) =>
            {
                attributes
                    .into_iter()
                    .map(|attribute| self.str_node(ByteRange(attribute.value_range)))
                    .collect()
            }
            MacroText::UiToken(ranges) if !ranges.is_empty() => ranges
                .into_iter()
                .map(|range| {
                    self.budget.claim()?;
                    Ok(ByteNode::Code {
                        range: ByteRange(range),
                        value_range: ByteRange(range),
                    })
                })
                .collect(),
            MacroText::Prose(_) | MacroText::UiToken(_) | MacroText::Opaque => {
                self.opaque_inline(node.range, node.range)
            }
        }
    }

    fn opaque_inline(
        &mut self,
        range: TextRange,
        value_range: TextRange,
    ) -> Result<Vec<ByteNode>, PlanError> {
        self.budget.claim()?;
        Ok(vec![ByteNode::Code {
            range: ByteRange(range),
            value_range: ByteRange(value_range),
        }])
    }

    fn str_node(&mut self, range: ByteRange) -> Result<ByteNode, PlanError> {
        self.budget.claim()?;
        Ok(ByteNode::Str {
            range,
            value_range: range,
        })
    }

    fn comment(&mut self, range: ByteRange, value_range: ByteRange) -> Result<ByteNode, PlanError> {
        self.budget.claim()?;
        Ok(ByteNode::Comment { range, value_range })
    }

    fn insert_line_comments(&mut self, children: &mut Vec<ByteNode>) -> Result<(), PlanError> {
        let mut comments = self
            .analysis
            .syntax()
            .nodes(SyntaxKind::CommentLine)
            .map(|node| node.range())
            .collect::<Vec<_>>();
        comments.sort_by_key(|range| (range.start(), range.end()));
        let mut planned = Vec::with_capacity(comments.len());
        for range in comments {
            let range = ByteRange(range);
            if range_in_comment_container(children, range) {
                continue;
            }
            let Some(value_range) = line_comment_value_range(self.source, range)? else {
                continue;
            };
            // Every accepted source comment produces at least the Comment leaf.
            // Its Paragraph wrapper is charged after the destination type is known.
            self.budget.claim()?;
            planned.push(ByteNode::Paragraph {
                range,
                children: vec![ByteNode::Comment { range, value_range }],
            });
        }
        let planned_count = planned.len();
        let mut removed_wrappers = 0;
        let comments = merge_comments(children, planned, &mut removed_wrappers);
        merge_sorted(children, comments);
        for _ in 0..planned_count.saturating_sub(removed_wrappers) {
            self.budget.claim()?;
        }
        Ok(())
    }
}

enum MacroText<'attribute> {
    Prose(Vec<&'attribute MacroAttribute>),
    UiToken(Vec<TextRange>),
    Opaque,
}

fn plain_macro_prose(source: &str, attribute: &MacroAttribute) -> bool {
    let range = ByteRange(attribute.value_range);
    let Some(value) = source.get(range.start()..range.end()) else {
        return false;
    };
    value == attribute.value && is_plain_inline_text(value)
}

fn sort_and_check(nodes: &mut [ByteNode]) -> Result<(), PlanError> {
    nodes.sort_by_key(|node| (node.range().start(), node.range().end()));
    let mut previous_end = None;
    for node in nodes {
        let node_range = node.range();
        if previous_end.is_some_and(|end| end > node_range.start()) {
            return Err(PlanError::OverlappingSiblings);
        }
        if let Some(children) = node.children_mut() {
            sort_and_check(children)?;
            for child in children {
                if child.range().start() < node_range.start()
                    || child.range().end() > node_range.end()
                {
                    return Err(PlanError::InvalidSourceRange);
                }
            }
        }
        previous_end = Some(node_range.end());
    }
    Ok(())
}

#[derive(Clone, Copy)]
enum ParentType {
    Document,
    Header,
    Paragraph,
    List,
    ListItem,
    BlockQuote,
    Table,
    TableRow,
    TableCell,
    Strong,
    Emphasis,
    Link,
}

fn validate_hierarchy(parent: ParentType, nodes: &[ByteNode]) -> Result<(), PlanError> {
    for node in nodes {
        let allowed = match parent {
            ParentType::Document | ParentType::ListItem | ParentType::BlockQuote => matches!(
                node,
                ByteNode::Header { .. }
                    | ByteNode::Paragraph { .. }
                    | ByteNode::List { .. }
                    | ByteNode::BlockQuote { .. }
                    | ByteNode::Table { .. }
                    | ByteNode::CodeBlock { .. }
            ),
            ParentType::Header | ParentType::Strong | ParentType::Emphasis | ParentType::Link => {
                matches!(
                    node,
                    ByteNode::Str { .. }
                        | ByteNode::Code { .. }
                        | ByteNode::Strong { .. }
                        | ByteNode::Emphasis { .. }
                        | ByteNode::Link { .. }
                        | ByteNode::Break { .. }
                )
            }
            ParentType::Paragraph => matches!(
                node,
                ByteNode::Comment { .. }
                    | ByteNode::Str { .. }
                    | ByteNode::Code { .. }
                    | ByteNode::Strong { .. }
                    | ByteNode::Emphasis { .. }
                    | ByteNode::Link { .. }
                    | ByteNode::Break { .. }
            ),
            ParentType::List => matches!(node, ByteNode::ListItem { .. }),
            ParentType::Table => matches!(node, ByteNode::TableRow { .. }),
            ParentType::TableRow => matches!(node, ByteNode::TableCell { .. }),
            ParentType::TableCell => matches!(
                node,
                ByteNode::Comment { .. }
                    | ByteNode::Str { .. }
                    | ByteNode::Code { .. }
                    | ByteNode::Strong { .. }
                    | ByteNode::Emphasis { .. }
                    | ByteNode::Link { .. }
                    | ByteNode::Break { .. }
            ),
        };
        if !allowed {
            return Err(PlanError::InvalidNodeHierarchy);
        }
        let (child_parent, children) = match node {
            ByteNode::Header { children, .. } => (Some(ParentType::Header), children.as_slice()),
            ByteNode::Paragraph { children, .. } => {
                (Some(ParentType::Paragraph), children.as_slice())
            }
            ByteNode::List { children, .. } => (Some(ParentType::List), children.as_slice()),
            ByteNode::ListItem { children, .. } => {
                (Some(ParentType::ListItem), children.as_slice())
            }
            ByteNode::BlockQuote { children, .. } => {
                (Some(ParentType::BlockQuote), children.as_slice())
            }
            ByteNode::Table { children, .. } => (Some(ParentType::Table), children.as_slice()),
            ByteNode::TableRow { children, .. } => {
                (Some(ParentType::TableRow), children.as_slice())
            }
            ByteNode::TableCell { children, .. } => {
                (Some(ParentType::TableCell), children.as_slice())
            }
            ByteNode::Strong { children, .. } => (Some(ParentType::Strong), children.as_slice()),
            ByteNode::Emphasis { children, .. } => {
                (Some(ParentType::Emphasis), children.as_slice())
            }
            ByteNode::Link { children, .. } => (Some(ParentType::Link), children.as_slice()),
            ByteNode::CodeBlock { .. }
            | ByteNode::Comment { .. }
            | ByteNode::Str { .. }
            | ByteNode::Code { .. }
            | ByteNode::Break { .. } => (None, &[] as &[ByteNode]),
        };
        if let Some(child_parent) = child_parent {
            validate_hierarchy(child_parent, children)?;
        }
    }
    Ok(())
}

fn merge_comments(
    nodes: &mut [ByteNode],
    comments: Vec<ByteNode>,
    removed_wrappers: &mut usize,
) -> Vec<ByteNode> {
    let mut by_node = (0..nodes.len()).map(|_| Vec::new()).collect::<Vec<_>>();
    let mut at_scope = Vec::new();
    let mut cursor = 0;
    for comment in comments {
        let range = comment.range();
        while cursor < nodes.len() && nodes[cursor].range().end() <= range.start() {
            cursor += 1;
        }
        if cursor < nodes.len()
            && nodes[cursor].range().start() <= range.start()
            && range.end() <= nodes[cursor].range().end()
        {
            by_node[cursor].push(comment);
        } else {
            at_scope.push(comment);
        }
    }

    let mut routed_at_scope = Vec::new();
    for (node, contained) in nodes.iter_mut().zip(by_node) {
        if contained.is_empty() || is_comment_container(node) {
            continue;
        }
        let accepts_comment = node.accepts_comment_block();
        let is_table_cell = matches!(node, ByteNode::TableCell { .. });
        if let Some(children) = node.children_mut() {
            let mut remaining = merge_comments(children, contained, removed_wrappers);
            if accepts_comment && is_table_cell {
                for node in &mut remaining {
                    let ByteNode::Paragraph { children, .. } = node else {
                        continue;
                    };
                    if children.len() == 1 && matches!(children[0], ByteNode::Comment { .. }) {
                        *node = children.remove(0);
                        *removed_wrappers = removed_wrappers.saturating_add(1);
                    }
                }
                merge_sorted(children, remaining);
            } else if accepts_comment {
                merge_sorted(children, remaining);
            } else {
                routed_at_scope.append(&mut remaining);
            }
        } else {
            routed_at_scope.extend(contained);
        }
    }
    merge_sorted_values(at_scope, routed_at_scope)
}

fn merge_sorted(nodes: &mut Vec<ByteNode>, additions: Vec<ByteNode>) {
    if additions.is_empty() {
        return;
    }
    *nodes = merge_sorted_values(std::mem::take(nodes), additions);
}

fn merge_sorted_values(left: Vec<ByteNode>, right: Vec<ByteNode>) -> Vec<ByteNode> {
    let capacity = left.len().saturating_add(right.len());
    let mut left = left.into_iter().peekable();
    let mut right = right.into_iter().peekable();
    let mut output = Vec::with_capacity(capacity);
    while left.peek().is_some() || right.peek().is_some() {
        let take_left = match (left.peek(), right.peek()) {
            (Some(left), Some(right)) => {
                (left.range().start(), left.range().end())
                    <= (right.range().start(), right.range().end())
            }
            (Some(_), None) => true,
            (None, Some(_)) => false,
            (None, None) => break,
        };
        output.push(if take_left {
            left.next().expect("peeked left node")
        } else {
            right.next().expect("peeked right node")
        });
    }
    output
}

fn is_comment_container(node: &ByteNode) -> bool {
    matches!(node, ByteNode::Comment { .. })
        || matches!(
            node,
            ByteNode::Paragraph { children, .. }
                if children.len() == 1 && matches!(children[0], ByteNode::Comment { .. })
        )
}

fn range_in_comment_container(nodes: &[ByteNode], range: ByteRange) -> bool {
    let candidate = nodes.partition_point(|node| node.range().end() <= range.start());
    let Some(node) = nodes
        .get(candidate)
        .filter(|node| node.range().start() <= range.start() && range.end() <= node.range().end())
    else {
        return false;
    };
    if is_comment_container(node) {
        return true;
    }
    match node {
        ByteNode::Header { children, .. }
        | ByteNode::Paragraph { children, .. }
        | ByteNode::List { children, .. }
        | ByteNode::ListItem { children, .. }
        | ByteNode::BlockQuote { children, .. }
        | ByteNode::Table { children, .. }
        | ByteNode::TableRow { children, .. }
        | ByteNode::TableCell { children, .. }
        | ByteNode::Strong { children, .. }
        | ByteNode::Emphasis { children, .. }
        | ByteNode::Link { children, .. } => range_in_comment_container(children, range),
        ByteNode::CodeBlock { .. }
        | ByteNode::Comment { .. }
        | ByteNode::Str { .. }
        | ByteNode::Code { .. }
        | ByteNode::Break { .. } => false,
    }
}

fn line_comment_value_range(
    source: &str,
    range: ByteRange,
) -> Result<Option<ByteRange>, PlanError> {
    let raw = source
        .get(range.start()..range.end())
        .ok_or(PlanError::InvalidSourceRange)?;
    let Some(prefix) = raw.find("//").map(|offset| offset + 2) else {
        return Ok(None);
    };
    let leading = raw[prefix..]
        .char_indices()
        .find(|(_, character)| !character.is_whitespace())
        .map_or(raw.len(), |(offset, _)| prefix + offset);
    let trailing = raw
        .trim_end_matches(['\r', '\n', '\u{2028}', '\u{2029}'])
        .len();
    Ok(Some(ByteRange::new(
        range.start() + leading.min(trailing),
        range.start() + trailing,
    )?))
}

fn newline_range(source: &str, start: usize, end: usize) -> Result<Option<ByteRange>, PlanError> {
    let gap = source
        .get(start..end)
        .ok_or(PlanError::InvalidSourceRange)?;
    for (offset, character) in gap.char_indices() {
        let length = match character {
            '\r' if gap[offset..].starts_with("\r\n") => 2,
            '\r' | '\n' => 1,
            '\u{2028}' | '\u{2029}' => character.len_utf8(),
            _ => continue,
        };
        return Ok(Some(ByteRange::new(
            start + offset,
            start + offset + length,
        )?));
    }
    Ok(None)
}

struct Utf16Offsets {
    values: BTreeMap<usize, u32>,
}

impl Utf16Offsets {
    fn new(source: &str, root: ByteRange, nodes: &[ByteNode]) -> Result<Self, PlanError> {
        let mut needed = BTreeSet::from([root.start(), root.end()]);
        for node in nodes {
            collect_offsets(node, &mut needed);
        }
        if needed.last().is_some_and(|offset| *offset > source.len()) {
            return Err(PlanError::InvalidSourceRange);
        }
        let mut values = BTreeMap::new();
        let mut utf16 = 0usize;
        let mut needed = needed.into_iter().peekable();
        if needed.peek() == Some(&0) {
            values.insert(0, 0);
            needed.next();
        }
        for (byte, character) in source.char_indices() {
            while needed.peek().is_some_and(|offset| *offset == byte) {
                values.insert(
                    byte,
                    u32::try_from(utf16).map_err(|_| PlanError::InvalidSourceRange)?,
                );
                needed.next();
            }
            utf16 += character.len_utf16();
            let end = byte + character.len_utf8();
            while needed.peek().is_some_and(|offset| *offset == end) {
                values.insert(
                    end,
                    u32::try_from(utf16).map_err(|_| PlanError::InvalidSourceRange)?,
                );
                needed.next();
            }
        }
        if needed.next().is_some() {
            return Err(PlanError::InvalidSourceRange);
        }
        Ok(Self { values })
    }

    fn range(&self, range: ByteRange) -> Result<Utf16Range, PlanError> {
        Ok(Utf16Range(
            *self
                .values
                .get(&range.start())
                .ok_or(PlanError::InvalidSourceRange)?,
            *self
                .values
                .get(&range.end())
                .ok_or(PlanError::InvalidSourceRange)?,
        ))
    }

    fn node(&self, node: ByteNode) -> Result<TxtAstNode, PlanError> {
        let children = |nodes: Vec<ByteNode>| {
            nodes
                .into_iter()
                .map(|node| self.node(node))
                .collect::<Result<Vec<_>, _>>()
        };
        Ok(match node {
            ByteNode::Header {
                range,
                depth,
                children: value,
            } => TxtAstNode::Header {
                range: self.range(range)?,
                depth,
                children: children(value)?,
            },
            ByteNode::Paragraph {
                range,
                children: value,
            } => TxtAstNode::Paragraph {
                range: self.range(range)?,
                children: children(value)?,
            },
            ByteNode::List {
                range,
                ordered,
                children: value,
            } => TxtAstNode::List {
                range: self.range(range)?,
                ordered,
                children: children(value)?,
            },
            ByteNode::ListItem {
                range,
                children: value,
            } => TxtAstNode::ListItem {
                range: self.range(range)?,
                children: children(value)?,
            },
            ByteNode::BlockQuote {
                range,
                children: value,
            } => TxtAstNode::BlockQuote {
                range: self.range(range)?,
                children: children(value)?,
            },
            ByteNode::Table {
                range,
                children: value,
            } => TxtAstNode::Table {
                range: self.range(range)?,
                children: children(value)?,
            },
            ByteNode::TableRow {
                range,
                children: value,
            } => TxtAstNode::TableRow {
                range: self.range(range)?,
                children: children(value)?,
            },
            ByteNode::TableCell {
                range,
                children: value,
            } => TxtAstNode::TableCell {
                range: self.range(range)?,
                children: children(value)?,
            },
            ByteNode::CodeBlock {
                range,
                value_range,
                lang,
            } => TxtAstNode::CodeBlock {
                range: self.range(range)?,
                value_range: self.range(value_range)?,
                lang,
            },
            ByteNode::Comment { range, value_range } => TxtAstNode::Comment {
                range: self.range(range)?,
                value_range: self.range(value_range)?,
            },
            ByteNode::Str { range, value_range } => TxtAstNode::Str {
                range: self.range(range)?,
                value_range: self.range(value_range)?,
            },
            ByteNode::Code { range, value_range } => TxtAstNode::Code {
                range: self.range(range)?,
                value_range: self.range(value_range)?,
            },
            ByteNode::Strong {
                range,
                children: value,
            } => TxtAstNode::Strong {
                range: self.range(range)?,
                children: children(value)?,
            },
            ByteNode::Emphasis {
                range,
                children: value,
            } => TxtAstNode::Emphasis {
                range: self.range(range)?,
                children: children(value)?,
            },
            ByteNode::Link {
                range,
                url,
                children: value,
            } => TxtAstNode::Link {
                range: self.range(range)?,
                url,
                children: children(value)?,
            },
            ByteNode::Break { range } => TxtAstNode::Break {
                range: self.range(range)?,
            },
        })
    }
}

fn collect_offsets(node: &ByteNode, output: &mut BTreeSet<usize>) {
    let range = node.range();
    output.extend([range.start(), range.end()]);
    match node {
        ByteNode::CodeBlock { value_range, .. }
        | ByteNode::Comment { value_range, .. }
        | ByteNode::Str { value_range, .. }
        | ByteNode::Code { value_range, .. } => {
            output.extend([value_range.start(), value_range.end()]);
        }
        _ => {}
    }
    if let Some(children) = match node {
        ByteNode::Header { children, .. }
        | ByteNode::Paragraph { children, .. }
        | ByteNode::List { children, .. }
        | ByteNode::ListItem { children, .. }
        | ByteNode::BlockQuote { children, .. }
        | ByteNode::Table { children, .. }
        | ByteNode::TableRow { children, .. }
        | ByteNode::TableCell { children, .. }
        | ByteNode::Strong { children, .. }
        | ByteNode::Emphasis { children, .. }
        | ByteNode::Link { children, .. } => Some(children),
        _ => None,
    } {
        for child in children {
            collect_offsets(child, output);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use adocweave::{AnalysisOptions, Engine};

    fn build(source: &str) -> TxtAstPlan {
        let analysis = Engine::new(AnalysisOptions::default())
            .analyze(source)
            .expect("analysis");
        plan(&analysis, PlanLimits::default()).expect("TxtAST plan")
    }

    fn children(node: &TxtAstNode) -> &[TxtAstNode] {
        match node {
            TxtAstNode::Header { children, .. }
            | TxtAstNode::Paragraph { children, .. }
            | TxtAstNode::List { children, .. }
            | TxtAstNode::ListItem { children, .. }
            | TxtAstNode::BlockQuote { children, .. }
            | TxtAstNode::Table { children, .. }
            | TxtAstNode::TableRow { children, .. }
            | TxtAstNode::TableCell { children, .. }
            | TxtAstNode::Strong { children, .. }
            | TxtAstNode::Emphasis { children, .. }
            | TxtAstNode::Link { children, .. } => children,
            TxtAstNode::CodeBlock { .. }
            | TxtAstNode::Comment { .. }
            | TxtAstNode::Str { .. }
            | TxtAstNode::Code { .. }
            | TxtAstNode::Break { .. } => &[],
        }
    }

    fn node_range(node: &TxtAstNode) -> Utf16Range {
        match node {
            TxtAstNode::Header { range, .. }
            | TxtAstNode::Paragraph { range, .. }
            | TxtAstNode::List { range, .. }
            | TxtAstNode::ListItem { range, .. }
            | TxtAstNode::BlockQuote { range, .. }
            | TxtAstNode::Table { range, .. }
            | TxtAstNode::TableRow { range, .. }
            | TxtAstNode::TableCell { range, .. }
            | TxtAstNode::CodeBlock { range, .. }
            | TxtAstNode::Comment { range, .. }
            | TxtAstNode::Str { range, .. }
            | TxtAstNode::Code { range, .. }
            | TxtAstNode::Strong { range, .. }
            | TxtAstNode::Emphasis { range, .. }
            | TxtAstNode::Link { range, .. }
            | TxtAstNode::Break { range } => *range,
        }
    }

    fn visit<'a>(nodes: &'a [TxtAstNode], output: &mut Vec<&'a TxtAstNode>) {
        for node in nodes {
            output.push(node);
            visit(children(node), output);
        }
    }

    fn byte_at_utf16(source: &str, wanted: u32) -> usize {
        if wanted == 0 {
            return 0;
        }
        let mut offset = 0u32;
        for (byte, character) in source.char_indices() {
            offset += character.len_utf16() as u32;
            if offset == wanted {
                return byte + character.len_utf8();
            }
        }
        assert_eq!(offset, wanted);
        source.len()
    }

    fn text_at(source: &str, range: Utf16Range) -> &str {
        &source[byte_at_utf16(source, range.0)..byte_at_utf16(source, range.1)]
    }

    #[test]
    fn block_variant_matrix_builds_typed_nodes() {
        let source = "= 見出し\n\n段落です。\n\n literal\n\n'''\n\n[source,rust]\n----\nfn main() {}\n----\n\n....\nliteral\n....\n\n* 箇条書き\n\n. 番号付き\n\n用語:: 説明\n\n<1> callout\n\n[stem]\n++++\nx\n++++\n\n////\ncomment\n////\n\n====\nexample\n====\n\n--\nopen\n--\n\n****\nsidebar\n****\n\n++++\npass\n++++\n\n____\nquote\n____\n\n|===\n|cell\n|===\n\ninclude::missing.adoc[]\n";
        let output = build(source);
        let mut all = Vec::new();
        visit(&output.children, &mut all);
        assert!(
            all.iter()
                .any(|node| matches!(node, TxtAstNode::Header { .. }))
        );
        assert!(
            all.iter()
                .any(|node| matches!(node, TxtAstNode::CodeBlock { .. }))
        );
        assert!(
            all.iter()
                .any(|node| matches!(node, TxtAstNode::List { ordered: true, .. }))
        );
        assert!(
            all.iter()
                .any(|node| matches!(node, TxtAstNode::List { ordered: false, .. }))
        );
        assert!(
            all.iter()
                .any(|node| matches!(node, TxtAstNode::BlockQuote { .. }))
        );
        assert!(
            all.iter()
                .any(|node| matches!(node, TxtAstNode::Table { .. }))
        );
        assert!(
            all.iter()
                .any(|node| matches!(node, TxtAstNode::Comment { .. }))
        );
    }

    #[test]
    fn block_titles_are_non_overlapping_siblings_and_only_quotes_are_block_quotes() {
        let source = ".段落題\n段落\n\n.一覧題\n* 項目\n\n.コード題\n[source,rust]\n----\ncode\n----\n\n.表題\n|===\n|セル\n|===\n\n.引用題\n____\n引用\n____\n\n.例題\n====\n説明\n====\n\n.開放題\n--\n開放\n--\n\n.サイドバー題\n****\n補足\n****\n";
        let plan = build(source);
        let mut previous = 0;
        for node in &plan.children {
            let range = node_range(node);
            assert!(previous <= range.0, "overlap at {node:?}");
            previous = range.1;
        }
        let mut all = Vec::new();
        visit(&plan.children, &mut all);
        assert_eq!(
            all.iter()
                .filter(|node| matches!(node, TxtAstNode::BlockQuote { .. }))
                .count(),
            1
        );
        assert!(plan.children.windows(2).any(|pair| matches!(
            pair,
            [TxtAstNode::Header { depth: 1, .. }, TxtAstNode::List { .. }]
        )));
    }

    #[test]
    fn inline_variant_matrix_preserves_types_and_marks_opaque_nodes_as_code() {
        let source = "前 **強調** _斜体_ #印# ^上^ ~下~ `code` {name} stem:[x] pass:[raw] link:https://example.com[表示] <<id,参照>> +\n後\n";
        let plan = build(source);
        let mut all = Vec::new();
        visit(&plan.children, &mut all);
        assert!(
            all.iter()
                .any(|node| matches!(node, TxtAstNode::Strong { .. }))
        );
        assert!(
            all.iter()
                .any(|node| matches!(node, TxtAstNode::Emphasis { .. }))
        );
        assert!(
            all.iter()
                .any(|node| matches!(node, TxtAstNode::Link { .. }))
        );
        assert!(
            all.iter()
                .any(|node| matches!(node, TxtAstNode::Break { .. }))
        );
        assert_eq!(
            plan.children
                .iter()
                .filter(|node| matches!(node, TxtAstNode::Paragraph { .. }))
                .count(),
            1,
            "opaque inline constructs must preserve their paragraph"
        );
        assert!(
            all.iter()
                .filter(|node| matches!(node, TxtAstNode::Code { .. }))
                .count()
                >= 4
        );
    }

    #[test]
    fn inline_code_is_non_prose_and_preserves_paragraph_context() {
        let source = "前 `サーバー` 後。";
        let output = build(source);
        let [TxtAstNode::Paragraph { children, .. }] = output.children.as_slice() else {
            panic!(
                "inline code must stay in one paragraph: {:?}",
                output.children
            );
        };
        assert!(matches!(
            children.as_slice(),
            [
                TxtAstNode::Str { .. },
                TxtAstNode::Code { .. },
                TxtAstNode::Str { .. }
            ]
        ));
        let code = children
            .iter()
            .find_map(|node| match node {
                TxtAstNode::Code { value_range, .. } => Some(text_at(source, *value_range)),
                _ => None,
            })
            .expect("inline code");
        assert_eq!(code, "サーバー");
        assert!(children.iter().all(|node| match node {
            TxtAstNode::Str { value_range, .. } => text_at(source, *value_range) != "サーバー",
            _ => true,
        }));

        for source in [
            "C compiler（以下、``stdenv.cc``とします。）",
            "基準である``path``とします。",
        ] {
            let output = build(source);
            let [TxtAstNode::Paragraph { children, .. }] = output.children.as_slice() else {
                panic!("code context was split: {source}");
            };
            assert!(
                children
                    .iter()
                    .any(|node| matches!(node, TxtAstNode::Code { .. }))
            );
        }
    }

    #[test]
    fn standard_macro_policy_exposes_only_visible_natural_language() {
        let source = "footnote:[脚注本文] footnote:[C++ APIです。]\n\nkbd:[Ctrl,Shift,T]\n\nbtn:[保存]\n\nmenu:File[Open,Recent]\n\nimage:pic.png[代替文]\n\nicon:save[title=保存アイコン]\n\naudio:sound.mp3[title=音声題]\n\nvideo:movie.mp4[title=動画題]\n\naudio:sound.mp3[] video:movie.mp4[] icon:save[]\n\nbtn:[{label}] image:pic.png[{alt}] footnote:[**強調**です。]\n\nfootnote:[https://example.com/path] footnote:[user@example.com] image:pic.png[https://example.com] audio:x.mp3[title=https://example.com]\n\nuser@example.com [[anchor,非表示]] cite:[key] indexterm:[索引]\n";
        let plan = build(source);
        let mut all = Vec::new();
        visit(&plan.children, &mut all);
        let visible = all
            .iter()
            .filter_map(|node| match node {
                TxtAstNode::Str { value_range, .. } => Some(text_at(source, *value_range)),
                _ => None,
            })
            .collect::<Vec<_>>();
        for expected in [
            "脚注本文",
            "C++ APIです。",
            "代替文",
            "保存アイコン",
            "音声題",
            "動画題",
        ] {
            assert!(
                visible.contains(&expected),
                "missing visible macro text: {expected}"
            );
        }
        assert!(
            !visible
                .iter()
                .any(|value| value.contains("user@example.com"))
        );
        assert!(!visible.contains(&"非表示"));
        assert!(!visible.contains(&"key"));
        assert!(!visible.contains(&"索引"));
        for excluded in [
            "sound.mp3",
            "movie.mp4",
            "save",
            "{label}",
            "{alt}",
            "**強調**です。",
            "https://example.com/path",
            "user@example.com",
            "https://example.com",
        ] {
            assert!(!visible.contains(&excluded), "unexpected prose: {excluded}");
        }
        let ui_tokens = all
            .iter()
            .filter_map(|node| match node {
                TxtAstNode::Code { value_range, .. } => Some(text_at(source, *value_range)),
                _ => None,
            })
            .collect::<Vec<_>>();
        for expected in ["Ctrl", "Shift", "T", "保存", "File", "Open", "Recent"] {
            assert!(
                ui_tokens.contains(&expected),
                "missing UI token: {expected}"
            );
        }
    }

    #[test]
    fn visible_macro_text_preserves_surrounding_prose_run() {
        let source = "本文です。 footnote:[注釈です。]\n";
        let plan = build(source);
        assert_eq!(plan.children.len(), 1);
        let TxtAstNode::Paragraph { children, .. } = &plan.children[0] else {
            panic!(
                "本文と脚注が単一のParagraphではありません: {:?}",
                plan.children
            );
        };
        assert_eq!(children.len(), 2);
        assert!(
            children
                .iter()
                .all(|node| matches!(node, TxtAstNode::Str { .. }))
        );
    }

    #[test]
    fn empty_link_labels_are_non_prose_without_splitting_the_paragraph() {
        let source = "前 link:https://example.com[] 後";
        let plan = build(source);
        let mut all = Vec::new();
        visit(&plan.children, &mut all);
        assert!(
            !all.iter()
                .any(|node| matches!(node, TxtAstNode::Link { .. })),
            "{all:?}"
        );
        let [TxtAstNode::Paragraph { children, .. }] = plan.children.as_slice() else {
            panic!("empty link label split the paragraph: {:?}", plan.children);
        };
        assert!(
            children
                .iter()
                .any(|node| matches!(node, TxtAstNode::Code { .. }))
        );
    }

    #[test]
    fn opaque_inlines_preserve_one_heading_and_one_table_cell() {
        let source = "== 前 {unknown} 後\n\n|===\n|前{unknown}後\n|===\n";
        let plan = build(source);
        let mut all = Vec::new();
        visit(&plan.children, &mut all);
        assert_eq!(
            all.iter()
                .filter(|node| matches!(node, TxtAstNode::Header { .. }))
                .count(),
            1
        );
        assert_eq!(
            all.iter()
                .filter(|node| matches!(node, TxtAstNode::TableCell { .. }))
                .count(),
            1
        );
        assert!(
            all.iter()
                .filter(|node| matches!(node, TxtAstNode::Code { .. }))
                .count()
                >= 2
        );
    }

    #[test]
    fn asciidoc_table_cells_contain_only_phrasing_nodes() {
        fn is_phrasing(node: &TxtAstNode) -> bool {
            matches!(
                node,
                TxtAstNode::Comment { .. }
                    | TxtAstNode::Str { .. }
                    | TxtAstNode::Code { .. }
                    | TxtAstNode::Strong { .. }
                    | TxtAstNode::Emphasis { .. }
                    | TxtAstNode::Link { .. }
                    | TxtAstNode::Break { .. }
            )
        }

        let source =
            "|===\na|最初の段落\n\n// textlint-disable\n\n* 一つ\n* 二つ\n\n最後の段落\n|===\n";
        let plan = build(source);
        let mut all = Vec::new();
        visit(&plan.children, &mut all);
        let cell = all
            .iter()
            .find_map(|node| match node {
                TxtAstNode::TableCell { children, .. } => Some(children),
                _ => None,
            })
            .expect("AsciiDoc table cell");
        assert!(!cell.is_empty());
        assert!(cell.iter().all(is_phrasing));
        assert_eq!(
            cell.iter()
                .filter(|node| matches!(node, TxtAstNode::Comment { .. }))
                .count(),
            1
        );
        assert!(cell.iter().any(|node| match node {
            TxtAstNode::Str { value_range, .. } => {
                matches!(
                    text_at(source, *value_range),
                    "\n" | "\r" | "\r\n" | "\u{2028}" | "\u{2029}"
                )
            }
            _ => false,
        }));
    }

    #[test]
    fn delimited_comments_are_not_duplicated_as_line_comments() {
        let source = "////\n// inner line\nblock comment\n////\n";
        let output = build(source);
        let mut all = Vec::new();
        visit(&output.children, &mut all);
        assert_eq!(
            all.iter()
                .filter(|node| matches!(node, TxtAstNode::Comment { .. }))
                .count(),
            1
        );

        let mut large = String::from("////\n");
        for index in 0..10_000 {
            large.push_str(&format!("// interior {index}\n"));
        }
        large.push_str("////\n");
        let analysis = Engine::new(AnalysisOptions::default())
            .analyze(&large)
            .expect("analysis");
        plan(&analysis, PlanLimits { max_nodes: 3 })
            .expect("interior lines do not consume additional plan nodes");
        assert_eq!(
            plan(&analysis, PlanLimits { max_nodes: 2 }),
            Err(PlanError::NodeLimitExceeded { max_nodes: 2 })
        );
    }

    #[test]
    fn utf16_offsets_cover_all_textlint_line_terminators() {
        let source = "😀a\r\nb\rc\nd\u{2028}e\u{2029}終";
        let root = ByteRange::new(0, source.len()).expect("root");
        let endpoints = [
            0,
            "😀".len(),
            "😀a\r\n".len(),
            "😀a\r\nb\r".len(),
            "😀a\r\nb\rc\n".len(),
            "😀a\r\nb\rc\nd\u{2028}".len(),
            "😀a\r\nb\rc\nd\u{2028}e\u{2029}".len(),
            source.len(),
        ];
        let nodes = endpoints
            .windows(2)
            .map(|pair| ByteNode::Str {
                range: ByteRange::new(pair[0], pair[1]).expect("range"),
                value_range: ByteRange::new(pair[0], pair[1]).expect("range"),
            })
            .collect::<Vec<_>>();
        let offsets = Utf16Offsets::new(source, root, &nodes).expect("UTF-16 offsets");
        for endpoint in endpoints {
            let range = ByteRange::new(0, endpoint).expect("range");
            assert_eq!(
                offsets.range(range).expect("mapped").1,
                source[..endpoint].encode_utf16().count() as u32
            );
        }
    }

    #[test]
    fn comments_remain_in_source_order_inside_nested_quotes() {
        let mut source = String::from("____\n");
        for index in 0..2_000 {
            source.push_str(&format!("// directive-{index}\n\n段落 {index}\n\n"));
        }
        source.push_str("____\n");
        let plan = build(&source);
        let quote = plan
            .children
            .iter()
            .find(|node| matches!(node, TxtAstNode::BlockQuote { .. }))
            .expect("quote");
        let mut all = Vec::new();
        visit(std::slice::from_ref(quote), &mut all);
        let comments = all
            .iter()
            .filter_map(|node| match node {
                TxtAstNode::Comment { value_range, .. } => Some(*value_range),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(comments.len(), 2_000);
        assert!(comments.windows(2).all(|pair| pair[0].0 < pair[1].0));
    }

    #[test]
    fn node_budget_is_applied_during_construction() {
        let analysis = Engine::new(AnalysisOptions::default())
            .analyze("本文")
            .expect("analysis");
        assert_eq!(
            plan(&analysis, PlanLimits { max_nodes: 0 }),
            Err(PlanError::NodeLimitExceeded { max_nodes: 0 })
        );
        assert_eq!(
            plan(&analysis, PlanLimits { max_nodes: 1 }),
            Err(PlanError::NodeLimitExceeded { max_nodes: 1 })
        );
    }

    #[test]
    fn hierarchy_validator_covers_every_parent_type() {
        let range = ByteRange::new(0, 0).expect("range");
        let invalid_block = || ByteNode::CodeBlock {
            range,
            value_range: range,
            lang: None,
        };
        let invalid_phrasing = || ByteNode::Str {
            range,
            value_range: range,
        };
        for (parent, child) in [
            (ParentType::Document, invalid_phrasing()),
            (ParentType::Header, invalid_block()),
            (ParentType::Paragraph, invalid_block()),
            (ParentType::List, invalid_phrasing()),
            (ParentType::ListItem, invalid_phrasing()),
            (ParentType::BlockQuote, invalid_phrasing()),
            (ParentType::Table, invalid_phrasing()),
            (ParentType::TableRow, invalid_phrasing()),
            (ParentType::TableCell, invalid_block()),
            (ParentType::Strong, invalid_block()),
            (ParentType::Emphasis, invalid_block()),
            (ParentType::Link, invalid_block()),
        ] {
            assert_eq!(
                validate_hierarchy(parent, &[child]),
                Err(PlanError::InvalidNodeHierarchy)
            );
        }
    }
}
